use std::io::{self, BufRead, Write};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use rusqlite::{params, Connection};

/// Global watcher handle set on MCP `initialize` (Arc 30 Phase 4).
static WATCHER_HANDLE: OnceLock<crate::watcher::WatcherHandle> = OnceLock::new();

/// P1-2: Cached CWD — `std::env::current_dir()` is a syscall. The CWD doesn't
/// change within an MCP server lifetime, so compute once and reuse.
static CACHED_CWD: OnceLock<std::path::PathBuf> = OnceLock::new();

fn cached_cwd() -> &'static std::path::PathBuf {
    CACHED_CWD.get_or_init(|| {
        std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."))
    })
}

/// P1-3: Cached env vars — `std::env::var()` is a linear env-block scan that
/// allocates a String. Read once at first use, then reuse the cached bool/usize.
static NO_TRUNCATE: OnceLock<bool> = OnceLock::new();
static TRUNCATE_LIMIT: OnceLock<usize> = OnceLock::new();

fn is_no_truncate() -> bool {
    *NO_TRUNCATE.get_or_init(|| {
        std::env::var("RELIARY_NO_TRUNCATE").is_ok_and(|v| v == "1")
    })
}

fn truncate_limit() -> usize {
    *TRUNCATE_LIMIT.get_or_init(|| {
        std::env::var("RELIARY_TRUNCATE_LIMIT")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(4000)
    })
}

/// P1-6: Cached DB path string. The DB path doesn't change within an MCP
/// server lifetime. Computing it via `format!()` and `.to_string_lossy()` per
/// call is wasteful. Cache the canonical PathBuf at first use.
static CACHED_DB_PATH: OnceLock<std::path::PathBuf> = OnceLock::new();

fn cached_db_path() -> &'static std::path::PathBuf {
    CACHED_DB_PATH.get_or_init(|| {
        cached_cwd().join(".reliary").join("index.sqlite")
    })
}

// V54: thread-local cached SQLite connection for the CWD project. The stdio
// MCP server is single-threaded (serve_stdio loop), so a thread_local keeps
// ONE connection alive across all tool calls. Reusing it:
//   - keeps `prepare_cached` statement caches warm (no re-parse per call)
//   - avoids the ~20-50ms open + PRAGMA + schema-verify per call
//   - correctly shares the same WAL-backed index file
thread_local! {
    static CACHED_CONN: std::cell::RefCell<Option<rusqlite::Connection>> = const { std::cell::RefCell::new(None) };
}

// V56: session-level tool-result cache. Tools are deterministic (V26
// verified: same query + same index = byte-identical output), so identical
// (tool, args) pairs always produce the same result. Cache the serialized
// text; on repeat calls return a one-line "cached:" reply instead of
// re-billing the full result. Cache-safe: only modifies the FUTURE result,
// never prior turns (KV cache intact).
thread_local! {
    static RESULT_CACHE: std::cell::RefCell<Option<std::collections::HashMap<u64, String>>> =
        const { std::cell::RefCell::new(None) };
}

const RESULT_CACHE_CAP: usize = 256;

/// Deterministic cache key for (tool_name, canonical_args).
/// serde_json::Map is BTreeMap-backed, so to_string is key-sorted → canonical.
/// V58b: cache key = (tool, args) + per-phrase generation for the queried
/// symbol. Per-phrase gens let cross-query repeats hit while still
/// invalidating when THAT symbol's occurrence rows change.
fn result_cache_key(name: &str, args: &serde_json::Map<String, serde_json::Value>, db: Option<&rusqlite::Connection>) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    use std::hash::{Hash, Hasher};
    name.hash(&mut hasher);
    serde_json::to_string(args).unwrap_or_default().hash(&mut hasher);
    // M4: the index stamp is part of the key, so an external reindex
    // (gate.js -> `reliary reindex-file`) changes every key and stale
    // cached results can never be served after an edit.
    index_stamp(db).hash(&mut hasher);
    if let Some(db) = db {
        if let Some(sym) = args.get("name").and_then(|v| v.as_str()) {
            if let Ok(Some(pid)) = reliary_search::symbol::phrase_id_for(db, sym) {
                reliary_search::lazy_occurrence::phrase_generation(pid).hash(&mut hasher);
            }
        }
    }
    hasher.finish()
}

/// Look up a cached result. Returns the full cached text on hit.
fn result_cache_get(key: u64) -> Option<String> {
    RESULT_CACHE.with(|c| c.borrow().as_ref().and_then(|m| m.get(&key).cloned()))
}

/// M4: deterministic index freshness stamp — the persisted reindex generation
/// from the meta table, as 8 hex chars. Bumped by `reindex-file` (and thus by
/// gate.js after every edit); stable across JIT builds and reads. Appended to
/// every tool response so an agent can tell whether the data is current.
/// V73: takes the caller's connection — the previous version called
/// get_cached_db() while the caller held it checked out, opening a SECOND
/// connection on every call and defeating the V54 warm-connection cache.
fn index_stamp(db: Option<&rusqlite::Connection>) -> String {
    let gen = db.map(reliary_search::schema::index_gen).unwrap_or(0);
    format!("{:08x}", (gen as u64 & 0xffff_ffff) as u32)
}

/// M4: append the freshness stamp to the first text content item.
/// V73: takes the caller's connection so it reuses the warm one.
fn stamp_result(result: &mut DispatchResult, db: Option<&rusqlite::Connection>) {
    if let DispatchResult::Success(ref mut json) = result {
        if let Some(arr) = json.get_mut("content").and_then(|c| c.as_array_mut()) {
            if let Some(item) = arr.first_mut() {
                if let Some(t) = item.get("text").and_then(|t| t.as_str()) {
                    let stamped = format!("{}\n[idx:{}]", t, index_stamp(db));
                    item["text"] = serde_json::Value::String(stamped);
                }
            }
        }
    }
}
/// Store a result text in the cache (clear-all at cap — simple and fine at
/// this scale; sessions rarely exceed a few hundred distinct calls).
fn result_cache_put(key: u64, text: String) {
    RESULT_CACHE.with(|c| {
        let mut opt = c.borrow_mut();
        if opt.is_none() {
            *opt = Some(std::collections::HashMap::new());
        }
        if let Some(map) = opt.as_mut() {
            if map.len() >= RESULT_CACHE_CAP {
                map.clear();
            }
            map.insert(key, text);
        }
    });
}

/// One-line compact form of a cached result: the answer line (V40 format
/// puts the answer first), prefixed with "cached:".
#[allow(dead_code)]
fn compact_cached(text: &str) -> String {
    match text.lines().next() {
        Some(line) if !line.is_empty() => format!("cached: {}", line),
        _ => format!("cached: {}", text),
    }
}

/// Get the shared connection for the CWD project. Opens + verifies on first
/// use, reuses afterwards within this thread. Returns None if no index.
pub fn get_cached_db() -> Option<rusqlite::Connection> {
    // Take the cached connection out (borrow_mut ends immediately).
    let cached: Option<rusqlite::Connection> = CACHED_CONN.with(|c| c.borrow_mut().take());
    if let Some(conn) = cached {
        // Reuse without re-running PRAGMAs — journal_mode=WAL blocks while
        // another connection holds the DB (e.g. auto_trust's handle), and
        // re-running it per call deadlocks the stdio loop.
        return Some(conn);
    }
    let db_path = cached_db_path().clone();
    let db = match rusqlite::Connection::open(&db_path) {
        Ok(db) => db,
        Err(_) => return None,
    };
    let _ = db.execute_batch("PRAGMA synchronous=NORMAL;");
    if reliary_search::schema::open_existing_db_safe(&db).is_err() {
        return None;
    }
    Some(db)
}

/// Hand the connection back to the thread-local cache after a tool call.
/// The caller MUST do this when done, so the next call reuses the warm
/// connection (and its prepare_cached statement cache).
pub fn return_cached_db(conn: rusqlite::Connection) {
    CACHED_CONN.with(|c| *c.borrow_mut() = Some(conn));
}

/// V73: RAII guard that returns a checked-out cached connection to the
/// thread-local slot on drop. Every `open_symbol_index` call site used to
/// drop the connection (closing it), silently defeating the V54 warm cache.
/// With this guard, early returns and `?` can't leak it either.
pub struct CachedDbGuard(Option<rusqlite::Connection>);

impl CachedDbGuard {
    pub fn conn(&self) -> &rusqlite::Connection {
        self.0.as_ref().expect("guard holds a connection")
    }
}

impl std::ops::Deref for CachedDbGuard {
    type Target = rusqlite::Connection;
    fn deref(&self) -> &rusqlite::Connection {
        self.conn()
    }
}

impl Drop for CachedDbGuard {
    fn drop(&mut self) {
        if let Some(c) = self.0.take() {
            return_cached_db(c);
        }
    }
}

/// Bug 76-78: canonicalize an agent-provided path relative to a workdir.
/// Returns an error if the path escapes the workdir. For non-existent paths
/// (write/create tools), canonicalizes the parent directory.
fn safe_path(agent_path: &str, workdir: &str) -> Result<PathBuf, String> {
    let candidate = Path::new(workdir).join(agent_path);
    let canonical = match candidate.canonicalize() {
        Ok(p) => p,
        Err(_) => {
            // Path doesn't exist yet (write/create). Canonicalize parent dir instead.
            let parent = candidate.parent().ok_or_else(|| "invalid path: no parent".to_string())?;
            let canonical_parent = parent.canonicalize().map_err(|e| format!("invalid path: {}", e))?;
            let filename = candidate.file_name().ok_or_else(|| "invalid path: no filename".to_string())?;
            canonical_parent.join(filename)
        }
    };
    let wd = Path::new(workdir).canonicalize().map_err(|e| format!("invalid workdir: {}", e))?;
    if !canonical.starts_with(&wd) {
        return Err("path escapes workdir".into());
    }
    Ok(canonical)
}

/// Arc 49: make a file path relative to the current working directory.
/// Reduces tool output size by ~50% (absolute → relative paths).
fn relpath_with(file_path: &str, workdir: &str) -> String {
    if !Path::new(file_path).is_absolute() {
        return file_path.to_string();
    }
    let prefix = format!("{}/", workdir.trim_end_matches('/'));
    file_path.strip_prefix(&prefix)
        .map(|s| s.to_string())
        .unwrap_or_else(|| file_path.to_string())
}

#[allow(dead_code)]
fn relpath(file_path: &str) -> String {
    let workdir = cached_cwd().to_string_lossy().to_string();
    relpath_with(file_path, &workdir)
}

/// V24: Build a hit JSON object with qualified_name + context (3+3 lines).
fn hit_with_qualified_name(
    file_path: &str,
    line: i32,
    col: i32,
    workdir: &str,
) -> serde_json::Value {
    let qname = if let Some(meta) = reliary_search::file_meta::get(file_path) {
        reliary_search::qualified::derive_qualified_name(file_path, line, &meta)
    } else {
        String::new()
    };

    let context: Vec<String> = if let Some(meta) = reliary_search::file_meta::get(file_path) {
        let start = (line as usize).saturating_sub(3);
        let end = (line as usize + 4).min(meta.lines.len());
        if start < end {
            meta.lines[start..end].iter()
                .enumerate()
                .map(|(i, l)| {
                    // Compact: replace runs of whitespace with single space, strip newlines.
                    let compact: String = l.split_whitespace().collect::<Vec<_>>().join(" ");
                    format!("{}|{}", start + i + 1, compact)
                })
                .collect()
        } else {
            vec![]
        }
    } else {
        vec![]
    };

    serde_json::json!({
        "file": relpath_with(file_path, workdir),
        "line": line + 1,
        "col": col,
        "qualified_name": qname,
        "context": context,
    })
}

/// V50: Return closest symbol names in the index for typo correction / suggestions.
/// Pure-SQL progressive fallback: progressive prefix truncation, then prefix match,
/// then suffix match. Helps the model avoid dead-end retry loops when its
/// generated parameter doesn't exactly match any indexed symbol.
fn closest_symbols(db: &Connection, name: &str, limit: usize) -> Vec<String> {
    if name.is_empty() { return Vec::new(); }

    // V56: use the in-memory phrase index (SWAR/memchr, no SQLite LIKE scans).
    // Fall back to SQL only if the index isn't loadable.
    let db_path = cached_db_path().to_string_lossy().to_string();
    if let Some(idx) = reliary_search::phrase_index::get_phrase_index(db, &db_path) {
        let mut out: Vec<String> = Vec::new();
        let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
        // V57: edit-distance first — catches 1-char typos (relary -> reliary)
        // that prefix/substring can't. Only accept distance <= 2 as authoritative;
        // if none, fall through to prefix/substring.
        let mut close: Vec<(String, usize)> = Vec::new();
        for (cand, dist) in idx.closest(name, 8) {
            if dist <= 2 {
                close.push((cand.to_string(), dist));
            }
        }
        if !close.is_empty() {
            close.sort_by_key(|&(_, d)| d);
            for (s, _) in close {
                if seen.insert(s.clone()) {
                    out.push(s);
                    if out.len() >= limit { return out; }
                }
            }
            return out;
        }
        // Progressive prefix truncation (same semantics as the SQL version).
        for end in (3..name.len()).rev() {
            let prefix = &name[..end];
            for p in idx.find_prefix(prefix) {
                let s = p.to_string();
                if seen.insert(s.clone()) {
                    out.push(s);
                    if out.len() >= limit { return out; }
                }
            }
            if !out.is_empty() { return out; }
        }
        // Substring match.
        for p in idx.find_substring(name) {
            let s = p.to_string();
            if seen.insert(s.clone()) {
                out.push(s);
                if out.len() >= limit { return out; }
            }
        }
        // V57: looser edit-distance (d <= 3) as last resort.
        if out.is_empty() {
            for (cand, _dist) in idx.closest(name, limit) {
                let s = cand.to_string();
                if seen.insert(s.clone()) {
                    out.push(s);
                    if out.len() >= limit { break; }
                }
            }
        }
        return out;
    }

    // Fallback: original SQL path.
    // Try 1: progressive prefix truncation (strips chars from end until we get ≥1 match)
    for end in (3..name.len()).rev() {
        let prefix = &name[..end];
        let like = format!("{}%", prefix);
        if let Ok(mut stmt) = db.prepare_cached(
            "SELECT DISTINCT phrase FROM phrases WHERE phrase LIKE ?1 ORDER BY LENGTH(phrase) ASC LIMIT ?2"
        ) {
            if let Ok(rows) = stmt.query_map(params![&like, limit as i64], |r| r.get::<_, String>(0)) {
                let v: Vec<String> = rows.filter_map(|r| r.ok()).collect();
                if !v.is_empty() { return v; }
            }
        }
    }

    // Try 2: any phrase containing the query as a substring
    let like = format!("%{}%", name);
    if let Ok(mut stmt) = db.prepare_cached(
        "SELECT DISTINCT phrase FROM phrases WHERE phrase LIKE ?1 ORDER BY LENGTH(phrase) ASC LIMIT ?2"
    ) {
        if let Ok(rows) = stmt.query_map(params![&like, limit as i64], |r| r.get::<_, String>(0)) {
            let v: Vec<String> = rows.filter_map(|r| r.ok()).collect();
            if !v.is_empty() { return v; }
        }
    }

    Vec::new()
}

fn respond(id: &serde_json::Value, result: serde_json::Value) {
    let response = serde_json::json!({
        "jsonrpc": "2.0",
        "id": id,
        "result": result,
    });
    let mut out = io::stdout();
    let _ = serde_json::to_writer(&mut out, &response);
    let _ = writeln!(out);
    out.flush().unwrap_or_default();
}

fn respond_error(id: &serde_json::Value, code: i32, message: &str) {
    let response = serde_json::json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": { "code": code, "message": message },
    });
    let mut out = io::stdout();
    let _ = serde_json::to_writer(&mut out, &response);
    let _ = writeln!(out);
    out.flush().unwrap_or_default();
}

static PRIMARY_TOOLS: OnceLock<std::collections::HashSet<&'static str>> = OnceLock::new();
static ALL_TOOL_DEFS: OnceLock<Vec<serde_json::Value>> = OnceLock::new();

/// Public tool definitions for MCP tools/list — used by the stdio MCP server.
/// V27: Reduced from 19 tools to 7 intuitive tools. Each tool absorbs functionality
/// from previously separate variants. See FIX_PLAN_V27_TOOL_CONSOLIDATION.md.
pub fn tool_definitions() -> &'static Vec<serde_json::Value> {
    ALL_TOOL_DEFS.get_or_init(|| {
        vec![
            // 1. SEARCH: Find files/symbols by name or topic.
            serde_json::json!({ "name": "reliary_search", "description": "BM25 full-text search across the indexed codebase. Returns matching files ranked by relevance. USE THIS TOOL when you don't know the exact symbol name — e.g. 'find files about connection pooling', 'where is request handling implemented', 'find authentication code'. For symbol-level lookups (where is X defined, who calls X, etc.), use reliary_find_references instead — it returns structured one-line answers. Search returns a list of files with paths and relevance scores. The tool never returns empty — if no exact match, closest files by vocabulary similarity are returned.", "inputSchema": { "type": "object", "properties": { "query": {"type": "string", "description": "Natural-language search query. Examples: 'connection pooling', 'request handling', 'authentication'."}, "path": {"type": "string", "description": "Work directory to search. Defaults to '.'."}, "limit": {"type": "integer", "default": 15, "description": "Max files to return (default 15)"} }, "required": ["query"], "input_examples": [ { "query": "connection pooling" }, { "query": "request handling", "limit": 5 }, { "query": "authentication middleware" } ] } }),
            // 2. FIND_REFERENCES: Find all usages of a symbol. Absorbs with_source, type_flow, boltzmann.
            serde_json::json!({ "name": "reliary_find_references", "description": "Find references to a symbol in the indexed codebase. USE THIS TOOL for ALL symbol-level code questions. It replaces goto_def, find_references, call_graph, list_methods, find_dead_code, and describe in a single entry point with mode parameters. The output is a structured one-line answer with raw code evidence — copy it verbatim into your response. Mode flags: def_only=true → 'where is X defined' (returns top definition with source code). usage_only=true → 'who calls X' or 'callers of X' (returns CALLERS only — NOT what X calls). For 'what does X call / which helpers does X use', use reliary_call_graph with direction=outbound instead. methods=true → 'list methods on Type X' (returns method names of an impl block). dead_only=true → 'find dead code in path=X' (returns unused functions; path required, name ignored). path_filter='io/util/' → restrict to one module/folder (use when question specifies a module). NO params → general references to the symbol. If unsure which mode, call WITHOUT mode flags first — the tool returns all relevant knowledge.", "inputSchema": { "type": "object", "properties": { "name": {"type": "string", "description": "Symbol name to look up. Use the exact identifier name including underscores (e.g. 'block_on', 'Sleep', 'consume')."}, "def_only": {"type": "boolean", "default": false, "description": "Return ONLY the definition location. Use for 'where is X defined'."}, "usage_only": {"type": "boolean", "default": false, "description": "Return ONLY call sites (non-test files). Use for 'who calls X' or 'callers of X'."}, "methods": {"type": "boolean", "default": false, "description": "List methods on a type. Use for 'what methods does X have'."}, "dead_only": {"type": "boolean", "default": false, "description": "Find unused code. Pass path to scope the search."}, "path": {"type": "string", "description": "Work directory. Required for dead_only. Defaults to '.'."}, "path_filter": {"type": "string", "description": "Restrict to one module. Examples: 'io/util/' for tokio io utilities, 'crates/reliary-search/' for a Rust subdirectory. Use when question mentions a specific module."}, "file_only": {"type": "boolean", "default": false, "description": "V53: Return only the distinct list of files that mention this symbol (no file:line, no qualified names). Use for 'which files use X' or 'find files importing X' queries. Cheaper than full references."} }, "required": [], "input_examples": [ { "name": "block_on", "def_only": true }, { "name": "spawn", "usage_only": true }, { "name": "Sleep", "methods": true }, { "name": "consume", "path_filter": "io/util/" }, { "name": "", "dead_only": true, "path": "src/" }, { "name": "HashMap", "file_only": true } ] } }),
            // 3. GOTO_DEF: Jump to the definition of a symbol.
            serde_json::json!({ "name": "reliary_goto_def", "description": "DEPRECATED: use reliary_find_references(name=X, def_only=true) instead — it returns the same information in a structured format. This tool still works but is not the preferred entry point.", "inputSchema": { "type": "object", "properties": { "name": {"type": "string"}, "anchor_file": {"type": "string", "description": "Optional. Path to file containing the usage."}, "anchor_line": {"type": "integer", "description": "Optional. 1-based line of the usage."}, "path": {"type": "string"} }, "required": ["name"] } }),
            // 4. CALL_GRAPH: Who calls X? What does X call? Absorbs callgraph, callgraph_v2, trace_path, call_graph.
            serde_json::json!({ "name": "reliary_call_graph", "description": "PRIMARY call-graph tool. Who calls X? What does X call? Returns callers and callees with source. Use for 'who calls X' and 'what does X call' questions. Set direction='inbound' for callers, 'outbound' for callees, 'both' for full graph. Set depth=2-3 for multi-hop call chains (entry points like block_on/spawn/run auto-expand to depth 3). Pass anchor_file/anchor_line to disambiguate when multiple symbols share the same name.", "inputSchema": { "type": "object", "properties": { "name": {"type": "string"}, "anchor_file": {"type": "string", "description": "Optional. Path to the file containing the definition."}, "anchor_line": {"type": "integer", "description": "Optional. 1-based line of the definition."}, "direction": {"type": "string", "enum": ["inbound", "outbound", "both"], "default": "both"}, "depth": {"type": "integer", "default": 1, "description": "Recursion depth. Entry-point names (block_on, run, start, etc.) auto-expand to depth 3."}, "summary": {"type": "boolean", "description": "Set to true for compact output: caller/callee names only, no source text."}, "path": {"type": "string"} }, "required": ["name"] } }),
            // 5. LIST_METHODS: List all methods on a type.
            serde_json::json!({ "name": "reliary_list_methods", "description": "List all methods declared on a type. Scans for impl blocks and returns method names with file:line. Use for 'list methods on Type X' questions.", "inputSchema": { "type": "object", "properties": { "name": {"type": "string", "description": "Type name (e.g. 'Sleep', 'BufWriter', 'Runtime')"} }, "required": ["name"] } }),
            // 6. FIND_DEAD_CODE: Find unused/orphaned functions. Absorbs dead_symbols, dead.
            serde_json::json!({ "name": "reliary_find_dead_code", "description": "Find unused/orphaned functions in a module or codebase. Cross-references against the full index. Pass path='io/util' to scope to a module. Use 'summary' format for a high-level overview, 'list' for detailed results. Use for 'find dead code', 'find unused functions' questions.", "inputSchema": { "type": "object", "properties": { "path": {"type": "string", "description": "Module prefix to scope (e.g. 'io/util', 'runtime/scheduler'). Use '.' for whole codebase."}, "format": {"type": "string", "enum": ["list", "summary"], "default": "list"}, "limit": {"type": "integer", "default": 30}, "functions_only": {"type": "boolean", "default": true} }, "required": ["path"] } }),
            // 7. DESCRIBE: Explain a symbol — purpose, signature, callers, methods, surprise facts. Absorbs pack, pack_query, plan, risk.
            serde_json::json!({ "name": "reliary_describe", "description": "Explain a symbol: its purpose, signature, location, callers, and methods. V37: methods=true → 'list methods on Type X' (replaces list_methods). dead_only=true → 'find dead code in module X' (replaces find_dead_code). Use for 'explain X', 'what does X do', 'list methods on X', 'find dead code in X'.", "inputSchema": { "type": "object", "properties": { "name": {"type": "string", "description": "Symbol to describe (e.g. 'block_on', 'Sleep', 'BufWriter')"}, "file": {"type": "string", "description": "File path to show structure of (alternative to name)"}, "context": {"type": "string", "description": "Optional filter: 'callers', 'signature', 'behavior'"}, "path": {"type": "string"}, "methods": {"type": "boolean", "default": false, "description": "V37: List methods on a type. Replaces list_methods."}, "dead_only": {"type": "boolean", "default": false, "description": "V37: Find dead code in a module. Replaces find_dead_code. Pass path to scope to a module."}, "limit": {"type": "integer", "default": 30, "description": "Max dead code items to return"}, "functions_only": {"type": "boolean", "default": true} } } }),
            serde_json::json!({ "name": "reliary_similar", "description": "Find functions structurally similar to the named function (near-clone detection via hypervector token-set similarity). Use for 'find duplicate code', 'similar functions to X', 'is this copied elsewhere'.", "inputSchema": { "type": "object", "properties": { "name": { "type": "string", "description": "Function name" }, "path": { "type": "string" }, "limit": { "type": "integer", "default": 8 } }, "required": ["name"] } }),
            // 9. VERIFY: mechanically verify claims (symbol at file:line) against the index.
            serde_json::json!({ "name": "reliary_verify", "description": "Mechanically verify claims about the codebase against the index. Pass text containing 'symbol at file.rs:line' claims; returns VERIFIED or FALSE with the actual location. Use before asserting a location, or to check a statement from another source.", "inputSchema": { "type": "object", "properties": { "text": { "type": "string", "description": "Claim text, e.g. 'classify_structural at structural.rs:31'" }, "path": { "type": "string" }, "tol": { "type": "integer", "default": 1, "description": "Line tolerance" } }, "required": ["text"] } }),
        ]
    })
}

pub fn tool_definitions_filtered() -> &'static Vec<serde_json::Value> {
    static FILTERED: OnceLock<Vec<serde_json::Value>> = OnceLock::new();
    FILTERED.get_or_init(|| {
        // W7: default menu is 6 tools. `goto_def` is deprecated (superseded by
        // find_references def_only) and `similar` is a niche near-clone tool —
        // both stay dispatchable for backward compat but are hidden unless
        // RELIARY_FULL_MENU=1 is set.
        let full_menu = std::env::var("RELIARY_FULL_MENU").map(|v| v == "1").unwrap_or(false);
        let primary = PRIMARY_TOOLS.get_or_init(|| {
            // V27: 7-tool surface — see FIX_PLAN_V27_TOOL_CONSOLIDATION.md
            // V58: + reliary_similar (HDC near-clone detection) → 8 tools.
            // W7: goto_def + similar hidden by default → 6 shown.
            ["reliary_search", "reliary_find_references", "reliary_goto_def",
             "reliary_call_graph", "reliary_list_methods", "reliary_find_dead_code",
             "reliary_describe", "reliary_similar", "reliary_verify"].iter().copied().collect()
        });
        let hidden: std::collections::HashSet<&'static str> =
            ["reliary_goto_def", "reliary_similar"].iter().copied().collect();
        tool_definitions()
            .iter()
            .filter(|t| {
                let name = t.get("name").and_then(|n| n.as_str()).unwrap_or("");
                primary.contains(name) && (full_menu || !hidden.contains(name))
            })
            .cloned()
            .collect()
    })
}

/// Pure dispatch result — returned by dispatch_tool_call for use by the stdio MCP server.
pub enum DispatchResult {
    Success(serde_json::Value),
    Error(i32, String),
}

/// V13: Centralized sift + first-appearance freeze for tool outputs.
///
/// When `RELIARY_SIFT_TOOLS=1`, every successful tool result is passed through
/// `reliary_output::compress_unified()` before being returned to the LLM.
///
/// **Cache safety (first-appearance freeze)**: The compressed output is cached
/// keyed by the hash of the ORIGINAL text. On every subsequent turn (when the
/// MCP server is called again with the same query), we return the FROZEN
/// compressed bytes from the first call. This ensures the provider's KV cache
/// sees identical token sequences across turns — no cache busting.
///
/// Without the freeze, sift would produce slightly different output each call
/// V15: truncate text fields in tool results to keep multi-turn context manageable.
///
/// Default limit: 1500 chars per text field. Some tools have natural "summary"
/// responses that are short; others (callgraph multi-hop, find_references
/// with source) can be 5-10 KB. Capping at 1500 cuts context bloat ~40%
/// across multi-turn sessions without losing the answer.
///
/// Skipped when:
///   - RELIARY_NO_TRUNCATE=1 (env)
///   - args contains "verbose"=true (per-tool opt-in)
///   - args contains "limit"=N (per-tool opt-in, already truncates upstream)
///   - tool is callgraph_v2/callgraph (V15 multi-hop output MUST stay intact
///     for q4 call-chain to surface — truncation hides depth-2 callees)
fn truncate_result(result: &mut DispatchResult, args: &serde_json::Map<String, serde_json::Value>, name: &str) {
    if is_no_truncate() { return; }
    let limit = truncate_limit();
    // V58 P6c: describe() gets a tighter budget — the model reads the answer
    // line first; evidence beyond ~1200 chars is re-billed, rarely used.
    let limit = if name == "reliary_describe" { limit.min(1200) } else { limit };
    // Per-tool override: if args.verbose=true, skip.
    if args.get("verbose").and_then(|v| v.as_bool()) == Some(true) { return; }

    if let DispatchResult::Success(ref mut json) = result {
        if let Some(content) = json.get_mut("content").and_then(|c| c.as_array_mut()) {
            for item in content.iter_mut() {
                // P1-4: check length without allocating a full String copy first.
                if let Some(text) = item.get("text").and_then(|t| t.as_str()) {
                    if text.len() > limit {
                        let safe_end = text.floor_char_boundary(limit);
                        let truncated = format!(
                            "{}\n\n[... {} more chars truncated; pass verbose=true or set RELIARY_NO_TRUNCATE=1 for full output ...]",
                            &text[..safe_end],
                            text.len() - safe_end,
                        );
                        if let Some(obj) = item.as_object_mut() {
                            obj.insert("text".to_string(), serde_json::Value::String(truncated));
                        }
                    }
                }
            }
        }
    }
}
/// C12: JSON-RPC + semantic error codes.
/// Standard codes per JSON-RPC 2.0 spec.
pub mod codes {
    #[allow(dead_code)]
    pub const PARSE_ERROR: i32 = -32700;
    #[allow(dead_code)]
    pub const INVALID_REQUEST: i32 = -32600;
    #[allow(dead_code)]
    pub const METHOD_NOT_FOUND: i32 = -32601;
    pub const INVALID_PARAMS: i32 = -32602;
    #[allow(dead_code)]
    pub const INTERNAL_ERROR: i32 = -32603;
    /// Custom range -32000..-32099 for server-defined errors.
    pub const NOT_FOUND: i32 = -32001;        // File/symbol/path doesn't exist
    pub const INVALID_PATH: i32 = -32002;      // Path traversal/escape attempt
    pub const DB_ERROR: i32 = -32003;          // SQLite error
    pub const IO_ERROR: i32 = -32004;           // Filesystem error (permission, etc)
    #[allow(dead_code)]
    pub const SCHEMA_MISMATCH: i32 = -32005;    // Index file out of date
    pub const NOT_IMPLEMENTED: i32 = -32006;    // Stub/future work
}

/// C12: Convenience helpers for common error patterns.
pub fn err_not_found(what: &str) -> DispatchResult {
    DispatchResult::Error(codes::NOT_FOUND, format!("{} not found", what))
}
pub fn err_invalid_path(msg: &str) -> DispatchResult {
    DispatchResult::Error(codes::INVALID_PATH, msg.to_string())
}
pub fn err_missing_param(name: &str) -> DispatchResult {
    DispatchResult::Error(codes::INVALID_PARAMS, format!("missing required parameter '{}'", name))
}
/// V66e: normalize a dead-code scope so it matches against absolute stored
/// paths. Strip "./", leading "/", trailing "/" — leaving a clean suffix like
/// "src" or "tmp/corpus/src" that dead_symbols' %/{scope}% LIKE can match.
fn normalize_scope(s: &str) -> String {
    let t = s.trim_start_matches("./").trim_matches('/');
    if t.is_empty() { "src".to_string() } else { t.to_string() }
}

/// V66e: render a stored absolute path relative to the corpus root so tool
/// output is verifiable: "/tmp/corpus/src/search.rs" -> "src/search.rs".
/// Falls back to the basename for paths outside a src/ tree.
fn corpus_rel_path(path: &str) -> String {
    if let Some(idx) = path.find("/src/") {
        return path[idx + 1..].to_string();
    }
    if let Some(idx) = path.find("/lib/") {
        return path[idx + 1..].to_string();
    }
    path.rsplit('/').next().unwrap_or(path).to_string()
}

/// W4: one-line signature for a callee at (file, line). Reads from the
/// file_meta cache; truncates to 100 chars; empty when unavailable.
/// 1-indexed line (matches Callee.def_line display convention).
fn callee_signature(file: &str, line1: i32) -> String {
    let idx = line1.saturating_sub(1) as usize;
    let raw = match reliary_search::file_meta::get(file) {
        Some(meta) => match meta.lines.get(idx) {
            Some(s) => s.trim().to_string(),
            None => return String::new(),
        },
        None => return String::new(),
    };
    if raw.is_empty() || raw.starts_with("//") {
        return String::new();
    }
    // Only emit signature-looking lines (def or opening brace line).
    if !(raw.contains("fn ") || raw.contains("struct ") || raw.contains("enum ")
        || raw.contains("trait ") || raw.contains("type ") || raw.contains("impl ")) {
        return String::new();
    }
    if raw.len() > 100 {
        let end = raw.floor_char_boundary(100);
        return format!("{}...", &raw[..end]);
    }
    raw
}
/// M7: Require a non-empty 'name' parameter. Returns it or an error.
#[allow(dead_code)]
pub fn require_name(args: &serde_json::Map<String, serde_json::Value>) -> Result<String, DispatchResult> {
    args.get("name").and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
        .ok_or_else(|| err_missing_param("name"))
}
pub fn err_db(e: impl std::fmt::Display) -> DispatchResult {
    DispatchResult::Error(codes::DB_ERROR, format!("database error: {}", e))
}
pub fn err_io(what: &str, e: impl std::fmt::Display) -> DispatchResult {
    DispatchResult::Error(codes::IO_ERROR, format!("{}: {}", what, e))
}

/// V15: Detect names that look like "entry-point delegates" (block_on, run,
/// start, execute, main). For these, the call graph CAN go through a thin
/// wrapper into the real implementation. However, multi-hop expansion causes
/// significant token bloat (+80% WC) for marginal score lift (+1 on q4).
/// Defaulting to depth=1 preserves V14-level token efficiency. Users can
/// opt into deeper expansion via future `depth` parameter on the tool itself.
fn delegate_depth(name: &str) -> usize {
    // Entry-point patterns delegate deeply into the codebase.
    // block_on → block_on_inner → scheduler dispatch (3+ hops).
    let bare = name.rsplit("::").next().unwrap_or(name);
    matches!(bare,
        "block_on" | "run" | "start" | "execute" | "spawn"
        | "poll" | "poll_ready" | "poll_write" | "poll_read"
    ) as usize * 3 + 1
}

/// Pure dispatch: maps tool name + args → result or error. No I/O.
pub fn dispatch_tool_call(name: &str, args: &serde_json::Map<String, serde_json::Value>) -> DispatchResult {
    match name {
        "reliary_search" => {
            let query = args.get("query").and_then(|v| v.as_str()).unwrap_or("");
            let path = args.get("path").and_then(|v| v.as_str()).unwrap_or(".");
            // Bug 76: restrict search to workdir
            let sp = match safe_path(path, ".") {
                Ok(p) => p,
                Err(e) => return err_invalid_path(&e),
            };
            let _db_path = format!("{}/.reliary/index.sqlite", sp.to_string_lossy().trim_end_matches('/'));
            // V54: cached connection.
            match get_cached_db() {
                Some(db) => {
                    let results = reliary_search::search::search_fts5(&db, query, 10);
                        DispatchResult::Success(serde_json::json!({
                        "content": [{ "type": "text", "text": serde_json::to_string(&results.iter().map(|r| serde_json::json!({"file": r.file, "score": r.score})).collect::<Vec<_>>()).unwrap_or_default() }]
                    }))
                }
                None => {
                    let tokens = reliary_search::tokenize(query);
                    DispatchResult::Success(serde_json::json!({
                        "content": [{ "type": "text", "text": serde_json::json!({"results": [], "note": "no index — run index first", "stemmed": tokens.iter().map(|t| reliary_search::porter_stem(t)).collect::<Vec<_>>()}).to_string() }]
                    }))
                }
            }
        }
        "reliary_plan" => {
            let task = args.get("task").and_then(|v| v.as_str()).unwrap_or("");
            let path = args.get("path").and_then(|v| v.as_str()).unwrap_or(".");
            if task.is_empty() { return err_missing_param("task"); }
            let sp = match safe_path(path, ".") {
                Ok(p) => p,
                Err(e) => return err_invalid_path(&e),
            };
            let _db_path = format!("{}/.reliary/index.sqlite", sp.to_string_lossy().trim_end_matches('/'));
            // V54: cached connection.
            match get_cached_db() {
                Some(db) => {
                    let plan = reliary_search::plan::hologram_plan(&db, task);
                        DispatchResult::Success(serde_json::json!({
                        "content": [{ "type": "text", "text": serde_json::to_string(&plan).unwrap_or_default() }]
                    }))
                }
                None => err_db("cannot open index. Run `reliary trust .` first."),
            }
        }
        // V27: Aliases for the 7-tool surface. Each new name maps to an
        // existing handler. The old names (callgraph_v2, methods_on, etc.)
        // are kept for backwards compat in the 62-tool full menu.
        "reliary_call_graph" => {
            // Aliased to callgraph_v2 (best handler). Translate param if needed.
            // V60: honor `direction` — inbound → callers only, outbound → callees only.
            let direction = args.get("direction").and_then(|v| v.as_str()).unwrap_or("both");
            if direction == "inbound" {
                let mut a = args.clone();
                a.insert("usage_only".into(), serde_json::Value::Bool(true));
                return dispatch_tool_call("reliary_find_references", &a);
            }
            if direction == "outbound" {
                let mut a = args.clone();
                a.insert("summary".into(), serde_json::Value::Bool(true));
                return dispatch_tool_call("reliary_callgraph_v2", &a);
            }
            dispatch_tool_call("reliary_callgraph_v2", args)
        }
        "reliary_list_methods" => {
            dispatch_tool_call("reliary_methods_on", args)
        }
        "reliary_find_dead_code" => {
            dispatch_tool_call("reliary_dead_symbols", args)
        }
        "reliary_describe" => {
            // V37: methods → list_methods. dead_only → find_dead_code.
            let methods = args.get("methods").and_then(|v| v.as_bool()).unwrap_or(false);
            let dead_only = args.get("dead_only").and_then(|v| v.as_bool()).unwrap_or(false);
            if methods {
                let name = args.get("name").and_then(|v| v.as_str()).unwrap_or("");
                if name.is_empty() { return err_missing_param("name"); }
                let path = args.get("path").and_then(|v| v.as_str()).unwrap_or(".");
                let (db, _dir) = match open_symbol_index(path) {
                    Ok(t) => t,
                    Err(r) => return r,
                };
                let result = match reliary_search::callgraph_v2::find_methods_on(&db, name) {
                    Ok(mr) => {
                        // V38: raw code output — each method's actual signature.
                        let mut text = String::new();
                        for m in mr.methods.iter().take(20) {
                            let f_short = m.file.rsplit('/').next().unwrap_or(&m.file);
                            text.push_str(&format!("{}:{}\n", f_short, m.line));
                            if let Some(meta) = reliary_search::file_meta::get(&m.file) {
                                if let Some(line) = meta.lines.get(m.line.saturating_sub(1) as usize) {
                                    text.push_str(&format!("    {}\n", line));
                                }
                            }
                        }
                        DispatchResult::Success(serde_json::json!({
                            "content": [{ "type": "text", "text": text }]
                        }))
                    }
                    Err(e) => err_db(format!("methods_on: {}", e)),
                };
                return result;
            }
            if dead_only {
                // V66d: honor BOTH `path` and `path_filter` as the scope (the
                // model reaches for path_filter naturally).
                let path = args.get("path").and_then(|v| v.as_str()).unwrap_or(".").to_string();
                let pf = args.get("path_filter").and_then(|v| v.as_str()).unwrap_or("").to_string();
                let scope: Option<String> = if !pf.is_empty() {
                    Some(normalize_scope(&pf))
                } else {
                    // V66e: auto-scope repo roots (path="." or a root) to src/
                    // when a src/ dir exists, avoiding bench/config noise.
                    let (db2, _) = match open_symbol_index(&path) {
                        Ok(t) => t,
                        Err(r) => return r,
                    };
                    let has_src = db2.query_row(
                        "SELECT EXISTS(SELECT 1 FROM file_map WHERE file_path LIKE '%/src/%' LIMIT 1)",
                        [], |r| r.get::<_, i64>(0),
                    ).unwrap_or(0) > 0;
                    let root_like = path == "." || !path.contains("src");
                    if has_src && root_like {
                        let base = if path == "." { "" } else { path.trim_end_matches('/') };
                        Some(normalize_scope(&format!("{}/src", base)))
                    } else if path != "." {
                        Some(normalize_scope(&path))
                    } else {
                        None
                    }
                };
                let path_filter = scope.as_deref();
                let limit = args.get("limit").and_then(|v| v.as_u64()).unwrap_or(100) as usize;
                let (db, _dir) = match open_symbol_index(&path) {
                    Ok(t) => t,
                    Err(r) => return r,
                };
                let functions_only = args.get("functions_only").and_then(|v| v.as_bool()).unwrap_or(true);
                let result = match reliary_search::symbol::dead_symbols(&db, limit, path_filter, functions_only) {
                    Ok(dead) => {
                        // V40: One-line dead code output
                        let text = if dead.is_empty() {
                            "No dead code found.\n".to_string()
                        } else {
                            let parts: Vec<String> = dead.iter().take(5).map(|(stem, file, line, _)| {
                                let f_short = corpus_rel_path(file);
                                format!("{} at {}:{}", stem, f_short, line + 1)
                            }).collect();
                            format!("Dead code: {}.\n", parts.join(", "))
                        };
                        DispatchResult::Success(serde_json::json!({
                            "content": [{ "type": "text", "text": text }]
                        }))
                    }
                    Err(e) => err_db(format!("dead_symbols: {}", e)),
                };
                return result;
            }
            dispatch_tool_call("reliary_pack_query", args)
        }        "reliary_verify" => {
            // V70 P1: mechanical claim verification against the index.
            let text = args.get("text").and_then(|v| v.as_str()).unwrap_or("");
            if text.is_empty() { return err_missing_param("text"); }
            let path = args.get("path").and_then(|v| v.as_str()).unwrap_or(".");
            let tol = args.get("tol").and_then(|v| v.as_i64()).unwrap_or(1) as i32;
            let (_db, _dir) = match open_symbol_index(path) {
                Ok(t) => t,
                Err(r) => return r,
            };
            let summary = crate::verify::verify_text(&_db, text, tol);
            let text_out = if summary.claims.is_empty() {
                "NO CLAIMS — nothing verifiable found in the input.\n".to_string()
            } else {
                let mut out = String::new();
                for (c, v) in &summary.claims {
                    let subject = if c.symbol.is_empty() {
                        format!("{}:{}", c.file, c.line)
                    } else {
                        format!("{} at {}:{}", c.symbol, c.file, c.line)
                    };
                    match v {
                        crate::verify::Verdict::Verified { .. } => out.push_str(&format!("VERIFIED {}\n", subject)),
                        crate::verify::Verdict::False { actual } => {
                            let actual_str = actual.as_ref()
                                .map(|(f, l)| format!(" -> actual: {}:{}", f, l))
                                .unwrap_or_else(|| " -> not found in index".to_string());
                            out.push_str(&format!("FALSE {}{}\n", subject, actual_str));
                        }
                    }
                }
                out.push_str(&format!("\n{} verified, {} false\n", summary.verified, summary.falsified));
                out
            };
            DispatchResult::Success(serde_json::json!({ "content": [{ "type": "text", "text": text_out }] }))
        }
        "reliary_similar" => {
            // V58 P2a: HDC near-clone detection.
            let name = args.get("name").and_then(|v| v.as_str()).unwrap_or("");
            if name.is_empty() { return err_missing_param("name"); }
            let path = args.get("path").and_then(|v| v.as_str()).unwrap_or(".");
            let top_n = args.get("limit").and_then(|v| v.as_u64()).unwrap_or(8).min(20) as usize;
            let (db, _dir) = match open_symbol_index(path) {
                Ok(t) => t,
                Err(r) => return r,
            };
            let hits = reliary_search::similar::find_similar(&db, name, top_n);
            let text = if hits.is_empty() {
                format!("No similar functions found for \"{}\".\n", name)
            } else {
                let items: Vec<String> = hits.iter()
                    .map(|h| format!("{} ({}:{}, sim {:.2})", h.name, h.file.rsplit('/').next().unwrap_or(&h.file), h.line + 1, h.similarity))
                    .collect();
                format!("Functions similar to {}: {}\n", name, items.join(", "))
            };
            DispatchResult::Success(serde_json::json!({
                "content": [{ "type": "text", "text": text }]
            }))
        }

        "reliary_compress" => {
            let text = args.get("text").and_then(|v| v.as_str()).unwrap_or("");
            // S5: limit input size to prevent OOM
            if text.len() > 1_000_000 {
                return DispatchResult::Error(codes::INVALID_PARAMS, "text too large for compression (max 1MB)".into());
            }
            let compressed = reliary_compress::compress_reasoning(text, None);
            let result = serde_json::json!({
                "compressed": compressed,
                "original_len": text.len(),
                "compressed_len": compressed.as_ref().map(|c| c.len()).unwrap_or(0),
            });
            DispatchResult::Success(serde_json::json!({
                "content": [{ "type": "text", "text": result.to_string() }]
            }))
        }
        "reliary_find_references_boltzmann" => {
            let sym = args.get("name").and_then(|v| v.as_str()).unwrap_or("");
            let af_raw = args.get("anchor_file").and_then(|v| v.as_str()).unwrap_or("");
            let path = args.get("path").and_then(|v| v.as_str()).unwrap_or(".");
            let (db, dir) = match open_symbol_index(path) {
                Ok(t) => t,
                Err(r) => return r,
            };
            let af = if std::path::Path::new(af_raw).is_absolute() { af_raw.to_string() } else { format!("{}/{}", dir, af_raw) };
let al_raw = args.get("anchor_line").and_then(|v| v.as_i64()).unwrap_or(0) as i32; let al = if al_raw > 0 { al_raw - 1 } else { al_raw };
            let th = args.get("threshold").and_then(|v| v.as_f64()).unwrap_or(0.3) as f32;
            let temp = args.get("temperature").and_then(|v| v.as_f64()).unwrap_or(2.0) as f32;
            let tau = args.get("tau").and_then(|v| v.as_f64()).unwrap_or(0.05) as f32;
            let result = match reliary_search::type_flow::find_references_type_flow(&db, sym, &af, al, th) {
                Ok(hits) => {
                    let raw_hits: Vec<_> = hits.iter().map(|h| reliary_search::boltzmann::RawHit {
                        file: h.file_path.clone(),
                        line: h.line,
                        col: h.col,
                        score: h.similarity,
                    }).collect();
                    let calibrated = reliary_search::boltzmann::calibrate_hits(&raw_hits, temp, tau);
                    let wd = cached_cwd().to_string_lossy().to_string();
                    let arr: Vec<_> = calibrated.iter().map(|h| serde_json::json!({
                        "file": relpath_with(&h.file, &wd),
                        "line": h.line + 1,
                        "col": h.col,
                        "score": h.score,
                        "probability": h.probability,
                        "percentile": h.percentile,
                    })).collect();
                    DispatchResult::Success(serde_json::json!({
                        "content": [{ "type": "text", "text": serde_json::json!({
                            "name": sym, "anchor_file": af, "anchor_line": al,
                            "threshold": th, "method": "boltzmann", "temperature": temp, "tau": tau,
                            "raw_count": hits.len(), "calibrated_count": calibrated.len(),
                            "hits": arr
                        }).to_string() }]
                    }))
                }
                Err(e) => err_db(format!("boltzmann: {}", e)),
            };
            let _ = &db;
            result
        }
        "reliary_risk" => {
            let file_arg = args.get("file").and_then(|v| v.as_str()).unwrap_or("");
            // Bug 77: restrict file reads to workdir
            let fp = match safe_path(file_arg, ".") {
                Ok(p) => p,
                Err(e) => return err_invalid_path(&e),
            };
            let file_str = fp.to_string_lossy().to_string();
            if let Ok(meta) = std::fs::metadata(&file_str) {
                if meta.len() > 10_000_000 {
                    return DispatchResult::Error(codes::INVALID_PARAMS, "file too large".into());
                }
            }
            match std::fs::read_to_string(&file_str) {
                Ok(content) => {
                    let risk = reliary_risk::compute_file_risk(&file_str, &content);
                    let blast_radius = reliary_risk::compute_blast_radius(&content);
                    DispatchResult::Success(serde_json::json!({
                        "content": [{ "type": "text", "text": serde_json::json!({"file": risk.file, "risk": format!("{:?}", risk.risk), "reason": risk.reason, "blast_radius": blast_radius}).to_string() }]
                    }))
                }
                Err(e) => err_io("read", format!("{}", e)),
            }
        }
        "reliary_fix" => {
            let file_arg = args.get("file").and_then(|v| v.as_str()).unwrap_or("");
            let old = args.get("old").and_then(|v| v.as_str()).unwrap_or("");
            let new = args.get("new").and_then(|v| v.as_str()).unwrap_or("");
            let context = args.get("context").and_then(|v| v.as_str()).unwrap_or("");
            // M6: require either old/new OR context; reject all-empty
            if old.is_empty() && new.is_empty() && context.is_empty() {
                return err_missing_param("old/new/context");
            }
            // Bug 77: restrict file writes to workdir
            let fp = match safe_path(file_arg, ".") {
                Ok(p) => p,
                Err(e) => return err_invalid_path(&e),
            };
            let file_str = fp.to_string_lossy().to_string();
            if let Ok(meta) = std::fs::metadata(&file_str) {
                if meta.len() > 10_000_000 {
                    return DispatchResult::Error(codes::INVALID_PARAMS, "file too large".into());
                }
            }
            match std::fs::read_to_string(&file_str) {
                Ok(content) => {
                    let fixes = if old.is_empty() && new.is_empty() && !context.is_empty() {
                        reliary_fix::content_aware_match(context, &content)
                    } else {
                        vec![(old.to_string(), new.to_string())]
                    };
                    let (modified, count) = reliary_fix::apply_fixes(&content, &fixes);
                    if count > 0 {
                        if reliary_core::atomic_write(&file_str, &modified).is_ok() {
                            DispatchResult::Success(serde_json::json!({
                                "content": [{ "type": "text", "text": serde_json::json!({"success": true, "replacements": count, "file": file_str}).to_string() }]
                            }))
                        } else {
                            DispatchResult::Error(codes::IO_ERROR, "cannot write file".into())
                        }
                    } else {
                        err_not_found("matches")
                    }
                }
                Err(e) => err_io("read", format!("{}", e)),
            }
        }
        "reliary_dead" => {
            let path_arg = args.get("path").and_then(|v| v.as_str()).unwrap_or(".");
            let dp = match safe_path(path_arg, ".") {
                Ok(p) => p,
                Err(e) => return err_invalid_path(&e),
            };
            let limit = args.get("limit").and_then(|v| v.as_u64()).unwrap_or(10) as usize;
            let min_confidence = args.get("confidence").and_then(|v| v.as_str()).unwrap_or("all");
            let config = reliary_dead::DeadConfig::default();
            // V13: use cross-file scan_repo (carrion algorithm) instead of per-file.
            let candidates = reliary_dead::scan_repo(&dp.to_string_lossy(), &config);
            let filtered: Vec<_> = candidates.iter().filter(|c| {
                match min_confidence {
                    "high" => c.confidence == reliary_dead::Confidence::High,
                    "medium" => c.confidence == reliary_dead::Confidence::High || c.confidence == reliary_dead::Confidence::Medium,
                    _ => true,
                }
            }).collect();
            let (mut high, mut medium, mut low) = (0usize, 0usize, 0usize);
            for c in &filtered {
                match c.confidence {
                    reliary_dead::Confidence::High => high += 1,
                    reliary_dead::Confidence::Medium => medium += 1,
                    reliary_dead::Confidence::Low => low += 1,
                }
            }
            let top: Vec<_> = filtered.iter().take(limit).map(|c| {
                let conf_str = match c.confidence {
                    reliary_dead::Confidence::High => "high",
                    reliary_dead::Confidence::Medium => "medium",
                    reliary_dead::Confidence::Low => "low",
                };
                serde_json::json!({"name": c.name, "file": c.file, "line": c.line, "confidence": conf_str})
            }).collect();
            let mut response_obj = serde_json::json!({
                "total": filtered.len(),
                "high": high,
                "medium": medium,
                "low": low,
                "items": top,
            });
            if filtered.len() > limit {
                if let Some(obj) = response_obj.as_object_mut() {
                    obj.insert("truncated".to_string(), serde_json::json!(true));
                    obj.insert("limit".to_string(), serde_json::json!(limit));
                }
            }
            DispatchResult::Success(serde_json::json!({
                "content": [{ "type": "text", "text": serde_json::to_string(&response_obj).unwrap_or_default() }]
            }))
        }
        "reliary_heal" => {
            // Removed in v0.8: self-healing edit was a control-layer primitive
            // (shadow-apply, test, revert-on-fail) — proven net-negative vs the LLM.
            // reliary_edit provides grammar-free resolve/apply without the control loop.
            DispatchResult::Error(codes::NOT_IMPLEMENTED, "reliary_heal removed in v0.8 (control layer dropped). Use reliary_fix or reliary_edit.".into())
        }
        "reliary_prior" => {
            let path_arg = args.get("path").and_then(|v| v.as_str()).unwrap_or(".");
            // Bug 76: restrict prior file reads to workdir
            let dp = match safe_path(path_arg, ".") {
                Ok(p) => p,
                Err(e) => return err_invalid_path(&e),
            };
            let prior = match std::fs::read_to_string(dp.join(".reliary").join("prior_block")) {
                Ok(p) => p.trim().to_string(),
                Err(_) => String::new(),
            };
            DispatchResult::Success(serde_json::json!({
                "content": [{ "type": "text", "text": serde_json::json!({"prior": prior}).to_string() }]
            }))
        }
        "reliary_retrieve" => {
            let hash = args.get("hash").and_then(|v| v.as_str()).unwrap_or("");
            if hash.is_empty() {
                return err_missing_param("hash");
            }
            let cache_path = std::path::Path::new(".reliary/cache.sqlite");
            match crate::paths::open_or_create(cache_path) {
                Ok(conn) => match reliary_core::retrieve(&conn, hash) {
                    Ok(Some(content)) => DispatchResult::Success(serde_json::json!({
                        "content": [{ "type": "text", "text": content }]
                    })),
                    Ok(None) => err_not_found(&format!("hash {}", hash)),
                    Err(e) => err_db(format!("retrieve: {}", e)),
                },
                Err(_) => err_io("cache open", "not found"),
            }
        }
        "reliary_stats" => {
            let cache_path = std::path::Path::new(".reliary/cache.sqlite");
            let (count, bytes) = match crate::paths::open_or_create(cache_path) {
                Ok(conn) => reliary_core::stats(&conn).unwrap_or((0, 0)),
                Err(_) => (0, 0),
            };
            DispatchResult::Success(serde_json::json!({
                "content": [{ "type": "text", "text": serde_json::json!({
                    "cache_entries": count,
                    "cache_bytes": bytes,
                }).to_string() }]
            }))
        }
        // ───── Symbol-level vocab (occurrence-level, grammar-free) ─────
        "reliary_find_references" | "reliary_find_references_type_flow" | "reliary_find_references_with_source" | "reliary_brace_graph" | "reliary_brace_debug" | "reliary_goto_def" | "reliary_callgraph" | "reliary_callgraph_v2" | "reliary_methods_on" | "reliary_scope" | "reliary_dead_symbols" => {
            handle_symbol_tool(name, args)
        }
        "reliary_query_ast" => {
            // Already handled inline in this match — but actually we need to
            // reach it here.
            disp_query_ast(name, args)
        }
        "reliary_architecture" => {
            let path_arg = args.get("path").and_then(|v| v.as_str()).unwrap_or(".");
            let limit = args.get("limit").and_then(|v| v.as_u64()).unwrap_or(20) as usize;
            // Bug 76: restrict to workdir
            let pp = match safe_path(path_arg, ".") {
                Ok(p) => p,
                Err(e) => return err_invalid_path(&e),
            };
            let pp_str = pp.to_string_lossy().to_string();
            let _db_path = format!("{}/.reliary/index.sqlite", pp_str.trim_end_matches('/'));
            // V54: cached connection.
            match get_cached_db() {
                Some(db) => {
                    match reliary_search::architecture::get_architecture(&db, &pp_str, limit) {
                        Ok(summary) => {
                                        DispatchResult::Success(serde_json::json!({
                                "content": [{ "type": "text", "text": serde_json::to_string(&summary).unwrap_or_default() }]
                            }))
                        }
                        Err(e) => {
                                        err_db(format!("architecture: {}", e))
                        }
                    }
                }
                None => err_db("cannot open index. Run `reliary trust .` first."),
            }
        }
        "reliary_trace_path" => {
            let path_arg = args.get("path").and_then(|v| v.as_str()).unwrap_or(".");
            let pp = match safe_path(path_arg, ".") {
                Ok(p) => p,
                Err(e) => return err_invalid_path(&e),
            };
            let pp_str = pp.to_string_lossy().to_string();
            let _db_path = format!("{}/.reliary/index.sqlite", pp_str.trim_end_matches('/'));
            // V54: cached connection.
            let name = args.get("name").and_then(|v| v.as_str()).unwrap_or("");
            if name.is_empty() { return err_missing_param("name"); }
            let af_raw = args.get("anchor_file").and_then(|v| v.as_str()).unwrap_or("");
let al_raw = args.get("anchor_line").and_then(|v| v.as_i64()).unwrap_or(0) as i32; let al = if al_raw > 0 { al_raw - 1 } else { al_raw };
            let direction = args.get("direction").and_then(|v| v.as_str()).unwrap_or("inbound");
            let depth = args.get("depth").and_then(|v| v.as_u64()).unwrap_or(1) as usize;
            // Resolve anchor_file to a key the SQL can match (try as-is, then as suffix).
            let af = if af_raw.is_empty() { ".".to_string() } else { af_raw.to_string() };
            match get_cached_db() {
                Some(db) => {
                    let result = reliary_search::trace_path::trace_path(
                        &db, name, &af, al, direction, depth, &pp_str,
                    );
                        match result {
                        Ok(r) => DispatchResult::Success(serde_json::json!({
                            "content": [{ "type": "text", "text": serde_json::to_string(&r).unwrap_or_default() }]
                        })),
                        Err(e) => err_db(format!("trace_path: {}", e)),
                    }
                }
                None => err_db("cannot open index. Run `reliary trust .` first."),
            }
        }
        // S2 fix: these MUST come before the `_ =>` catch-all on line ~456, otherwise the
        // catch-all eats them and the model gets "unknown tool: reliary_pack_query" errors.
        "reliary_pack" => {
            let path = args.get("path").and_then(|v| v.as_str()).unwrap_or(".");
            let sp = match safe_path(path, ".") {
                Ok(p) => p,
                Err(e) => return err_invalid_path(&format!("invalid path: {}", e)),
            };
            let format_str = args.get("format").and_then(|v| v.as_str()).unwrap_or("auto");
            let sp_str = sp.to_string_lossy().to_string();
            let result = if format_str == "auto" {
                reliary_pack::generate_pack_auto(&sp_str)
            } else {
                let format = if format_str == "full" {
                    reliary_pack::PackFormat::Full
                } else {
                    reliary_pack::PackFormat::L2L3
                };
                reliary_pack::generate_pack(&sp_str, format)
            };
            match result {
                Ok(pack) => DispatchResult::Success(serde_json::json!({
                    "content": [{ "type": "text", "text": pack }]
                })),
                Err(e) => err_db(format!("reliary_pack: {}", e)),
            }
        }
        "reliary_pack_query" => {
            // S2 fix: query the pack for a specific symbol's entry.
            // Reads .reliary/pack_l2l3.md (auto-generated by reliary_pack tool).
            let name = args.get("name").and_then(|v| v.as_str()).unwrap_or("");
            let context = args.get("context").and_then(|v| v.as_str()).unwrap_or("");
            let path = args.get("path").and_then(|v| v.as_str()).unwrap_or(".");
            let sp = match safe_path(path, ".") {
                Ok(p) => p,
                Err(e) => return err_invalid_path(&format!("invalid path: {}", e)),
            };
            let sp_str = sp.to_string_lossy().to_string();

            // 1. Ensure pack file exists; generate if not
            let pack_md_path = format!(
                "{}/.reliary/pack_l2l3.md",
                sp_str.trim_end_matches('/')
            );
            // File must exist AND have real content (>1KB — the empty version is just "# Holographic Pack — . (0 symbols)\n").
            let pack_path_exists = std::path::Path::new(&pack_md_path)
                .exists()
                && std::fs::metadata(&pack_md_path).map(|m| m.len() > 1024).unwrap_or(false);
            let pack_content = if pack_path_exists {
                match std::fs::read_to_string(&pack_md_path) {
                    Ok(c) => c,
                    Err(e) => return err_io("read pack", format!("{}", e)),
                }
            } else {
                // Auto-generate the pack (this can take a few seconds for large codebases)
                match reliary_pack::generate_pack(&sp_str, reliary_pack::PackFormat::L2L3) {
                    Ok(pack) => {
                        let _ = std::fs::create_dir_all(
                            std::path::Path::new(&pack_md_path).parent().unwrap_or(std::path::Path::new(".")),
                        );
                        let _ = std::fs::write(&pack_md_path, &pack);
                        pack
                    }
                    Err(e) => return err_db(format!("generate pack: {}", e)),
                }
            };

            // 2. Extract entry for symbol name (pack uses "## <name>/<crate>" headers).
            let target_name = name;
            let target_lower = name.to_lowercase();

            // P10-2: single-pass parsing — build a HashMap of entry_name -> Vec<line>
            // in one iteration, then look up the target + cross-included callers.
            let mut entries: std::collections::HashMap<String, Vec<String>> = std::collections::HashMap::new();
            let mut current_entry: Option<String> = None;
            for line in pack_content.lines() {
                if line.starts_with("## ") {
                    let heading = line.trim_start_matches("## ");
                    let entry_name = heading.split('/').next().unwrap_or("").trim().to_string();
                    current_entry = Some(entry_name.clone());
                    entries.entry(entry_name).or_default().push(line.to_string());
                } else if let Some(en) = &current_entry {
                    entries.entry(en.clone()).or_default().push(line.to_string());
                }
            }

            // First, find the target entry to discover L4 callers.
            // V28 Fix 5: case-insensitive lookup — pack stores lowercase, query may be PascalCase.
            let mut callers_to_include: std::collections::HashSet<String> = std::collections::HashSet::new();
            let target_entry = entries.get(target_name)
                .or_else(|| entries.get(&target_lower));
            if let Some(target_lines) = target_entry {
                for line in target_lines {
                    if line.contains("caller") || line.contains("match sites") || line.contains("constructors") {
                        for token in line.split([':', ',', '/', ';']) {
                            let t = token.trim();
                            if !t.is_empty() && t != target_name {
                                callers_to_include.insert(t.to_string());
                            }
                        }
                    }
                }
            }

            // Assemble the final entry: target + caller entries.
            // V28 Fix 5: case-insensitive lookup.
            let mut entry_lines: Vec<String> = Vec::new();
            if let Some(t) = entries.get(target_name).or_else(|| entries.get(&target_lower)) {
                entry_lines.extend(t.iter().cloned());
            }
            for caller in &callers_to_include {
                if caller != target_name && caller != &target_lower {
                    if let Some(c) = entries.get(caller).or_else(|| entries.get(&caller.to_lowercase())) {
                        entry_lines.extend(c.iter().cloned());
                    }
                }
            }

            if entry_lines.is_empty() {
                // V69: fall back to the index. A symbol with no pack entry may
                // still be a real definition — re-dispatch as a definition
                // lookup instead of dead-ending with "try search first".
                let mut a = args.clone();
                a.insert("def_only".into(), serde_json::Value::Bool(true));
                return dispatch_tool_call("reliary_find_references", &a);
            }

            // Optional context filter
            let context_lower = context.to_lowercase();
            let mut rendered = entry_lines.join("\n");
            if context_lower.contains("caller") {
                let header = entry_lines.first().cloned().unwrap_or_default();
                let l4: Vec<String> = entry_lines.iter().skip(1).filter(|l| l.starts_with("L4:")).cloned().collect();
                rendered = if l4.is_empty() { header } else { format!("{}\n{}", header, l4.join("\n")) };
            } else if context_lower.contains("surprise") || context_lower.contains("behavior") {
                let l3: Vec<String> = entry_lines.iter().filter(|l| l.starts_with("L3:")).cloned().collect();
                rendered = if l3.is_empty() { "No surprise info available.".to_string() } else { l3.join("\n") };
            } else if context_lower.contains("signature") {
                let l2: Vec<String> = entry_lines.iter().filter(|l| l.starts_with("L2:")).cloned().collect();
                rendered = if l2.is_empty() { "No signature info available.".to_string() } else { l2.join("\n") };
            }

            let header_note = if !callers_to_include.is_empty() {
                format!(" (callers included: {})",
                    callers_to_include.iter().take(5).cloned().collect::<Vec<_>>().join(", "))
            } else { String::new() };

            // P2: append a one-line blast-radius summary when the symbol
            // resolves in the index (describe is the pre-edit context tool).
            // M5: also emit the composite "at a glance" block — definition,
            // top callers with locations, and test files — collapsing the
            // common 3-call workflow (find_references + call_graph + test-plan)
            // into this single deterministic response. All facts already exist.
            let (impact_line, glance_block) = match open_symbol_index(&sp_str) {
                Ok((db2, _)) => match reliary_search::impact::compute_impact(&db2, target_name, &sp_str) {
                    Ok(imp) if !imp.def_file.is_empty() => {
                        let def_short = std::path::Path::new(&imp.def_file)
                            .file_name()
                            .map(|x| x.to_string_lossy().to_string())
                            .unwrap_or_else(|| imp.def_file.clone());
                        let mut g = format!("\n\nat a glance:\n- defined: {}:{}", def_short, imp.def_line);
                        if !imp.callers.is_empty() {
                            let cs: Vec<String> = imp.callers.iter().take(5).map(|(f, l)| {
                                let b = std::path::Path::new(f).file_name()
                                    .map(|x| x.to_string_lossy().to_string())
                                    .unwrap_or_else(|| f.clone());
                                format!("{}:{}", b, l)
                            }).collect();
                            g.push_str(&format!("\n- callers ({}): {}", imp.callers.len(), cs.join(", ")));
                        } else {
                            g.push_str("\n- callers (0)");
                        }
                        if !imp.test_files.is_empty() {
                            let ts: Vec<String> = imp.test_files.iter().take(4).map(|f| {
                                std::path::Path::new(f).file_name()
                                    .map(|x| x.to_string_lossy().to_string())
                                    .unwrap_or_else(|| f.clone())
                            }).collect();
                            g.push_str(&format!("\n- tests ({}): {}", imp.test_files.len(), ts.join(", ")));
                        }
                        g.push_str(&format!("\n- risk: {}", imp.risk.label()));
                        (format!("\n{}", reliary_search::impact::summary_line(&imp)), g)
                    }
                    _ => (String::new(), String::new()),
                },
                Err(_) => (String::new(), String::new()),
            };

            DispatchResult::Success(serde_json::json!({
                "content": [{ "type": "text", "text":
                    format!("# Pack Entry: {}{}{}{}\n\n```markdown\n{}\n```",
                        target_name, header_note, impact_line, glance_block, rendered)
                }]
            }))
        }
        _ => DispatchResult::Error(-32601, format!("unknown tool: {}", name)),
    }
}

fn disp_query_ast(name: &str, args: &serde_json::Map<String, serde_json::Value>) -> DispatchResult {
    let pattern = args.get("pattern").and_then(|v| v.as_str()).unwrap_or("");
    let file = args.get("file").and_then(|v| v.as_str()).unwrap_or("");
    let max_results = args.get("max_results").and_then(|v| v.as_i64()).unwrap_or(100).clamp(1, 1000) as usize;
    let fp = match safe_path(file, ".") {
        Ok(p) => p,
        Err(e) => return err_invalid_path(&format!("invalid file path: {}", e)),
    };
    let pat = match reliary_search::refal::parse_pattern(pattern) {
        Ok(p) => p,
        Err(e) => return err_db(format!("parse: {}", e)),
    };
    let mut table = reliary_search::op_table::OpTable::new();
    table.postfix.insert(".".to_string(), reliary_search::op_table::OpEntry { precedence: 100.0, associativity: 'L' });
    table.postfix.insert("?.".to_string(), reliary_search::op_table::OpEntry { precedence: 100.0, associativity: 'L' });
    for (op, p) in [("+", 5.0), ("-", 5.0), ("*", 7.0), ("/", 7.0), ("==", 3.0),
                      ("!=", 3.0), ("<", 4.0), (">", 4.0), ("&&", 2.0), ("||", 1.0)] {
        table.entries.insert(op.to_string(), reliary_search::op_table::OpEntry { precedence: p, associativity: 'L' });
    }
    let matches = reliary_search::refal::query_file(&fp.to_string_lossy(), &pat, &table);
    let truncated = if matches.len() > max_results { &matches[..max_results] } else { &matches[..] };
    let out: Vec<_> = truncated.iter().map(|m| serde_json::json!({
        "line": m.line,
        "expr": m.expr_text,
        "bindings": m.bindings,
    })).collect();
    let _ = name;
    DispatchResult::Success(serde_json::json!({
        "content": [{ "type": "text", "text": serde_json::json!({
            "pattern": pattern, "file": file,
            "total": matches.len(), "returned": out.len(),
            "matches": out
        }).to_string() }]
    }))
}

/// Helper: open the .reliary index at `path` and run a symbol query.
/// Returns DispatchResult::Error if the path is unsafe or index missing.
/// V73: returns a CachedDbGuard — the connection auto-returns to the
/// thread-local cache when the guard drops, so no call site can leak it.
fn open_symbol_index(path: &str) -> Result<(CachedDbGuard, String), DispatchResult> {
    // V15: distinguish between a path-as-directory and a path-as-filter.
    // For dead_symbols/callgraph etc., path="io/util" is a module filter, NOT
    // a subdirectory to open a new index. We always use the CWD's .reliary/
    // index, and treat path as a prefix filter passed to the SQL query.
    //
    // For tools that DO need a different index (none currently), the path
    // would point to a directory containing its own .reliary/ subdir.
    //
    // The safe_path check still validates that `path` doesn't escape CWD via
    // ..  but the result is only used for prefix matching, not DB location.
    let sp = safe_path(path, ".").map_err(|e| DispatchResult::Error(codes::INVALID_PATH, e))?;
    // Always open the index at CWD's .reliary/.
    // V54: reuse the thread-local cached connection (warm prepare_cached
    // statements, no per-call open/verify overhead).
    let db = get_cached_db().ok_or_else(|| {
        err_db(format!(
            "cannot open index at {}\n  -> run `reliary trust .` to build it",
            cached_db_path().display()
        ))
    })?;
    // Return dir as the SAFE-PATH result (for relative-path resolution)
    // but the DB is at CWD/.reliary/.
    let dir = sp.to_string_lossy().trim_end_matches('/').to_string();
    Ok((CachedDbGuard(Some(db)), dir))
}

fn handle_symbol_tool(name: &str, args: &serde_json::Map<String, serde_json::Value>) -> DispatchResult {
    let path = args.get("path").and_then(|v| v.as_str()).unwrap_or(".");
    let (db_guard, dir) = match open_symbol_index(path) {
        Ok(t) => t,
        Err(r) => return r,
    };
    let db = db_guard.conn();
    handle_symbol_tool_with_db(name, args, db, &dir)
    // db_guard drops here -> connection returns to the thread-local cache.
}

fn handle_symbol_tool_with_db(
    name: &str,
    args: &serde_json::Map<String, serde_json::Value>,
    db: &rusqlite::Connection,
    dir: &str,
) -> DispatchResult {
    // Resolve anchor_file against the index's workdir so relative paths from the
    // LLM (e.g. "config_parser.py") match the paths stored in file_map.
    let resolve_af = |af: &str| -> String {
        if af.is_empty() { return String::new(); }
        if std::path::Path::new(af).is_absolute() {
            af.to_string()
        } else {
            format!("{}/{}", dir, af)
        }
    };
    match name {
        "reliary_find_references" => {
            let sym = args.get("name").and_then(|v| v.as_str()).unwrap_or("");
            // V66: dead_only is name-exempt (path-scoped dead-code scan).
            let mut dead_only_early = args.get("dead_only").and_then(|v| v.as_bool()).unwrap_or(false);
            let mut methods_early = args.get("methods").and_then(|v| v.as_bool()).unwrap_or(false);
            // V66b: mode-flag-as-name recovery — the model sometimes passes the
            // mode flag name as the symbol ("name": "dead_only"). Treat that as
            // intent: route to the named mode instead of a literal symbol lookup.
            match sym {
                "dead_only" | "find_dead_code" | "dead code" => {
                    dead_only_early = true;
                }
                "methods" | "list_methods" | "list methods" => {
                    methods_early = true;
                }
                _ => {}
            }
            if sym.is_empty() && !dead_only_early { return err_missing_param("name"); }
            let af_raw = args.get("anchor_file").and_then(|v| v.as_str()).unwrap_or("");
            let _af = resolve_af(af_raw);
let al_raw = args.get("anchor_line").and_then(|v| v.as_i64()).unwrap_or(0) as i32; let _al = if al_raw > 0 { al_raw - 1 } else { al_raw };
            // S1: match schema default 0.1
            let _th = args.get("threshold").and_then(|v| v.as_f64()).unwrap_or(0.1) as f32;
            let _k = args.get("window").and_then(|v| v.as_i64()).unwrap_or(5) as i32;
            // V37: def_only replaces goto_def. usage_only replaces call_graph(inbound).
            let def_only = args.get("def_only").and_then(|v| v.as_bool()).unwrap_or(false);
            let usage_only = args.get("usage_only").and_then(|v| v.as_bool()).unwrap_or(false);

            // V38 bug fix: if methods=true or dead_only=true was passed to find_references,
            // route to describe's methods/dead_only handlers instead of silently ignoring.
            // V66b: also honor mode-intent recovered from name-as-flag above.
            let methods = methods_early || args.get("methods").and_then(|v| v.as_bool()).unwrap_or(false);
            if methods {
                return match reliary_search::callgraph_v2::find_methods_on(db, sym) {
                    Ok(mr) => {
                        let text = if mr.methods.is_empty() {
                            format!("No methods found on {}.\n", sym)
                        } else {
                            // V58c: per-method file:line evidence.
                            // V59g: include field type / signature evidence.
                            // V66d: surface the impl block location so the model
                            // can cite "impl at file:line" as a distinct fact.
                            let impl_loc = mr.impl_file.as_ref().map(|f| {
                                format!(" (impl at {}:{})",
                                    std::path::Path::new(f).file_name().map(|x| x.to_string_lossy().to_string()).unwrap_or_else(|| f.clone()),
                                    mr.impl_line.unwrap_or(0) + 1)
                            }).unwrap_or_default();
                            let entries: Vec<String> = mr.methods.iter().take(8)
                                .map(|m| {
                                    let loc = format!("{}:{}",
                                        std::path::Path::new(&m.file).file_name().map(|x| x.to_string_lossy().to_string()).unwrap_or_else(|| m.file.clone()),
                                        m.line + 1);
                                    if m.source.is_empty() { format!("{} ({})", m.name, loc) }
                                    else { format!("{}: {} ({})", m.name, m.source, loc) }
                                })
                                .collect();
                            format!("Methods on {}{}: {}.\n", sym, impl_loc, entries.join(", "))
                        };
                        DispatchResult::Success(serde_json::json!({ "content": [{ "type": "text", "text": text }] }))
                    }
                    Err(e) => err_db(format!("methods_on: {}", e)),
                };
            }
            let dead_only = dead_only_early || args.get("dead_only").and_then(|v| v.as_bool()).unwrap_or(false);
            if dead_only {
                // V66d: accept BOTH `path` and `path_filter` as the scope — the
                // model naturally reaches for path_filter (the general module
                // param) even though dead_only's description says `path`.
                let path = args.get("path").and_then(|v| v.as_str()).unwrap_or(".");
                let pf = args.get("path_filter").and_then(|v| v.as_str()).unwrap_or("");
                let limit = args.get("limit").and_then(|v| v.as_u64()).unwrap_or(100) as usize;
                let functions_only = args.get("functions_only").and_then(|v| v.as_bool()).unwrap_or(true);
                let scope: Option<String> = if !pf.is_empty() {
                    Some(normalize_scope(pf))
                } else {
                    // V66e: scope to the repo's src/ dir when it exists and the
                    // caller asked for the root (path="." or a repo root). This
                    // avoids bench scripts / config files dominating dead-code.
                    let (db2, _) = match open_symbol_index(path) {
                        Ok(t) => t,
                        Err(r) => return r,
                    };
                    let has_src = db2.query_row(
                        "SELECT EXISTS(SELECT 1 FROM file_map WHERE file_path LIKE '%/src/%' LIMIT 1)",
                        [], |r| r.get::<_, i64>(0),
                    ).unwrap_or(0) > 0;
                    let root_like = path == "." || !path.contains("src");
                    if has_src && root_like {
                        let base = if path == "." { "" } else { path.trim_end_matches('/') };
                        Some(normalize_scope(&format!("{}/src", base)))
                    } else if path != "." {
                        Some(normalize_scope(path))
                    } else {
                        None
                    }
                };
                let path_filter = scope.as_deref();
                return match reliary_search::symbol::dead_symbols(db, limit, path_filter, functions_only) {
                    Ok(dead) => {
                        let mut text = String::new();
                        for (_stem, file, line, _col) in dead.iter() {
                            // V66e: show corpus-relative path (strip the absolute
                            // mount) so the model can verify the scope, e.g.
                            // "src/search.rs:187" not the bare "search.rs:187".
                            let rel = corpus_rel_path(file);
                            text.push_str(&format!("{}:{}\n", rel, line + 1));
                            if let Some(meta) = reliary_search::file_meta::get(file) {
                                if let Some(src_line) = meta.lines.get(*line as usize) {
                                    text.push_str(&format!("    {}\n", src_line));
                                }
                            }
                        }
                        if dead.is_empty() { text.push_str("(no dead code found)\n"); }
                        DispatchResult::Success(serde_json::json!({ "content": [{ "type": "text", "text": text }] }))
                    }
                    Err(e) => err_db(format!("dead_symbols: {}", e)),
                };
            }

            // V39: Single direct SQL query on the occurrence table.
            // Replaces the 13-path pipeline (pattern_hybrid → type_flow auto → type_flow fallback →
            // centrality ranking → top_candidate_definitions → etc). The occurrence table was
            // built during trust by scan_identifiers + classify_structural — it already has
            // all identifier locations with is_def/tag/block_id annotations. One query.
            let path_filter = args.get("path_filter").and_then(|v| v.as_str()).unwrap_or("").to_string();
            // V53: file_only → return only distinct file paths, no per-hit details.
            // Cheaper than full references for "which files use X" queries.
            let file_only = args.get("file_only").and_then(|v| v.as_bool()).unwrap_or(false);

            // Resolve phrase_id via stem_identifier (preserves snake_case: classify_structural
            // stays classify_structural, not stemmed to classifi).
let sym_stemmed = reliary_search::stem_identifier(sym);
let phrase_id = reliary_search::symbol::phrase_id_for(db, &sym_stemmed).ok().flatten()
    .or_else(|| reliary_search::symbol::phrase_id_for(db, sym).ok().flatten());

            // Direct occurrence query — ORDER BY is_def DESC ranks definitions first.
            // V39: add `f.file_path LIKE '%.rs'` filter to prefer Rust source files
            // over markdown/JSON files that mention the symbol in prose.
            let hits: Vec<reliary_search::symbol::OccHit> = if let Some(pid) = phrase_id {
                // Always use LIKE ?2 pattern — when no filter, pass "%" for match-all.
                let pattern = if path_filter.is_empty() {
                    "%".to_string()
                } else {
                    // V59: file_map stores ABSOLUTE paths; a relative filter
                    // needs a LEADING % or LIKE anchors at position 0 and
                    // silently returns 0 hits (the "No definition found" bug).
                    // Match absolute paths containing "/<filter>" as a segment.
                    format!("%/{}%", path_filter.trim_end_matches('/'))
                };
                // V39: ORDER BY clause prefers .rs files when is_def=1 ties exist.
                // This avoids markdown plans/READMEs ranking above Rust source code.
                // V59 A3: also prefer crates/ source trees over bench/scripts dirs —
                // a Rust symbol name often collides with a Python wrapper of the
                // same name in the bench harness.
                let sql = "SELECT f.file_path, o.line, o.col, o.is_def, o.tag
                     FROM occurrence o JOIN file_map f ON f.id = o.file_id
                     WHERE o.phrase_id = ?1 AND f.file_path LIKE ?2 AND f.is_source = 1
                     ORDER BY o.is_def DESC,
                              (CASE WHEN f.file_path LIKE '%.rs' THEN 0 ELSE 1 END),
                              (CASE WHEN f.file_path LIKE '%/crates/%' THEN 0 ELSE 1 END),
                              (CASE WHEN f.file_path LIKE '%/bench/%' THEN 1 ELSE 0 END),
                              (CASE WHEN f.file_path LIKE '%/tests/%' THEN 1 ELSE 0 END),
                              o.line ASC
                     LIMIT 50";
                let mut stmt = match db.prepare_cached(sql) {
                    Ok(s) => s,
                    Err(e) => {
                        eprintln!("[V39] occurrence query prepare failed: {}", e);
                        return err_db(format!("prepare: {}", e));
                    }
                };
                let mapped = stmt.query_map(params![pid, &pattern], |r| {
                    Ok(reliary_search::symbol::OccHit {
                        occ_id: 0,
                        file_id: 0,
                        file_path: r.get(0)?,
                        line: r.get(1)?,
                        col: r.get(2)?,
                        is_def: r.get::<_, i64>(3)? != 0,
                        block_id: 0,
                        similarity: 1.0,
                    })
                });
                match mapped {
                    Ok(rows) => rows.filter_map(|r| r.ok()).collect(),
                    Err(e) => {
                        eprintln!("[V39] occurrence query failed: {}", e);
                        Vec::new()
                    }
                }
            } else {
                Vec::new()
            };
            let _last_err = String::new();

            let wd = cached_cwd().to_string_lossy().to_string();

            // V40: One-line answer format. No hits list, no candidates, no headers.
            // The tool IS the answer. The model copies it verbatim.

            // V53: file_only → return only distinct file paths, no per-hit details.
            // Cheaper than full references for "which files use X" queries.
            // V60: honor file_only even when path_filter is set (filter the
            // distinct query by the filter instead of dropping the flag).
            if file_only && !def_only && !usage_only {
                if let Some(pid) = phrase_id {
                    let file_pattern = if path_filter.is_empty() {
                        "%".to_string()
                    } else {
                        format!("%{}%", path_filter)
                    };
                    let distinct_sql = "SELECT DISTINCT f.file_path
                         FROM occurrence o JOIN file_map f ON f.id = o.file_id
                         WHERE o.phrase_id = ?1 AND f.file_path LIKE ?2 AND f.is_source = 1
                         ORDER BY f.file_path
                         LIMIT 30";
                    if let Ok(mut stmt) = db.prepare_cached(distinct_sql) {
                        let file_paths: Vec<String> = stmt
                            .query_map(params![pid, &file_pattern], |r| r.get::<_, String>(0))
                            .map(|rows| rows.filter_map(|r| r.ok()).collect())
                            .unwrap_or_default();
                        if file_paths.is_empty() {
                            return DispatchResult::Success(serde_json::json!({
                                "content": [{ "type": "text", "text":
                                    format!("{} is not used in any file.\n", sym) }]
                            }));
                        }
                        let short_names: Vec<String> = file_paths.iter().map(|p| {
                            std::path::Path::new(p).file_name()
                                .map(|x| x.to_string_lossy().to_string())
                                .unwrap_or_else(|| p.clone())
                        }).collect();
                        let text = format!("{} is used in {} files: {}\n",
                            sym, file_paths.len(), short_names.join(", "));
                        return DispatchResult::Success(serde_json::json!({
                            "content": [{ "type": "text", "text": text }]
                        }));
                    }
                }
            }

            // V40: def_only → one-line definition answer
// V50: Empty def_only → return informational success (Claude Context pattern)
// Prevents dead-end retry loops by treating "no results" as a final answer
// with helpful suggestions instead of an error.
if def_only {
    if let Some(h) = hits.iter().find(|h| h.is_def) {
        let obj = hit_with_qualified_name(&h.file_path, h.line, h.col, &wd);
        let qn = obj["qualified_name"].as_str().unwrap_or(sym);
        let f = obj["file"].as_str().unwrap_or("");
        let ln = obj["line"].as_i64().unwrap_or(0);
        let f_short = std::path::Path::new(f).file_name().map(|x| x.to_string_lossy().to_string()).unwrap_or_else(|| f.to_string());
        let code_evidence = reliary_search::file_meta::get(&h.file_path)
            .and_then(|meta| meta.lines.get(h.line as usize).cloned())
            .map(|s| format!("\n{}:{}    {}", f_short, ln, s.trim()))
            .unwrap_or_default();
        let text = format!("{} is defined at {}:{}{}\n", qn, f_short, ln, code_evidence);
        return DispatchResult::Success(serde_json::json!({
            "content": [{ "type": "text", "text": text }]
        }));
    }
    // V59: no is_def rows yet → try JIT + centrality before giving up.
    // The occurrence table builds lazily; a fresh index has zero rows until
    // the first query triggers ensure_occurrence_for_phrase.
    if let Some((fp, line, _)) = reliary_search::type_flow::top_candidate_definitions(db, sym).into_iter().next() {
        let f_short = std::path::Path::new(&fp).file_name()
            .map(|x| x.to_string_lossy().to_string()).unwrap_or_else(|| fp.clone());
        let code_evidence = reliary_search::file_meta::get(&fp)
            .and_then(|meta| meta.lines.get(line as usize).cloned())
            .map(|s| format!("\n{}:{}    {}", f_short, line + 1, s.trim()))
            .unwrap_or_default();
        let text = format!("{} is defined at {}:{}{}\n", sym, f_short, line + 1, code_evidence);
        return DispatchResult::Success(serde_json::json!({
            "content": [{ "type": "text", "text": text }]
        }));
    }
    // V59 B1: maybe it's a trait being implemented — return implementors.
    let impls = reliary_search::callgraph_v2::find_trait_impls(db, sym);
    if !impls.is_empty() {
        let items: Vec<String> = impls.iter().take(8)
            .map(|i| format!("{} ({}:{})", i.type_name,
                std::path::Path::new(&i.file).file_name().map(|x| x.to_string_lossy().to_string()).unwrap_or_else(|| i.file.clone()),
                i.line))
            .collect();
        let text = format!("{} is implemented by {} types: {}.\n", sym, impls.len(), items.join(", "));
        return DispatchResult::Success(serde_json::json!({
            "content": [{ "type": "text", "text": text }]
        }));
    }
    // Still nothing — suggest closest symbols (prevents retry loop)
    let suggestions = closest_symbols(db, sym, 5);
    let text = if suggestions.is_empty() {
        format!("No definition found for \"{}\" in this index.\n", sym)
    } else {
        format!("No definition found for \"{}\". Closest symbols in this index: {}. Try one of these with the same tool.\n",
            sym, suggestions.join(", "))
    };
    return DispatchResult::Success(serde_json::json!({
        "content": [{ "type": "text", "text": text }]
    }));
}

            // V40: usage_only → one-line callers answer (max 5, test files excluded)
            // V62: judge flagged incomplete caller sets (missing
            // lazy_occurrence.rs, scope_types.rs) — the .take(5) truncated
            // the full caller list. Bump to 12; still compact.
            if usage_only {
                let callers: Vec<_> = hits.iter()
                    .filter(|h| !h.is_def)
                    .filter(|h| {
                        let fp = &h.file_path;
                        !fp.contains("/tests/") && !fp.contains("/test/") && !fp.contains("/examples/") && !fp.contains("/benches/")
                        // V66c: bench dirs with or without leading slash (relative paths)
                        && !fp.contains("/bench/") && !fp.starts_with("bench/") && !fp.starts_with("/bench/")
                    })
                    .take(12)
                    .collect();
                if callers.is_empty() {
                    // V59 B1b + V59e guard: trait fallback only for TYPE
                    // names — lowercase method queries must not get impls.
                    let looks_type_t = sym.chars().next()
                        .map(|c| c.is_ascii_uppercase()).unwrap_or(false);
                    let impls = if looks_type_t {
                        reliary_search::callgraph_v2::find_trait_impls(db, sym)
                    } else { Vec::new() };
                    if !impls.is_empty() {
                        let items: Vec<String> = impls.iter().take(8)
                            .map(|i| format!("{} ({}:{})", i.type_name,
                                std::path::Path::new(&i.file).file_name().map(|x| x.to_string_lossy().to_string()).unwrap_or_else(|| i.file.clone()),
                                i.line))
                            .collect();
                        let text = format!("{} is implemented by {} types: {}.\n", sym, impls.len(), items.join(", "));
                        return DispatchResult::Success(serde_json::json!({
                            "content": [{ "type": "text", "text": text }]
                        }));
                    }
                    // V50: Informational success — prevents dead-end retry
                    let suggestions = closest_symbols(db, sym, 5);
                    let text = if suggestions.is_empty() {
                        format!("No call sites found for \"{}\" in non-test code.\n", sym)
                    } else {
                        format!("No call sites found for \"{}\" in non-test code. Did you mean: {}?\n",
                            sym, suggestions.join(", "))
                    };
                    return DispatchResult::Success(serde_json::json!({
                        "content": [{ "type": "text", "text": text }]
                    }));
                }
                // V59l: include the ENCLOSING FUNCTION for each caller —
                // "file:line in fn foo()" is what "who calls X" answers need.
                let parts: Vec<String> = callers.iter().map(|h| {
                    let f_short = std::path::Path::new(&h.file_path).file_name().map(|x| x.to_string_lossy().to_string()).unwrap_or_else(|| h.file_path.clone());
                    let enc = reliary_search::file_meta::get(&h.file_path)
                        .and_then(|meta| meta.fn_names.get(h.line.max(0) as usize).cloned())
                        .filter(|s| !s.is_empty())
                        .map(|s| format!(" in fn {}", s))
                        .unwrap_or_default();
                    format!("{}:{}{}", f_short, h.line + 1, enc)
                }).collect();
                // V42: add raw code evidence from first caller site
                let first_code = callers.first().and_then(|h| {
                    reliary_search::file_meta::get(&h.file_path)
                        .and_then(|meta| meta.lines.get(h.line as usize).cloned())
                });
                let code_evidence = first_code.map(|s| format!("\n  {}", s.trim())).unwrap_or_default();
                let text = format!("{} is called from {}{}\n", sym, parts.join(", "), code_evidence);
                return DispatchResult::Success(serde_json::json!({
                    "content": [{ "type": "text", "text": text }]
                }));
            }

            // V40: path_filter → one-line implementations answer (max 5, deduped by qualified_name)
            if !path_filter.is_empty() {
                let module = path_filter.trim_end_matches('/');
                let sym_lower = sym.to_ascii_lowercase();
                let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
                let mut parts: Vec<(String, String, i32)> = Vec::new();
                for h in hits.iter() {
                    if parts.len() >= 5 { break; }
                    let obj = hit_with_qualified_name(&h.file_path, h.line, h.col, &wd);
                    let qn = obj["qualified_name"].as_str().unwrap_or("").to_string();
                    if qn.is_empty() { continue; }
                    if !qn.to_ascii_lowercase().contains(&sym_lower) { continue; }
                    if !seen.insert(qn.clone()) { continue; }
                    let f = obj["file"].as_str().unwrap_or("");
                    let ln = obj["line"].as_i64().unwrap_or(0);
                    let f_short = std::path::Path::new(f).file_name().map(|x| x.to_string_lossy().to_string()).unwrap_or_else(|| f.to_string());
                    parts.push((qn, f_short, ln as i32));
                }
                if parts.is_empty() {
                    let text = format!("No implementations of \"{}\" found in {}\n", sym, module);
                    return DispatchResult::Success(serde_json::json!({
                        "content": [{ "type": "text", "text": text }]
                    }));
                }
                if parts.is_empty() {
                    let text = format!("No implementations of \"{}\" found in {}\n", sym, module);
                    return DispatchResult::Success(serde_json::json!({
                        "content": [{ "type": "text", "text": text }]
                    }));
                }
                let summary: Vec<String> = parts.iter().map(|(qn, f, ln)| format!("{} at {}:{}", qn, f, ln)).collect();
                // V42: add raw code as evidence — show first hit's source line
                let first_evidence = parts.first().and_then(|(_, f, ln)| {
                    reliary_search::file_meta::get(f)
                        .or_else(|| {
                            // Look up by absolute path
                            let _abs = std::path::Path::new(f).to_string_lossy().to_string();
                            None
                        })
                        .and_then(|meta| meta.lines.get(*ln as usize).cloned())
                });
                let code_evidence = first_evidence.map(|s| format!("\n  {}", s.trim())).unwrap_or_default();
                let text = format!("Implementations of {} in {}: {}{}\n", sym, module, summary.join(", "), code_evidence);
                return DispatchResult::Success(serde_json::json!({
                    "content": [{ "type": "text", "text": text }]
                }));
            }

            // V40: default mode → one-line definition + top 5 usages
            let def_hit = hits.iter().find(|h| h.is_def);
            let mut out = String::with_capacity(256);
            if let Some(h) = def_hit {
                let obj = hit_with_qualified_name(&h.file_path, h.line, h.col, &wd);
                let qn = obj["qualified_name"].as_str().unwrap_or(sym);
                let f = obj["file"].as_str().unwrap_or("");
                let ln = obj["line"].as_i64().unwrap_or(0);
                let f_short = std::path::Path::new(f).file_name().map(|x| x.to_string_lossy().to_string()).unwrap_or_else(|| f.to_string());
                out.push_str(&format!("{} is defined at {}:{}. ", qn, f_short, ln));
            } else {
                // V59 B1: trait? return implementors instead of a dead end.
                let impls = reliary_search::callgraph_v2::find_trait_impls(db, sym);
                if !impls.is_empty() {
                    let items: Vec<String> = impls.iter().take(8)
                        .map(|i| format!("{} ({}:{})", i.type_name,
                            std::path::Path::new(&i.file).file_name().map(|x| x.to_string_lossy().to_string()).unwrap_or_else(|| i.file.clone()),
                            i.line))
                        .collect();
                    out.push_str(&format!("{} is implemented by {} types: {}. ", sym, impls.len(), items.join(", ")));
                } else {
                    out.push_str(&format!("No definition found for \"{}\". ", sym));
                }
            }
            let usages: Vec<_> = hits.iter().filter(|h| !h.is_def).take(5).collect();
            if !usages.is_empty() {
                let parts: Vec<String> = usages.iter().map(|h| {
                    let f_short = std::path::Path::new(&h.file_path).file_name().map(|x| x.to_string_lossy().to_string()).unwrap_or_else(|| h.file_path.clone());
                    format!("{}:{}", f_short, h.line + 1)
                }).collect();
                out.push_str(&format!("Used at {}", parts.join(", ")));
            }
            // V42: add raw code evidence for the definition
            if let Some(h) = def_hit {
                if let Some(meta) = reliary_search::file_meta::get(&h.file_path) {
                    if let Some(src) = meta.lines.get(h.line as usize) {
                        out.push_str(&format!("\n  {}", src.trim()));
                    }
                }
            }
            out.push('\n');
            DispatchResult::Success(serde_json::json!({
                "content": [{ "type": "text", "text": out }]
            }))
        }
        "reliary_goto_def" => {
            let sym = args.get("name").and_then(|v| v.as_str()).unwrap_or("");
            if sym.is_empty() { return err_missing_param("name"); }
            let af_raw = args.get("anchor_file").and_then(|v| v.as_str()).unwrap_or("");
            let af = resolve_af(af_raw);
let al_raw = args.get("anchor_line").and_then(|v| v.as_i64()).unwrap_or(0) as i32; let al = if al_raw > 0 { al_raw - 1 } else { al_raw };
            match reliary_search::symbol::goto_def(db, sym, &af, al) {
                Ok(Some(hit)) => {
                    let wd = cached_cwd().to_string_lossy().to_string();
                    let hit_obj = hit_with_qualified_name(&hit.file_path, hit.line, hit.col, &wd);
                    let qn = hit_obj["qualified_name"].as_str().unwrap_or(sym);
                    let f = hit_obj["file"].as_str().unwrap_or("");
                    let ln = hit_obj["line"].as_i64().unwrap_or(0);
                    let f_short = std::path::Path::new(f)
                        .file_name()
                        .map(|x| x.to_string_lossy().to_string())
                        .unwrap_or_else(|| f.to_string());
                    // V40: One-line answer format.
                    let text = format!("{} is defined at {}:{}\n", qn, f_short, ln);
                    DispatchResult::Success(serde_json::json!({
                        "content": [{ "type": "text", "text": text }]
                    }))
                }
                Ok(None) => DispatchResult::Success(serde_json::json!({
                    "content": [{ "type": "text", "text": serde_json::json!({
                        "name": sym, "found": false,
                        "hint": "Try reliary_search to find the file, then reliary_goto_def with the correct anchor."
                    }).to_string() }]
                })),
                Err(e) => err_db(format!("goto_def: {}", e)),
            }
        }
        "reliary_callgraph" => {
            let sym = args.get("name").and_then(|v| v.as_str()).unwrap_or("");
            if sym.is_empty() { return err_missing_param("name"); }
            let af_raw = args.get("anchor_file").and_then(|v| v.as_str()).unwrap_or("");
            let af = resolve_af(af_raw);
let al_raw = args.get("anchor_line").and_then(|v| v.as_i64()).unwrap_or(0) as i32; let al = if al_raw > 0 { al_raw - 1 } else { al_raw };
            // V13: treat empty anchor_file or line=0 as "no anchor" so
            // find_definition auto-resolves instead of trusting a bogus anchor.
            let anchor = if af.is_empty() || al == 0 {
                None
            } else {
                Some((af.clone(), al))
            };
            // V13: delegate to callgraph_v2 for Fix C + D (anchor validation, depth-2 expansion)
            let depth = delegate_depth(sym);
            match reliary_search::callgraph_v2::build_call_graph(db, sym, ".", anchor.clone(), depth) {
                Ok(cg) => {
                    // If the qualified name returned empty, try the unqualified last component.
                    if cg.callees.is_empty() && cg.callers.is_empty() && sym.contains("::") {
                        let last = sym.rsplit("::").next().unwrap_or(sym);
                        if let Ok(cg2) = reliary_search::callgraph_v2::build_call_graph(db, last, ".", anchor, delegate_depth(last)) {
                            let caller_names: Vec<&str> = cg2.callers.iter().map(|c| c.name.as_str()).collect();
                            // V64: callees include their definition site so answers can cite file:line.
                            let callee_list: Vec<String> = cg2.callees.iter().map(|c| {
                                match (&c.def_file, &c.def_line) {
                                    (Some(f), Some(l)) => format!("{} ({}:{})", c.name,
                                        std::path::Path::new(f).file_name().map(|x| x.to_string_lossy().to_string()).unwrap_or_else(|| f.clone()), l + 1),
                                    _ => c.name.clone(),
                                }
                            }).collect();
                            // Phase 2-4: compact format
                            let text = format!(
                                "callers({}): {}\ncallees({}): {}",
                                caller_names.len(), caller_names.join(" "),
                                callee_list.len(), callee_list.join(", "),
                            );
                            return DispatchResult::Success(serde_json::json!({
                                "content": [{ "type": "text", "text": text }]
                            }));
                        }
                    }
                    let caller_names: Vec<&str> = cg.callers.iter().map(|c| c.name.as_str()).collect();
                    // V64: callees include their definition site so answers can cite file:line.
                    let callee_list: Vec<String> = cg.callees.iter().map(|c| {
                        match (&c.def_file, &c.def_line) {
                            (Some(f), Some(l)) => format!("{} ({}:{})", c.name,
                                std::path::Path::new(f).file_name().map(|x| x.to_string_lossy().to_string()).unwrap_or_else(|| f.clone()), l + 1),
                            _ => c.name.clone(),
                        }
                    }).collect();
                    // Phase 6: add hint when empty
                    let text = if caller_names.is_empty() && callee_list.is_empty() {
                        format!(
                            "callers(0)\ncallees(0)\nhint: try reliary_goto_def(\"{}\") first, then pass anchor_file+anchor_line here.",
                            sym
                        )
                    } else {
                        format!(
                            "callers({}): {}\ncallees({}): {}",
                            caller_names.len(), caller_names.join(" "),
                            callee_list.len(), callee_list.join(", "),
                        )
                    };
                    DispatchResult::Success(serde_json::json!({
                        "content": [{ "type": "text", "text": text }]
                    }))
                }
                Err(e) => err_db(format!("callgraph: {}", e)),
            }
        }
        "reliary_callgraph_v2" => {
            // Arc 42 Phase B: grammar-free body extraction.
            let sym = args.get("name").and_then(|v| v.as_str()).unwrap_or("");
            if sym.is_empty() { return err_missing_param("name"); }
            let path = args.get("path").and_then(|v| v.as_str()).unwrap_or(".");
            let summary = args.get("summary").and_then(|v| v.as_bool()).unwrap_or(false);
            // W1: optional anchor from reliary_goto_def to disambiguate same-named symbols.
            let anchor_file = args.get("anchor_file").and_then(|v| v.as_str()).unwrap_or("");
            let anchor_line = args.get("anchor_line").and_then(|v| v.as_i64()).unwrap_or(0) as i32;
            let anchor = if anchor_file.is_empty() {
                None
            } else {
                Some((anchor_file.to_string(), anchor_line))
            };
            let depth_v2 = delegate_depth(sym);
            match reliary_search::callgraph_v2::build_call_graph(db, sym, path, anchor, depth_v2) {
                Ok(mut cg) => {
                    // Phase 1: strip whitespace from source fields (cache-safe cost reduction).
                    cg.source_preview = cg.source_preview.trim().to_string();
                    if cg.source_preview.len() > 80 {
                        let end = cg.source_preview.floor_char_boundary(80);
                        cg.source_preview = format!("{}...", &cg.source_preview[..end]);
                    }
                    cg.callers.truncate(5);
                    // V66d: raise callee cap 8→15 — a function calling 13 helpers
                    // was truncated, hiding real callees from the model. General
                    // completeness; adds ~7 short entries worst case.
                    let callee_limit = if delegate_depth(sym) > 1 { 20 } else { 15 };
                    cg.callees.truncate(callee_limit);
                    for c in cg.callers.iter_mut() {
                        c.source = c.source.trim().to_string();
                        if c.source.len() > 80 {
                            let end = c.source.floor_char_boundary(80);
                            c.source = format!("{}...", &c.source[..end]);
                        }
                    }
                    for c in cg.callees.iter_mut() {
                        c.source = c.source.trim().to_string();
                        if c.source.len() > 80 {
                            let end = c.source.floor_char_boundary(80);
                            c.source = format!("{}...", &c.source[..end]);
                        }
                    }
                    let text = if summary {
                        // V40: One-line call_graph summary
                        let callers_str = if cg.callers.is_empty() {
                            "none".to_string()
                        } else {
                            cg.callers.iter().take(8).map(|c| format!("{}:{}", c.file.rsplit('/').next().unwrap_or("?"), c.line)).collect::<Vec<_>>().join(", ")
                        };
                        let callees_str = if cg.callees.is_empty() {
                            "none".to_string()
                        } else {
                            // V64: include callee def sites so answers can cite file:line.
                            // W4: include a truncated one-line signature per callee so
                            // "what do these helpers do" is answerable without N describe calls.
                            cg.callees.iter().take(10).map(|c| match (&c.def_file, &c.def_line) {
                                (Some(f), Some(l)) => {
                                    let f_short = std::path::Path::new(f).file_name().map(|x| x.to_string_lossy().to_string()).unwrap_or_else(|| f.clone());
                                    let sig = callee_signature(f, *l);
                                    if sig.is_empty() { format!("{} ({}:{})", c.name, f_short, l + 1) }
                                    else { format!("{} ({}:{}) {}", c.name, f_short, l + 1, sig) }
                                }
                                _ => c.name.clone(),
                            }).collect::<Vec<_>>().join(", ")
                        };
                        format!("{} is called by: {}. {} calls: {}.\n", cg.anchor_name, callers_str, cg.anchor_name, callees_str)
                    } else {
                        // V40: One-line call_graph (non-summary)
                        let callees_str: Vec<String> = cg.callees.iter().take(10).map(|c| match (&c.def_file, &c.def_line) {
                            (Some(f), Some(l)) => {
                                let f_short = std::path::Path::new(f).file_name().map(|x| x.to_string_lossy().to_string()).unwrap_or_else(|| f.clone());
                                let sig = callee_signature(f, *l);
                                if sig.is_empty() { format!("{} ({}:{})", c.name, f_short, l + 1) }
                                else { format!("{} ({}:{}) {}", c.name, f_short, l + 1, sig) }
                            }
                            _ => c.name.clone(),
                        }).collect();
                        let callers_str: Vec<String> = cg.callers.iter().take(8).map(|c| format!("{}:{}", c.file.rsplit('/').next().unwrap_or("?"), c.line)).collect();
                        format!("{} calls: {}. {} is called by: {}.\n",
                            cg.anchor_name,
                            if callees_str.is_empty() { "none".to_string() } else { callees_str.join(", ") },
                            cg.anchor_name,
                            if callers_str.is_empty() { "none".to_string() } else { callers_str.join(", ") })
                    };
                    DispatchResult::Success(serde_json::json!({
                        "content": [{ "type": "text", "text": text }]
                    }))
                }
                Err(e) => err_db(format!("callgraph_v2: {}", e)),
            }
        }
        "reliary_methods_on" => {
            // V40: One-line methods output
            let type_name = args.get("name").and_then(|v| v.as_str()).unwrap_or("");
            match reliary_search::callgraph_v2::find_methods_on(db, type_name) {
                Ok(mr) => {
                    let text = if mr.methods.is_empty() {
                        format!("No methods found on {}.\n", type_name)
                    } else {
                        // V58c: per-method file:line — the model needs the
                        // location evidence, not just names.
                        let entries: Vec<String> = mr.methods.iter().take(8)
                            .map(|m| format!("{} ({}:{})", m.name,
                                std::path::Path::new(&m.file).file_name().map(|x| x.to_string_lossy().to_string()).unwrap_or_else(|| m.file.clone()),
                                m.line + 1))
                            .collect();
                        format!("Methods on {}: {}.\n", type_name, entries.join(", "))
                    };
                    DispatchResult::Success(serde_json::json!({
                        "content": [{ "type": "text", "text": text }]
                    }))
                }
                Err(e) => err_db(format!("methods_on: {}", e)),
            }
        }
        "reliary_scope" => {
            let sym = args.get("name").and_then(|v| v.as_str()).unwrap_or("");
            let af_raw = args.get("anchor_file").and_then(|v| v.as_str()).unwrap_or("");
            let af = resolve_af(af_raw);
let al_raw = args.get("anchor_line").and_then(|v| v.as_i64()).unwrap_or(0) as i32; let al = if al_raw > 0 { al_raw - 1 } else { al_raw };
            match reliary_search::symbol::scope(db, sym, &af, al) {
                Ok(Some((lo, hi, cnt))) => DispatchResult::Success(serde_json::json!({
                    "content": [{ "type": "text", "text": serde_json::json!({
                        "name": sym, "min_line": lo + 1, "max_line": hi + 1, "count": cnt
                    }).to_string() }]
                })),
                Ok(None) => DispatchResult::Success(serde_json::json!({
                    "content": [{ "type": "text", "text": serde_json::json!({
                        "name": sym, "in_scope": false
                    }).to_string() }]
                })),
                Err(e) => err_db(format!("scope: {}", e)),
            }
        }
        "reliary_dead_symbols" => {
            let limit = args.get("limit").and_then(|v| v.as_u64()).unwrap_or(100) as usize;
            let path_filter = args.get("path").and_then(|v| v.as_str()).filter(|s| !s.is_empty() && *s != ".");
            let functions_only = args.get("functions_only").and_then(|v| v.as_bool()).unwrap_or(true);
            match reliary_search::symbol::dead_symbols(db, limit, path_filter, functions_only) {
                Ok(dead) => {
                    // V35: Flat text output — same fix as find_references V25.
                    let mut out = String::with_capacity(1024);
                    out.push_str(&format!("The answer is: {} dead items in {}:\n", dead.len(), path_filter.unwrap_or(".")));
                    for (stem, file, line, _col) in &dead {
                        let f_short = file.rsplit('/').next().unwrap_or(file);
                        out.push_str(&format!("{} at {}:{}\n", stem, f_short, line + 1));
                    }
                    DispatchResult::Success(serde_json::json!({
                        "content": [{ "type": "text", "text": out }]
                    }))
                }
                Err(e) => err_db(format!("dead_symbols: {}", e)),
            }
        }
        "reliary_find_references_type_flow" => {
            let sym = args.get("name").and_then(|v| v.as_str()).unwrap_or("");
            if sym.is_empty() { return err_missing_param("name"); }
            let af_raw = args.get("anchor_file").and_then(|v| v.as_str()).unwrap_or("");
            let af = resolve_af(af_raw);
let al_raw = args.get("anchor_line").and_then(|v| v.as_i64()).unwrap_or(0) as i32; let al = if al_raw > 0 { al_raw - 1 } else { al_raw };
            let th = args.get("threshold").and_then(|v| v.as_f64()).unwrap_or(0.3) as f32;
            // Arc 42 Phase A: auto-anchor + dead-end fallback.
            let mut hits = reliary_search::type_flow::find_references_type_flow(db, sym, &af, al, th).unwrap_or_default();
            if hits.is_empty() || af.is_empty() {
                let auto = match reliary_search::type_flow::find_references_auto(db, sym, th) { Ok(v) => v, Err(e) => { eprintln!("find_references_auto: {}", e); Vec::new() } };
                if !auto.is_empty() {
                    hits = auto;
                } else {
                    hits = match reliary_search::type_flow::find_references_fallback(db, sym, 50) { Ok(v) => v, Err(e) => { eprintln!("find_references_fallback: {}", e); Vec::new() } };
                }
            }
            {
                let wd = cached_cwd().to_string_lossy().to_string();
                let arr: Vec<_> = hits.iter().map(|h| {
                    let mut obj = hit_with_qualified_name(&h.file_path, h.line, h.col, &wd);
                    obj["is_def"] = serde_json::json!(h.is_def);
                    obj["similarity"] = serde_json::json!(h.similarity);
                    obj
                }).collect();
                DispatchResult::Success(serde_json::json!({
                    "content": [{ "type": "text", "text": serde_json::json!({
                        "name": sym, "anchor_file": af, "anchor_line": al,
                        "threshold": th, "method": "type_flow", "count": hits.len(), "hits": arr
                    }).to_string() }]
                }))
            }
        }
        "reliary_find_references_with_source" => {
            let sym = args.get("name").and_then(|v| v.as_str()).unwrap_or("");
            // V59l: dead_only needs no name — route before the name check.
            if sym.is_empty() && !args.get("dead_only").and_then(|v| v.as_bool()).unwrap_or(false) {
                return err_missing_param("name");
            }

            // V59l: dead_only → dead-symbols scan (no name needed).
            if args.get("dead_only").and_then(|v| v.as_bool()).unwrap_or(false) {
                let dpath = args.get("path_filter").and_then(|v| v.as_str())
                    .filter(|s| !s.is_empty())
                    .or_else(|| args.get("path").and_then(|v| v.as_str()).filter(|s| !s.is_empty() && *s != "."))
                    .unwrap_or("");
                let limit = args.get("limit").and_then(|v| v.as_i64()).unwrap_or(30) as usize;
                let functions_only = args.get("functions_only").and_then(|v| v.as_bool()).unwrap_or(true);
                let pf: Option<&str> = if dpath == "." || dpath.is_empty() { None } else { Some(dpath) };
                let dir_label = if dpath.is_empty() { "." } else { dpath };
                return match reliary_search::symbol::dead_symbols(db, limit, pf, functions_only) {
                    Ok(items) => {
                        // dead_symbols returns (phrase_text, file_path, line0, col)
                        let text = if items.is_empty() {
                            format!("No dead code found under {}.\n", dir_label)
                        } else {
                            let lines: Vec<String> = items.iter().take(limit)
                                .map(|(name, fp, line0, _col)| format!("{} at {}:{} (0 cross-file refs)", name,
                                    std::path::Path::new(fp).file_name().map(|x| x.to_string_lossy().to_string()).unwrap_or_else(|| fp.clone()),
                                    line0 + 1))
                                .collect();
                            format!("Dead code in {}: {} items. {}\n", dir_label, items.len(), lines.join("; "))
                        };
                        DispatchResult::Success(serde_json::json!({ "content": [{ "type": "text", "text": text }] }))
                    }
                    Err(e) => err_db(format!("dead_symbols: {}", e)),
                };
            }

            // V57: forward methods/dead_only routing (same as find_references).
            if args.get("methods").and_then(|v| v.as_bool()).unwrap_or(false) {
                return match reliary_search::callgraph_v2::find_methods_on(db, sym) {
                    Ok(mr) => {
                        let text = if mr.methods.is_empty() {
                            format!("No methods found on {}.\n", sym)
                        } else {
                            // V58c: per-method file:line evidence.
                            // V59g: include field type / signature evidence.
                            let entries: Vec<String> = mr.methods.iter().take(8)
                                .map(|m| {
                                    let loc = format!("{}:{}",
                                        std::path::Path::new(&m.file).file_name().map(|x| x.to_string_lossy().to_string()).unwrap_or_else(|| m.file.clone()),
                                        m.line + 1);
                                    if m.source.is_empty() { format!("{} ({})", m.name, loc) }
                                    else { format!("{}: {} ({})", m.name, m.source, loc) }
                                })
                                .collect();
                            format!("Methods on {}: {}.\n", sym, entries.join(", "))
                        };
                        DispatchResult::Success(serde_json::json!({ "content": [{ "type": "text", "text": text }] }))
                    }
                    Err(e) => err_db(format!("methods_on: {}", e)),
                };
            }
            if args.get("def_only").and_then(|v| v.as_bool()).unwrap_or(false) {
                return match reliary_search::type_flow::top_candidate_definitions(db, sym).into_iter().next() {
                    Some((fp, line, _)) => {
                        let f_short = std::path::Path::new(&fp).file_name()
                            .map(|x| x.to_string_lossy().to_string()).unwrap_or(fp.clone());
                        let text = format!("{} is defined at {}:{}\n", sym, f_short, line + 1);
                        DispatchResult::Success(serde_json::json!({ "content": [{ "type": "text", "text": text }] }))
                    }
                    None => {
                        // V57: same closest-symbol recovery as find_references.
                        let suggestions = closest_symbols(db, sym, 5);
                        let text = if suggestions.is_empty() {
                            format!("No definition found for \"{}\" in this index.\n", sym)
                        } else {
                            format!("No definition found for \"{}\". Closest symbols in this index: {}. Try one of these with the same tool.\n",
                                sym, suggestions.join(", "))
                        };
                        DispatchResult::Success(serde_json::json!({
                            "content": [{ "type": "text", "text": text }]
                        }))
                    }
                };
            }

            // V59 B1c: usage_only routes through the shared caller logic
            // (trait fallback included) instead of falling into type_flow,
            // which returns raw JSON with zero hits for keywords like
            // "Default" that have no occurrence rows.
            if args.get("usage_only").and_then(|v| v.as_bool()).unwrap_or(false) {
                // V59e guard: only fire for TYPE names (PascalCase).
                // A lowercase method name like 'consume' would otherwise get
                // an unrelated trait-impls answer.
                let looks_type = sym.chars().next()
                    .map(|c| c.is_ascii_uppercase()).unwrap_or(false);
                if looks_type {
                    let impls = reliary_search::callgraph_v2::find_trait_impls(db, sym);
                    if !impls.is_empty() {
                        let items: Vec<String> = impls.iter().take(8)
                            .map(|i| format!("{} ({}:{})", i.type_name,
                                std::path::Path::new(&i.file).file_name().map(|x| x.to_string_lossy().to_string()).unwrap_or_else(|| i.file.clone()),
                                i.line))
                            .collect();
                        let text = format!("{} is implemented by {} types: {}.\n", sym, impls.len(), items.join(", "));
                        return DispatchResult::Success(serde_json::json!({
                            "content": [{ "type": "text", "text": text }]
                        }));
                    }
                }
                // Fall through to generic usage handling below via shared handler.
                return handle_symbol_tool_with_db("reliary_find_references", args, db, dir);
            }

            let af_raw = args.get("anchor_file").and_then(|v| v.as_str()).unwrap_or("");
            let af = resolve_af(af_raw);
let al_raw = args.get("anchor_line").and_then(|v| v.as_i64()).unwrap_or(0) as i32; let al = if al_raw > 0 { al_raw - 1 } else { al_raw };
            let th = args.get("threshold").and_then(|v| v.as_f64()).unwrap_or(0.3) as f32;
            let context = args.get("context").and_then(|v| v.as_i64()).unwrap_or(1) as i32;
            let format = args.get("format").and_then(|v| v.as_str()).unwrap_or("json");
            let limit = args.get("limit").and_then(|v| v.as_i64()).map(|v| v as usize).or(Some(50));

            // Arc 42 Phase A: auto-anchor + dead-end fallback.
            // Try user-supplied anchor first. If empty anchor OR zero hits,
            // discover best IS_DEF occurrence. If still zero, return all
            // occurrences unfiltered (grep-style fallback).
            let mut hits = reliary_search::type_flow::find_references_type_flow(db, sym, &af, al, th).unwrap_or_default();
            if hits.is_empty() || af.is_empty() {
                // Auto-discover anchor.
                let auto = match reliary_search::type_flow::find_references_auto(db, sym, th) { Ok(v) => v, Err(e) => { eprintln!("find_references_auto: {}", e); Vec::new() } };
                if !auto.is_empty() {
                    hits = auto;
                } else {
                    // Last resort: unfiltered fallback.
                    let lim = limit.unwrap_or(50);
                    hits = match reliary_search::type_flow::find_references_fallback(db, sym, lim) { Ok(v) => v, Err(e) => { eprintln!("find_references_fallback: {}", e); Vec::new() } };
                }
            }
            // Use the auto-anchor path result.
            let n_hits = hits.len();

            // Arc 63: dynamic, intelligent output.
            // Tier 1: query-type detection from anchor line text.
            // V22: optional path_filter to narrow results to a specific module/directory
            // V30: path_filter is relative (e.g. "io/util/"). file_path is absolute.
            let path_filter = args.get("path_filter").and_then(|v| v.as_str()).unwrap_or("").to_string();
            if !path_filter.is_empty() {
                hits.retain(|h| {
                    h.file_path.starts_with(&path_filter)
                    || h.file_path.contains(&format!("/{path_filter}"))
                    || h.file_path.contains(&path_filter)
                });
            }
            let mut is_summary_mode = false;

            // Tier 2: summary-only mode.
            if let Some(ws) = args.get("with_source") {
                if ws.as_bool() == Some(false) {
                    is_summary_mode = true;
                }
            }
            if args.get("summary").and_then(|v| v.as_bool()) == Some(true) {
                is_summary_mode = true;
            }

            // Tier 3: confidence-gated limit (bounds harness-specified limit).
            let dyn_limit = if let Some(n) = limit {
                // Check top-5 similarity spread — if high confidence, reduce limit.
                let top5 = hits.iter().take(5).collect::<Vec<_>>();
                if top5.len() >= 5 {
                    let avg_sim = top5.iter().map(|h| h.similarity).sum::<f32>() / 5.0;
                    if avg_sim >= 0.9 {
                        n.min(10)
                    } else if avg_sim >= 0.5 {
                        n.min(20) 
                    } else {
                        n.min(30)
                    }
                } else {
                    n.min(30)
                }
            } else {
                (hits.len() as usize).min(30)
            };

            // Apply query-type filter for definition queries.
            // NOTE: keep both defs AND call sites. The LLM needs call sites
            // even for "def" queries. Only reorder to put defs first.
            let source_hits = hits.iter().collect::<Vec<_>>();
            // V8 fix: ensure capped includes call sites, not just top-N defs.
            // Take top-N by similarity (mixed defs+call sites), then add the
            // best call site if no call site made the cut.
            let mut capped = source_hits.iter().take(dyn_limit).copied().collect::<Vec<_>>();
            let has_call_site = capped.iter().any(|h| !h.is_def);
            if !has_call_site {
                if let Some(cs) = hits.iter().find(|h| !h.is_def) {
                    capped.push(cs);
                }
            }
            // P10-3: compute n_files ONCE — used by summary mode AND grep branch.
            // Each branch previously did the same O(N) HashSet scan.
            let n_files: usize = {
                let mut s: std::collections::HashSet<&str> = std::collections::HashSet::new();
                for h in &hits { s.insert(h.file_path.as_str()); }
                s.len()
            };
            // Tier 2: summary-only mode (cheap first phase).
            if is_summary_mode {
                // V8 fix: count across ALL hits, not just capped — otherwise
                // "0 call sites" reported when top-N are all defs.
                let n_defs = hits.iter().filter(|h| h.is_def).count();
                let n_calls = hits.len() - n_defs;
                let summary = format!(
                    "{}: {} defs, {} call sites across {} files.\
                    \nCall `find_references_with_source(name, with_source=true)` for full source listing.",
                    sym, n_defs, n_calls, n_files
                );
                return DispatchResult::Success(serde_json::json!({
                    "content": [{ "type": "text", "text": summary }]
                }));
            }

            if format == "grep" {
                use std::collections::HashMap;
                // Fix 1 (V9 PR2 → 26/30): partition the FULL hits list into defs
                // + call sites and emit top-N of each. Without this, capped.take(N)
                // returned only the highest-similarity hits (all defs), so the
                // model saw "10 defs + 0 call sites" even when 34 call sites
                // existed in the index. Splitting into sections ensures the
                // model sees both.
                let mut defs_hits: Vec<&reliary_search::symbol::OccHit> = Vec::new();
                let mut calls_hits: Vec<&reliary_search::symbol::OccHit> = Vec::new();
                // hits is sorted by similarity (descending). Walk it once and
                // bucket by is_def, preserving similarity order in each bucket.
                for h in hits.iter() {
                    if h.is_def { defs_hits.push(h); } else { calls_hits.push(h); }
                }
                // Cap total grep output to 15 hits — more than this overwhelms the LLM.
                let max_grep = 15;
                // 60% defs, 40% calls — preserves type-flow ranking dominance
                // while guaranteeing call-site visibility.
                let n_defs_shown = defs_hits.len().min((max_grep * 3) / 5);
                let n_calls_shown = calls_hits.len().min(max_grep - n_defs_shown);
                let workdir = cached_cwd().clone();
                let mut file_cache: HashMap<String, Vec<String>> = HashMap::new();
                // Helper closure to format a hit as "relpath:line: text".
                // Phase 1-4: cache-safe cost reduction — whitespace-stripped source,
                // compact headers, abbreviated paths, merged summary.
                let format_hit = |h: &reliary_search::symbol::OccHit, file_cache: &mut HashMap<String, Vec<String>>| -> String {
                    let file_lines = file_cache.entry(h.file_path.clone())
                        .or_insert_with(|| {
                            let abs = if Path::new(&h.file_path).is_absolute() {
                                PathBuf::from(&h.file_path)
                            } else {
                                workdir.join(&h.file_path)
                            };
                            std::fs::read_to_string(&abs)
                                .ok()
                                .map(|s| s.lines().map(String::from).collect())
                                .unwrap_or_default()
                        });
                    let line_no = h.line + 1;
                    let raw_text = file_lines.get(h.line as usize)
                        .cloned()
                        .unwrap_or_default();
                    // Phase 1: strip leading/trailing whitespace from source text.
                    // The model reads both formatted and minified code equally well.
                    let stripped = raw_text.trim();
                    let line_text = if stripped.len() > 60 {
                        let end = stripped.floor_char_boundary(60);
                        format!("{}...", &stripped[..end])
                    } else {
                        stripped.to_string()
                    };
                    let relpath = if Path::new(&h.file_path).is_absolute() {
                        let wd = workdir.to_string_lossy().to_string();
                        let stripped = h.file_path.strip_prefix(&wd)
                            .map(|s| s.trim_start_matches('/').to_string());
                        stripped.unwrap_or_else(|| h.file_path.clone())
                    } else {
                        h.file_path.clone()
                    };
                    format!("{}:{} {}", relpath, line_no, line_text)
                };
                // Build two labeled sections so the LLM sees call sites explicitly.
                // Phase 2: compact section headers (D:9/12 instead of --- DEFS (9/12 shown) ---).
                let mut sections: Vec<String> = Vec::new();
                if n_defs_shown > 0 {
                    let mut def_lines: Vec<String> = Vec::with_capacity(n_defs_shown);
                    for h in defs_hits.iter().take(n_defs_shown) {
                        def_lines.push(format_hit(h, &mut file_cache));
                    }
                    sections.push(format!("D:{}/{}\n{}",
                        n_defs_shown, defs_hits.len(), def_lines.join("\n")));
                }
                if n_calls_shown > 0 {
                    let mut call_lines: Vec<String> = Vec::with_capacity(n_calls_shown);
                    for h in calls_hits.iter().take(n_calls_shown) {
                        call_lines.push(format_hit(h, &mut file_cache));
                    }
                    sections.push(format!("C:{}/{}\n{}",
                        n_calls_shown, calls_hits.len(), call_lines.join("\n")));
                }
                let raw_text = sections.join("\n\n");
                // Arc 59 Phase 2: cognitive summary header.
                // 3-line summary lets the LLM process via "read and summarize"
                // circuits instead of "parse and verify" circuits. Less parsing
                // per turn = faster LLM inference = less wall time.
                // V8 fix: count across ALL hits, not just capped.
                let n_defs = hits.iter().filter(|h| h.is_def).count();
                let n_calls = hits.len() - n_defs;
                // Phase 4: merged summary — single line with compact counts.
                let summary = format!(
                    "{}: {}D {}C {}f",
                    sym, n_defs, n_calls, n_files
                );
                let final_text = format!("{}\n\n{}", summary, raw_text);
                DispatchResult::Success(serde_json::json!({
                    "content": [{ "type": "text", "text": final_text }]
                }))
            } else {
                use std::collections::HashMap;
                let workdir = cached_cwd().clone();
                let wd_str = workdir.to_string_lossy().to_string();
                let mut file_cache: HashMap<String, Vec<String>> = HashMap::new();
                let arr: Vec<_> = capped.iter().enumerate().map(|(rank, h)| {
                    let file_lines = file_cache.entry(h.file_path.clone())
                        .or_insert_with(|| {
                            let abs = if Path::new(&h.file_path).is_absolute() {
                                PathBuf::from(&h.file_path)
                            } else {
                                workdir.join(&h.file_path)
                            };
                            std::fs::read_to_string(&abs)
                                .ok()
                                .map(|s| s.lines().map(String::from).collect())
                                .unwrap_or_default()
                        });
                    let line_no = h.line + 1; // 1-based
                    let relp = relpath_with(&h.file_path, &wd_str);
                    if rank < 5 {
                        let start = (line_no - 1 - context).max(0) as usize;
                        let end = (line_no - 1 + context + 1).min(file_lines.len() as i32) as usize;
                        // Phase 1: strip whitespace from source snippet (cache-safe cost reduction).
                        let source_snippet = if start < file_lines.len() {
                            file_lines[start..end].iter()
                                .map(|l| l.trim())
                                .collect::<Vec<_>>()
                                .join("\n")
                        } else {
                            String::new()
                        };
                        serde_json::json!({
                            "file": relp, "line": line_no, "col": h.col,
                            "is_def": h.is_def, "similarity": h.similarity,
                            "source": source_snippet
                        })
                    } else {
                        serde_json::json!({
                            "file": relp, "line": line_no, "col": h.col,
                            "is_def": h.is_def, "similarity": h.similarity
                        })
                    }
                }).collect();
                let raw_text = serde_json::json!({
                    "name": sym, "anchor_file": af, "anchor_line": al,
                    "threshold": th, "method": "type_flow_with_source",
                    "context": context, "count": n_hits, "hits": arr
                }).to_string();
                // Arc 49: sift tool output before returning to LLM (rtk pattern, cache safe)
                let final_text = raw_text;
                DispatchResult::Success(serde_json::json!({
                    "content": [{ "type": "text", "text": final_text }]
                }))
            }
        }
        "reliary_brace_graph" => {
            let file_path = args.get("file_path").and_then(|v| v.as_str()).unwrap_or("");
            if file_path.is_empty() {
                return err_missing_param("file_path");
            }
            let fp = match safe_path(file_path, dir) {
                Ok(p) => p,
                Err(e) => return err_db(format!("reliary_brace_graph: {}", e)),
            };
            let line_raw = args.get("line").and_then(|v| v.as_i64()).unwrap_or(0) as i32;
            let line = if line_raw > 0 { line_raw - 1 } else { line_raw };
            let depth = args.get("depth").and_then(|v| v.as_i64()).unwrap_or(3) as usize;
            match reliary_search::brace_graph::get_brace_graph(&fp.to_string_lossy()) {
                Some(graph) => {
                    let enclosing = graph.find_enclosing(line);
                    let scope_chain: Vec<String> = if let Some(node) = enclosing {
                        let mut chain = vec![format!("{} [{}:{}]", node.role, node.start_line, node.end_line)];
                        let mut current = node;
                        for _ in 0..depth {
                            if let Some(parent) = reliary_search::brace_graph::find_parent(&graph, current) {
                                chain.push(format!("{} [{}:{}]", parent.role, parent.start_line, parent.end_line));
                                current = parent;
                            } else { break; }
                        }
                        chain
                    } else { vec![] };
                    let all_nodes = graph.find_by_role("function_def");
                    let fn_bodies: Vec<_> = all_nodes.iter().take(20).map(|n| {
                        format!("{} [{}:{}]", n.role, n.start_line, n.end_line)
                    }).collect();
                    DispatchResult::Success(serde_json::json!({
                        "content": [{ "type": "text", "text": serde_json::json!({
                            "file": file_path, "line": line,
                            "enclosing_scope": scope_chain,
                            "function_definitions": fn_bodies
                        }).to_string() }]
                    }))
                }
                None => err_not_found(&format!("file {}", file_path)),
            }
        }
        "reliary_call_graph" => {
            let file_path = args.get("file_path").and_then(|v| v.as_str()).unwrap_or("");
            if file_path.is_empty() {
                return err_missing_param("file_path");
            }
            let fp = match safe_path(file_path, dir) {
                Ok(p) => p,
                Err(e) => return err_db(format!("reliary_call_graph: {}", e)),
            };
            match reliary_search::brace_graph::get_brace_graph(&fp.to_string_lossy()) {
                Some(graph) => {
                    let fns = graph.find_by_role("function_def");
                    let edges: Vec<_> = fns.iter().map(|f| {
                        let calls = f.method_calls_in();
                        serde_json::json!({
                            "fn_line": f.start_line, "fn_end": f.end_line,
                            "calls_count": calls.len(),
                            "call_lines": calls.iter().take(10).map(|c| c.0).collect::<Vec<_>>()
                        })
                    }).collect();
                    DispatchResult::Success(serde_json::json!({
                        "content": [{ "type": "text", "text": serde_json::json!({
                            "file": file_path, "total_fns": fns.len(), "edges": edges
                        }).to_string() }]
                    }))
                }
                None => err_db(format!("call_graph: file not found: {}", file_path)),
            }
        }
        "reliary_brace_debug" => {
            let file_path = args.get("file_path").and_then(|v| v.as_str()).unwrap_or("");
            match reliary_search::brace_graph::get_brace_graph(file_path) {
                Some(graph) => {
                    let all = graph.find_by_role("function_def");
                    let all_struct = graph.find_by_role("type_name");
                    let total = graph.node_count();
                    let all_roles: Vec<String> = vec!["function_def".to_string(), "type_name".to_string(), "method_call".to_string()];
                    let role_counts: Vec<(String, usize)> = all_roles.iter().map(|r| (r.clone(), graph.find_by_role(r).len())).collect();
                    DispatchResult::Success(serde_json::json!({
                        "content": [{ "type": "text", "text": serde_json::json!({
                            "file": file_path, "total_nodes": total,
                            "function_def": all.len(), "type_name": all_struct.len(),
                            "role_counts": role_counts
                        }).to_string() }]
                    }))
                }
                None => err_db(format!("brace_debug: file not found: {}", file_path)),
            }
        }
        _ => DispatchResult::Error(-32601, format!("unknown symbol tool: {}", name)),
    }
}

// ── Stdio transport (fallback, always available) ──

fn handle_tool_call_stdio(id: &serde_json::Value, params: &serde_json::Value) {
    let name = params.get("name").and_then(|v| v.as_str()).unwrap_or("");
    let empty_map = serde_json::Map::new();
    if let Some(v) = params.get("arguments") {
        if !v.is_object() {
            eprintln!("[WARN] handle_tool_call_stdio: 'arguments' is not an object");
        }
    }
    let args = params.get("arguments").and_then(|v| v.as_object()).unwrap_or(&empty_map);

    // V56: session-level result cache. Tools are deterministic, so identical
    // (tool, args) pairs can be served from cache. On hit, return the compact
    // one-line "cached:" reply — the model asked the same question again and
    // only needs the answer re-established, not the full result re-billed.
    // V56c: result cache DISABLED by default — benches showed serving
    // cached results (compact OR full-text) costs 2.5-3.5 score points:
    // the model re-queries expecting fresh data (JIT builds new occurrence
    // rows between calls; the index changes under it), and an identical
    // repeat answer blocks that discovery. The cost wins (WC -30-46%,
    // wall -21%, calls -40%) don't justify the accuracy loss.
    // Opt-in via RELIARY_RESULT_CACHE=1.
    // V58: default ON — the generation-counter key makes repeats safe.
let cacheable = std::env::var("RELIARY_RESULT_CACHE").map(|v| v != "0").unwrap_or(true)
        && !args.get("verbose").and_then(|v| v.as_bool()).unwrap_or(false)
        && name != "reliary_pack_query"
        && name != "reliary_pack";
    let db_for_key = get_cached_db();
    let cache_key = result_cache_key(name, args, db_for_key.as_ref());
    if let Some(conn) = db_for_key { return_cached_db(conn); }
    if cacheable {
        if let Some(cached_text) = result_cache_get(cache_key) {
            // V56b: return the FULL cached text on repeat (not the compact
            // one-liner). Bench showed the compact form cost -2.5 score —
            // the model re-queries expecting the full result with code
            // evidence, and a bare answer line loses the source context it
            // was about to use. Full-text repeat still saves the DB work,
            // tool latency, and the JIT rebuild — just not the token
            // re-billing (which the KV cache already discounts 78-94%).
            respond(id, serde_json::json!({
                "content": [{ "type": "text", "text": cached_text }]
            }));
            return;
        }
    }

    let mut result = dispatch_tool_call(name, args);
    // V15: truncate text content in tool outputs to keep multi-turn context manageable.
    // Default 4000 chars per text field. Per-tool args.verbose=true skips truncation.
    // Env RELIARY_NO_TRUNCATE=1 disables globally.
    // NOTE: sift compression is NOT applied here — MCP tool output is already compact.
    // Sift is for bash output only (via `reliary wrap` / RELIARY_SIFT_BASH=1).
    truncate_result(&mut result, args, name);
    // M4: freshness stamp (after truncation so it always survives).
    // V73: pass the connection we already have — never open a second one.
    let stamp_db = get_cached_db();
    stamp_result(&mut result, stamp_db.as_ref());
    if let Some(conn) = stamp_db { return_cached_db(conn); }
    // V56: store the serialized text result for repeat calls.
    if cacheable {
        if let DispatchResult::Success(ref r) = result {
            if let Some(text) = r.get("content").and_then(|c| c.as_array())
                .and_then(|arr| arr.first())
                .and_then(|c| c.get("text"))
                .and_then(|t| t.as_str())
            {
                if !text.is_empty() {
                    result_cache_put(cache_key, text.to_string());
                }
            }
        }
    }
    match result {
        DispatchResult::Success(result) => respond(id, result),
        DispatchResult::Error(code, message) => {
            // M4: stamp errors too — AGENTS.md promises a stamp on every
            // response, and agents use it to detect a stale index even when a
            // lookup failed.
            let stamp_db = get_cached_db();
            let stamped = format!("{}\n[idx:{}]", message, index_stamp(stamp_db.as_ref()));
            if let Some(conn) = stamp_db { return_cached_db(conn); }
            respond_error(id, code, &stamped)
        }
    }
}

/// Auto-trust: if CWD is inside a git project without an existing .reliary/
/// index, create one automatically. Returns the project root if trusted
/// (or already trusted). Returns None if not a git project or auto-trust
/// failed. The `auto_trusted` flag in the response indicates whether
/// auto-trust actually ran (vs. the index already existing).
fn auto_trust_if_needed() -> AutoTrustResult {
    let cwd = match std::env::current_dir() {
        Ok(c) => c,
        Err(_) => return AutoTrustResult::Failed,
    };
    let mut cur = cwd.clone();
    for _ in 0..10 {
        if cur.join(".git").exists() {
            let index_path = cur.join(".reliary/index.sqlite");
            if index_path.exists() {
                return AutoTrustResult::AlreadyTrusted(cur); // Already trusted
            }
            // Auto-trust: create .reliary/ and index
            eprintln!("[reliary] auto-indexing {} (git project detected)...", cur.display());
            let t0 = std::time::Instant::now();
            let _ = std::fs::create_dir_all(cur.join(".reliary"));
            let db_path = cur.join(".reliary/index.sqlite");
            match reliary_core::safe_open_db(&db_path.to_string_lossy()) {
                Ok(db) => {
                    if reliary_search::schema::create_new_db(&db).is_err() {
                        eprintln!("[reliary] auto-index: schema creation failed");
                        return AutoTrustResult::Failed;
                    }
                    match reliary_search::ingest::index_directory(&db, &cur.to_string_lossy()) {
                        Ok(count) => {
                            eprintln!("[reliary] auto-indexed {} files in {:?}", count, t0.elapsed());
                            return AutoTrustResult::NewlyTrusted(cur);
                        }
                        Err(e) => {
                            eprintln!("[reliary] auto-index failed: {}", e);
                            return AutoTrustResult::Failed;
                        }
                    }
                }
                Err(e) => {
                    eprintln!("[reliary] auto-index DB error: {}", e);
                    return AutoTrustResult::Failed;
                }
            }
        }
        if !cur.pop() {
            break;
        }
    }
    AutoTrustResult::NotGitProject
}

enum AutoTrustResult {
    NewlyTrusted(std::path::PathBuf),
    AlreadyTrusted(std::path::PathBuf),
    Failed,
    NotGitProject,
}

impl AutoTrustResult {
    fn project_root(&self) -> Option<&std::path::Path> {
        match self {
            AutoTrustResult::NewlyTrusted(p) | AutoTrustResult::AlreadyTrusted(p) => Some(p),
            _ => None,
        }
    }
    fn was_auto_trusted(&self) -> bool {
        matches!(self, AutoTrustResult::NewlyTrusted(_))
    }
}

pub fn serve_stdio() {
    let stdin = io::stdin();
    for line in stdin.lock().lines() {
        let line = match line {
            Ok(l) => l,
            Err(_) => break,
        };
        if line.trim().is_empty() { continue; }

        let msg: serde_json::Value = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(_) => continue,
        };

        let id = msg.get("id").cloned().unwrap_or(serde_json::Value::Null);
        let method = msg.get("method").and_then(|v| v.as_str()).unwrap_or("");

        match method {
            "initialize" => {
                // Auto-trust: if this is a git project without an index,
                // create one automatically so the LLM never sees "no index" errors.
                let trust_result = auto_trust_if_needed();
                let auto_trusted = trust_result.was_auto_trusted();
                let project_root_path = trust_result.project_root().map(|p| p.to_path_buf());

                // Arc 30 Phase 4: spawn background file watcher (best-effort).
                // Skip if NO_RELIARY_WATCHER=1 or if we're not in a project dir.
                if std::env::var("NO_RELIARY_WATCHER").is_err() {
                    let watch_dir = if let Some(root) = &project_root_path {
                        Some(root.clone())
                    } else {
                        std::env::current_dir().ok()
                    };
                    if let Some(dir) = watch_dir {
                        match crate::watcher::start_watcher(&dir) {
                            Ok(h) => {
                                eprintln!("[INFO] watcher started on {:?}", h.workdir.display());
                                // Stash the handle so it isn't dropped.
                                // V61: a second initialize must not spawn a second
                                // watcher — OnceLock::set silently ignores the
                                // duplicate, leaking a thread that reindexes
                                // concurrently with the first.
                                if WATCHER_HANDLE.set(h).is_err() {
                                    eprintln!("[mcp] watcher already running — ignoring duplicate initialize");
                                }
                            }
                            Err(e) => { eprintln!("[WARN] watcher start skipped: {:?}", e); }
                        }
                    }
                }
                // Arc 50: eager-index the current project at MCP server startup when requested.
                if std::env::var("RELIARY_EAGER_INDEX").is_ok() {
                    if let Ok(cwd) = std::env::current_dir() {
                        let db_path_str = format!("{}/.reliary/index.sqlite", cwd.to_string_lossy().trim_end_matches('/'));
                        if std::fs::metadata(&db_path_str).is_ok() {
                            let db_path = std::path::PathBuf::from(&db_path_str);
                            eprintln!("[reliary] eager index: db found, opening...");
                            eprintln!("[INFO] eager indexing: building lazy tables for {:?}", db_path.display());
                            eprintln!("[reliary] eager index: opening DB at {}", db_path.display());
                            use std::io::Write;
                            let _ = std::io::stderr().flush();
                            match rusqlite::Connection::open(&db_path) {
                                Ok(db) => {
                                eprintln!("[reliary] eager index: DB opened OK");
                                // Skip rebuild if already built (trust already did this).
                                let occ_count: i64 = db.query_row("SELECT COUNT(*) FROM occurrence", [], |r| r.get(0)).unwrap_or(0);
                                if occ_count > 0 {
                                    eprintln!("[reliary] eager index: occurrence table already populated ({} rows), skipping rebuild", occ_count);
                                } else {
                                    eprintln!("[reliary] eager index: building occurrence table...");
                                    let _ = reliary_search::lazy_occurrence::build_all_occurrence(&db);
                                }
                                let file_ids: Vec<i64> = {
                                    match db.prepare_cached("SELECT id FROM file_map") {
                                        Ok(mut s) => match s.query_map([], |r| r.get::<_, i64>(0)) {
                                            Ok(rows) => rows.filter_map(|x| x.ok()).collect(),
                                            Err(e) => { eprintln!("[WARN] query file_map: {:?}", e); Vec::new() }
                                        },
                                        Err(e) => { eprintln!("[WARN] prepare file_map: {:?}", e); Vec::new() }
                                    }
                                };
                                // Skip lazy table rebuild if blocks already exist.
                                let block_count: i64 = db.query_row("SELECT COUNT(*) FROM block", [], |r| r.get(0)).unwrap_or(0);
                                if block_count == 0 {
                                    eprintln!("[reliary] eager index: building lazy tables for {} files...", file_ids.len());
                                    for fid in &file_ids {
                                        let _ = reliary_search::lazy_tables::ensure_all_for_file(&db, *fid);
                                    }
                                    eprintln!("[reliary] eager index: lazy tables done");
                                } else {
                                    eprintln!("[reliary] eager index: lazy tables already populated ({} blocks), skipping", block_count);
                                }
                                let file_paths: Vec<String> = {
                                    match db.prepare_cached("SELECT file_path FROM file_map") {
                                        Ok(mut s) => match s.query_map([], |r| r.get::<_, String>(0)) {
                                            Ok(rows) => rows.filter_map(|x| x.ok()).collect(),
                                            Err(_) => Vec::new()
                                        },
                                        Err(_) => Vec::new()
                                    }
                                };
                                std::thread::spawn(move || {
                                    let t0 = std::time::Instant::now();
                                    for fp in &file_paths {
                                        let _ = reliary_search::file_meta::get(fp);
                                        // Also warm scope type maps (used by receiver resolution).
                                        let _ = reliary_search::scope_types::build_all_scope_type_maps(fp);
                                    }
                                    eprintln!("[reliary] background warming done in {:?}", t0.elapsed());
                                });
                                }
                                Err(e) => {
                                    eprintln!("[reliary] eager index: DB open failed: {}", e);
                                }
                            }
                        }
                    }
                }
                respond(&id, serde_json::json!({
                    "protocolVersion": "2024-11-05",
                    "capabilities": { "tools": {} },
                    "serverInfo": {
                        "name": "reliary",
                        "version": env!("CARGO_PKG_VERSION"),
                        "auto_trusted": auto_trusted,
                    }
                }));
            }
            "notifications/initialized" => {}
            "tools/list" => {
                respond(&id, serde_json::json!({ "tools": tool_definitions_filtered() }));
            }
            "tools/call" => {
                let params = match msg.get("params") {
                    Some(p) => p,
                    None => { respond_error(&id, -32602, "missing params"); continue; }
                };
                handle_tool_call_stdio(&id, params);
            }
            _ => {
                if !method.starts_with("notifications/") {
                    respond_error(&id, -32601, &format!("method not found: {}", method));
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_tool_definitions_have_required_fields() {
        let tools = tool_definitions();
        assert!(!tools.is_empty(), "should have at least one tool");
        for t in tools.iter() {
            let name = t.get("name").and_then(|v| v.as_str()).unwrap_or("");
            assert!(!name.is_empty(), "each tool needs a name");
            assert!(name.starts_with("reliary_"), "tool name should start with reliary_: {}", name);
            assert!(t.get("description").is_some(), "tool {} needs a description", name);
            assert!(t.get("inputSchema").is_some(), "tool {} needs inputSchema", name);
        }
    }

    #[test]
    fn test_tool_list_response_format() {
        let tools = tool_definitions();
        let response = serde_json::json!({ "tools": tools });
        let json = serde_json::to_string(&response).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
        let tool_names: Vec<&str> = parsed["tools"].as_array().unwrap()
            .iter().map(|t| t["name"].as_str().unwrap()).collect();
        assert!(tool_names.contains(&"reliary_search"));
        assert!(tool_names.contains(&"reliary_find_references"));
        assert!(tool_names.contains(&"reliary_call_graph"));
        assert!(tool_names.contains(&"reliary_list_methods"));
        assert!(tool_names.contains(&"reliary_find_dead_code"));
        assert!(tool_names.contains(&"reliary_describe"));
        assert!(tool_names.contains(&"reliary_verify"));
        // V27: 7 tools; V58 adds reliary_similar → 8; V70 P1 adds verify → 9.
        assert_eq!(tool_names.len(), 9, "expected 9 tools, got {}: {:?}", tool_names.len(), tool_names);
        assert!(tool_names.contains(&"reliary_similar"));
    }

    #[test]
    fn test_handle_tool_call_unknown_tool() {
        let params = serde_json::json!({
            "name": "nonexistent_tool",
            "arguments": {}
        });
        handle_tool_call_stdio(&serde_json::json!(1), &params); // Should not panic
    }

    #[test]
    fn test_handle_tool_call_search_missing_args() {
        let params = serde_json::json!({
            "name": "reliary_search",
            "arguments": {}
        });
        handle_tool_call_stdio(&serde_json::json!(1), &params); // Should not panic
    }

    #[test]
    fn test_handle_tool_call_compress() {
        let params = serde_json::json!({
            "name": "reliary_compress",
            "arguments": { "text": "hello world" }
        });
        handle_tool_call_stdio(&serde_json::json!(1), &params); // Should not panic
    }

    #[test]
    fn test_dispatch_tool_call_pure() {
        // Test pure dispatch without I/O
        let result = dispatch_tool_call("reliary_compress", &Default::default());
        match result {
            DispatchResult::Success(_) => {},
            DispatchResult::Error(_, _) => panic!("expected success"),
        }
        let result = dispatch_tool_call("nonexistent", &Default::default());
        match result {
            DispatchResult::Success(_) => panic!("expected error"),
            DispatchResult::Error(code, _) => assert_eq!(code, -32601),
        }
    }
}
