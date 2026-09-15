# FIX PLAN V8 — AST-Level Gap Fixes (Transparent Grammar-Free)

Branch: `fix/v5-deep-audit` (extend) → merge as `fix/v8-ast-gaps`

Scope: 5 quick-win bug fixes for the AST-level gaps surfaced by the long bench
(q4 block_on_chain scored 1/3, q10 dead_code scored 0/3 on cond=A historically).

Read-only investigation completed in (b27). Key findings:

- W1 (HIGH): `reliary_callgraph_v2` schema lacks `anchor_file`/`anchor_line`. When
  LLM asks for `block_on`, the tool calls `find_definition` which returns whichever
  indexed `block_on` def happens to come first (often `spawn_local`'s body or test
  fixtures), not the `Runtime::block_on` def the LLM actually wants.

- W2 (HIGH): `reliary_trace_path` exits early at `trace_path.rs:90` when
  `phrase_id == 0`. For `Runtime::block_on`, the structural classifier failed to
  index the `pub fn block_on<F: Future>` line as `is_def=1` in runtime.rs.
  Verified: 0 occurrences of block_on in runtime.rs in the index.

- W3 (HIGH): Structural classifier `classify_structural` (`structural.rs:31`) fails
  on Rust generic function signatures like `pub fn block_on<F: Future>(&self, ...)`.
  The "last identifier before delimiter" logic chokes on the `<F: Future>` generic
  parameter. Need to skip past generics before finding the signature delimiter.

- W4 (LOW): `reliary_dead_symbols` works correctly when called with sufficient
  limit. Verified: limit=200 returns 200 dead symbols including many in io/util.
  Tool description + schema default (already 100) need a hint that 100+ is the
  expected default for cross-project scans.

- W5 (LOW): cond=A system prompt (`RELIARY_SYS`) doesn't tell the LLM which tool
  to use for which question class. Add 3-4 lines mapping question types to tools.

No new dependencies. No tree-sitter, syn, quote, pest. Pure grammar-free stays
pure. Estimated total: ~8 hours.

---

## W1: Add anchor_file/anchor_line to `reliary_callgraph_v2`

**Files:** `crates/reliary-agent/src/mcp.rs:80`, `crates/reliary-search/src/callgraph_v2.rs:331`

**Root cause:** Schema lacks `anchor_file`/`anchor_line`. When LLM has the
definition context (e.g., it just got `Runtime::block_on` at `runtime/runtime.rs:341`
from `reliary_goto_def`), it cannot pass that context to `reliary_callgraph_v2`.
The tool re-runs `find_definition` which returns the wrong `block_on` (often
`spawn_local`'s body, which contains 5 calls to `block_on`).

**Fix sketch:**

### 1. Update schema (mcp.rs:80)

Change:
```
"name": "reliary_callgraph_v2", "description": "...", "inputSchema": {
    "type": "object",
    "properties": {
        "name": {"type": "string"},
        "path": {"type": "string"},
        "summary": {"type": "boolean", ...}
    },
    "required": ["name"]
}
```

To:
```
"name": "reliary_callgraph_v2", "description": "...", "inputSchema": {
    "type": "object",
    "properties": {
        "name": {"type": "string"},
        "path": {"type": "string"},
        "anchor_file": {"type": "string", "description": "Optional. Path to the file containing the definition (from reliary_goto_def). When omitted, the tool auto-discovers the definition."},
        "anchor_line": {"type": "integer", "description": "Optional. Line of the definition (1-based, from reliary_goto_def). Used to disambiguate when multiple symbols share the same name."},
        "summary": {"type": "boolean", ...}
    },
    "required": ["name"]
}
```

### 2. Update handler (mcp.rs:774-813)

Change:
```rust
let sym = args.get("name").and_then(|v| v.as_str()).unwrap_or("");
let path = args.get("path").and_then(|v| v.as_str()).unwrap_or(".");
let summary = args.get("summary").and_then(|v| v.as_bool()).unwrap_or(false);
match reliary_search::callgraph_v2::build_call_graph(&db, sym, path) {
```

To:
```rust
let sym = args.get("name").and_then(|v| v.as_str()).unwrap_or("");
let path = args.get("path").and_then(|v| v.as_str()).unwrap_or(".");
let summary = args.get("summary").and_then(|v| v.as_bool()).unwrap_or(false);
let anchor_file = args.get("anchor_file").and_then(|v| v.as_str()).unwrap_or("");
let anchor_line = args.get("anchor_line").and_then(|v| v.as_i64()).unwrap_or(0) as i32;
let anchor = if anchor_file.is_empty() {
    None
} else {
    Some((anchor_file.to_string(), anchor_line))
};
match reliary_search::callgraph_v2::build_call_graph(&db, sym, path, anchor) {
```

### 3. Update `build_call_graph` signature (callgraph_v2.rs:331)

Change:
```rust
pub fn build_call_graph(
    db: &Connection, raw_name: &str, _path: &str,
) -> rusqlite::Result<CallGraph> {
    // Step 1: Find the definition.
    let (anchor_file, anchor_line) = match find_definition(db, raw_name) {
```

To:
```rust
pub fn build_call_graph(
    db: &Connection, raw_name: &str, _path: &str,
    anchor: Option<(String, i32)>,
) -> rusqlite::Result<CallGraph> {
    // Step 1: Find the definition.
    // If anchor provided, use it directly; otherwise auto-discover.
    let (anchor_file, anchor_line) = match anchor {
        Some((f, l)) if !f.is_empty() => (f, l),
        _ => match find_definition(db, raw_name) {
```

When anchor is provided, also verify the anchor matches the requested symbol.
Add a verification step: read the file at anchor_line and check that
`raw_name` appears as the function name on that line (post-trim, before
generic params). If mismatch, return an error explaining the conflict.

### 4. Update PRIMARY_TOOLS list

If `reliary_callgraph_v2` is in the PRIMARY_TOOLS set in mcp.rs:107, no change
needed (schema is auto-derived). Verify after edit.

### 5. Tests to add

- New test: `test_build_call_graph_with_anchor` — pass anchor pointing to
  runtime/runtime.rs:341, verify returned `anchor_file/anchor_line` matches
  and `source_preview` shows `pub fn block_on` not `spawn_local`.
- New test: `test_build_call_graph_with_bad_anchor` — pass anchor pointing
  to wrong file, verify error returned.

---

## W2: Fix `reliary_trace_path` early exit when phrase missing

**Files:** `crates/reliary-search/src/trace_path.rs:42-90`

**Root cause:** At line 90, `if anchor_fid > 0 && phrase_id > 0` gates the entire
function. When `Runtime::block_on` is asked about at runtime/runtime.rs:345,
phrase_id is 0 because the structural classifier didn't index that def. The
function returns immediately with empty results — confusing to the LLM.

**Fix sketch:**

### 1. Add a fallback for missing phrase_id

After line 67 (where `phrase_id` is queried), add:

```rust
if phrase_id == 0 {
    // Phrase not indexed as a stem. Fall back to unstemmed literal match.
    let literal_count: i64 = db.query_row(
        "SELECT COUNT(*) FROM occurrence o
         JOIN file_map f ON f.id = o.file_id
         WHERE f.file_id = ?1 AND o.phrase_id IN (
             SELECT id FROM phrases WHERE phrase = ?2
         )",
        params![anchor_fid, anchor_symbol.to_ascii_lowercase()],
        |r| r.get(0),
    ).unwrap_or(0);
    if literal_count == 0 {
        // Last resort: enumerate all phrases in this file and find by name substring
        let mut stmt = db.prepare_cached(
            "SELECT DISTINCT p.id FROM phrases p
             JOIN occurrence o ON o.phrase_id = p.id
             WHERE o.file_id = ?1 AND p.phrase LIKE ?2 ESCAPE '\\'
             LIMIT 10"
        )?;
        let rows = stmt.query_map(
            params![anchor_fid, format!("%{}%", anchor_symbol.replace('%', r"\%").replace('_', r"\_"))],
            |r| r.get::<_, i32>(0)
        )?;
        for r in rows {
            if let Ok(pid) = r { /* set phrase_id */ break; }
        }
    }
}
```

This gives three layers: stem match, unstemmed literal match, substring match.

### 2. Add a clear error path

When all three fallbacks fail, return a `TraceResult` with empty arrays AND an
`error` field explaining why:

```rust
return Ok(TraceResult {
    anchor: TraceAnchor { symbol: anchor_symbol.to_string(), file: anchor_file_rel, line: anchor_line as usize },
    direction: direction.to_string(),
    depth_used: depth,
    callers: vec![],
    callees: vec![],
    error: Some(format!("symbol '{}' not indexed at {}:{}; structural classifier may have missed it", anchor_symbol, anchor_file, anchor_line)),
});
```

This requires adding an `error: Option<String>` field to `TraceResult` struct.
Check struct definition first (trace_path.rs:1-40).

### 3. Tests to add

- `test_trace_path_unknown_symbol` — pass `nonexistent_xyz` as anchor_symbol,
  verify `error` field is populated.
- `test_trace_path_block_on_runtime` — verify `Runtime::block_on` at runtime.rs:345
  returns either real results (after W3 fix) OR a clear error.

---

## W3: Fix structural classifier for Rust generic signatures

**Files:** `crates/reliary-search/src/structural.rs:31-148`

**Root cause:** `classify_structural` looks for the "last identifier before the
signature delimiter" to extract the defined name. For Rust generic functions:

```rust
pub fn block_on<F: Future>(&self, future: F) -> F::Output {
```

The signature delimiter is `(`. But the generic parameter `<F: Future>` appears
before it. The classifier probably stops at `Future` (the last identifier before
the `<` rather than before the `(`).

**Fix sketch:**

### 1. Skip past generic parameters before finding the delimiter

In `structural.rs`, the `find_byte_outside_string(trimmed, b'(').is_some()` check
at line 65 should be modified to skip over `<...>` generics first:

Add a helper function:
```rust
/// Find the byte index of `(` or `<` outside strings, but skip past generic
/// parameter lists (`<T>`, `<T: Bound>`, `<T, U>`) first. Returns the position
/// of the signature `(`.
fn find_signature_paren(s: &str) -> Option<usize> {
    let bytes = s.as_bytes();
    let mut i = 0;
    let mut in_string = false;
    let mut escape = false;
    while i < bytes.len() {
        let b = bytes[i];
        if escape { escape = false; i += 1; continue; }
        if b == b'\\' && in_string { escape = true; i += 1; continue; }
        if b == b'"' { in_string = !in_string; i += 1; continue; }
        if in_string { i += 1; continue; }
        if b == b'<' {
            // Skip past generic parameter list.
            let mut depth = 1;
            i += 1;
            while i < bytes.len() && depth > 0 {
                if bytes[i] == b'<' { depth += 1; }
                else if bytes[i] == b'>' { depth -= 1; }
                i += 1;
            }
            continue;
        }
        if b == b'(' { return Some(i); }
        i += 1;
    }
    None
}
```

### 2. Use the new helper

Replace `find_byte_outside_string(trimmed, b'(').is_some()` at line 65 with:
```rust
let sig_paren_pos = find_signature_paren(trimmed);
let has_open_paren = sig_paren_pos.is_some();
```

Then when finding the "last identifier before delimiter", use `sig_paren_pos`
as the boundary instead of `(`:

```rust
let search_end = sig_paren_pos.unwrap_or(bytes.len());
let name_end = scan_last_identifier_before(&trimmed[..search_end])?;
```

This change should cascade: `classify_structural` should now correctly identify
`block_on` (not `Future`) as the defined name on the line.

### 3. Verify with quick test

```bash
# After fix, verify runtime.rs:341 produces a def occurrence:
echo "pub fn block_on<F: Future>(&self, future: F) -> F::Output {" > /tmp/test_sig.rs
cargo test -p reliary-search --features=test classify_structural
# Or run a small Rust test that calls classify_structural directly on the line
```

### 4. Add test case

In `crates/reliary-search/src/structural.rs` tests module:
```rust
#[test]
fn test_classify_structural_generic_fn() {
    let r = classify_structural("pub fn block_on<F: Future>(&self, future: F) -> F::Output {", 0, true, false);
    assert_eq!(r.tag, 1); // fn_def
    assert!(r.is_def);
    assert_eq!(r.defined_name, Some("block_on"));
}

#[test]
fn test_classify_structural_generics_with_constraints() {
    let r = classify_structural("fn map<B, F: FnOnce(A) -> B>(self, f: F) -> Option<B> {", 0, true, false);
    assert_eq!(r.defined_name, Some("map"));
}
```

### 5. Re-index tokio + verify

After the fix, re-index tokio and re-run q4 long bench to confirm lift.

```bash
reliary reindex-file --help
# Actually need full re-index:
rm -rf /tmp/tokio-corpus/tokio/src/.reliary/index.sqlite*
reliary index /tmp/tokio-corpus/tokio/src

# Verify block_on is now indexed in runtime.rs:
sqlite3 /tmp/tokio-corpus/tokio/src/.reliary/index.sqlite \
  "SELECT COUNT(*) FROM occurrence o JOIN phrases p ON o.phrase_id=p.id JOIN file_map f ON o.file_id=f.id WHERE p.phrase='block_on' AND f.file_path LIKE '%runtime.rs'"
# Expected: > 0 (was 0 before fix)
```

---

## W4: Improve `reliary_dead_symbols` discoverability

**Files:** `crates/reliary-agent/src/mcp.rs:83, 843-855`

**Root cause:** Tool description is generic. Default limit is 100 (already correct),
but LLM may call with smaller limit expecting dead code to be top-N. The first
20 results are sorted by `o.occ_id` (insertion order), not by likelihood — so
the most likely dead code may not be at the top.

**Fix sketch:**

### 1. Update tool description (mcp.rs:83)

Change description to be more directive:
```rust
"name": "reliary_dead_symbols",
"description": "Find unused functions and methods. Returns (stem, file, line, col) tuples where the symbol appears as a definition but has no other occurrences in the codebase. Use limit=100+ for cross-project scans of large repos. For ranked confidence (high/medium/low), use reliary_dead instead."
```

### 2. Sort by likelihood instead of occ_id

In `crates/reliary-search/src/symbol.rs:906-911`, change the ORDER BY:

```rust
"SELECT o.phrase_id, f.file_path, o.line, o.col, o.block_id
 FROM occurrence o JOIN file_map f ON f.id = o.file_id
 WHERE o.is_def = 1
 ORDER BY (o.tag = 1) DESC,         -- fn_def first, then methods
          LENGTH(f.file_path) ASC,  -- shorter paths first (closer to src root)
          o.occ_id"
```

This puts actual function definitions at the top, with shorter file paths first
(more likely to be public helpers).

### 3. Tests

Verify that `reliary_dead_symbols` with limit=20 for `io/util/` returns mostly
functions in that directory (not tests or other modules).

---

## W5: System prompt hint for cond=A

**Files:** `crates/reliary-agent/src/multi_turn_harness.py:RELIARY_SYS` (or wherever the system prompt is defined)

**Root cause:** LLM doesn't know which tool maps to which question type. For
q4 ("trace call chain"), it should use `reliary_callgraph_v2` (or
`reliary_trace_path`). For q10 ("dead code"), it should use `reliary_dead_symbols`
or `reliary_dead`.

**Fix sketch:**

### 1. Add a tool-selection cheat-sheet to RELIARY_SYS

Find the RELIARY_SYS string in `multi_turn_harness.py` (around line 200-300)
and add a paragraph at the end:

```python
RELIARY_SYS += """

Tool selection cheat-sheet:
- 'Where is X defined?' -> reliary_goto_def
- 'Who calls X?' / 'What does X call?' -> reliary_callgraph_v2 (pass anchor_file/anchor_line from goto_def for accuracy)
- 'List all callers of X' -> reliary_callgraph_v2 with summary=true
- 'Trace a call chain through X' -> reliary_trace_path (pass direction='outbound' or 'both', depth=2)
- 'Find functions in module M never called' -> reliary_dead_symbols with limit=100+
- 'Find unused code with confidence ranking' -> reliary_dead
- 'Show methods on Type T' -> reliary_methods_on
- 'Find calls matching a pattern (foo.bar, x + y, etc)' -> reliary_query_ast
"""
```

### 2. Tests

Run long bench seed=42 cond=A and verify q4 and q10 scores improve.

---

## Validation plan

After all 5 fixes:

1. **cargo build** + **cargo test** — should stay clean (207/207 tests pass)
2. **Re-index tokio** to pick up W3 fix
3. **Run long_session_bench.py --conditions A --seeds 42** — should show lift
4. **Expected outcomes:**
   - q4 block_on_chain: 1/3 -> 2-3/3 (W1 anchor disambiguation + W3 classifier)
   - q10 dead_code: 0/3 -> 2-3/3 (W4 better discovery + W5 prompt hint)
   - Other queries: same or slightly better
   - Total: 24.5/30 baseline -> 26-27/30 expected

5. **Run long_session_bench.py --conditions D** — verify pack + W1-W5 combine well

---

## Risk assessment

| Fix | Risk | Mitigation |
|-----|------|------------|
| W1 | Low — additive schema fields | Backward compat: existing callers ignore new fields |
| W2 | Low — adds fallback path | Only triggers when stem match fails |
| W3 | Medium — changes classification | Add tests, verify W3 doesn't break existing def detection |
| W4 | Very low — reorders existing query | Same data, better sort |
| W5 | None — prompt text change | No code change, can revert instantly |

Total risk: LOW. Estimated time: ~8 hours including validation.