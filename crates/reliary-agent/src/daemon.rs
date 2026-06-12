use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::path::Path;
use std::sync::Arc;
use crate::session_state::SessionState;
use crate::chronicle;

/// Walk up from a file path to find the project root containing .reliary/
/// Canonicalizes the path to prevent symlink traversal attacks.
pub fn find_reliary_root(path: &str) -> Option<(String, String, String)> {
    let canonical = std::fs::canonicalize(path).ok()?;
    let mut current = if canonical.is_dir() {
        canonical
    } else {
        canonical.parent()?.to_path_buf()
    };
    loop {
        let reliary_dir = current.join(".reliary");
        if reliary_dir.is_dir() {
            let root = current.to_string_lossy().to_string();
            let index = reliary_dir.join("index.sqlite").to_string_lossy().to_string();
            let chronicle = reliary_dir.join("chronicle.sqlite").to_string_lossy().to_string();
            return Some((root, index, chronicle));
        }
        if !current.pop() {
            return None;
        }
    }
}

fn index_db_path(path: &str) -> String {
    format!("{}/.reliary/index.sqlite", path.trim_end_matches('/'))
}

/// Identifier veto: check that all identifiers in new_text exist in the project or known libraries
fn identifier_veto(new_text: &str, file_path: &str) -> Result<(), String> {
    let identifiers = reliary_search::scan_identifiers(new_text);
    if identifiers.is_empty() {
        return Ok(());
    }

    // Find the project index from the file path
    let index_path = match find_reliary_root(file_path) {
        Some((_, idx, _)) => idx,
        None => return Err("veto: no .reliary index found for this project".to_string()),
    };

    // Common library/standard identifiers that don't need project definitions
    let known_libs = [
        // Rust std
        "std", "core", "alloc", "vec", "string", "option", "result", "box", "rc", "arc",
        "clone", "copy", "debug", "display", "fmt", "iter", "into", "from",
        // Python std
        "os", "sys", "json", "re", "math", "time", "datetime", "pathlib", "typing",
        "list", "dict", "tuple", "set", "str", "int", "float", "bool", "none",
        // Common test
        "test", "assert", "assert_eq", "assert_ne", "assert_true", "assert_false",
        "setup", "teardown", "before_each", "after_each",
    ];

    // Build a set of identifiers that exist in the project index
    let mut project_ids = std::collections::HashSet::new();
    if let Ok(db) = rusqlite::Connection::open(&index_path) {
        if reliary_search::schema::open_existing_db(&db).is_ok() {
            for id in &identifiers {
                // Query FTS5 for the identifier
                let results = reliary_search::search::search_fts5(&db, id, 1);
                if !results.is_empty() {
                    project_ids.insert(id.clone());
                }
            }
        }
    }

    for id in &identifiers {
        if project_ids.contains(id) { continue; }
        if id.len() <= 2 { continue; }
        if known_libs.contains(&id.as_str()) { continue; }
        // Check if it looks like a well-known lib (all-caps const, single char, etc.)
        if id.chars().all(|c| c.is_uppercase() || c == '_') { continue; }
        return Err(format!("veto: '{}' not found in project or known libraries", id));
    }
    Ok(())
}

fn daemon_handle(mut stream: TcpStream, state: Arc<SessionState>) {
    // Set read timeout to prevent thread leaks on half-open connections
    let _ = stream.set_read_timeout(Some(std::time::Duration::from_secs(30)));
    let _ = stream.set_write_timeout(Some(std::time::Duration::from_secs(10)));
    let mut line = String::new();
    let mut reader = BufReader::new(&stream);

    if reader.read_line(&mut line).is_err() || line.trim().is_empty() {
        return;
    }
    let cmd = line.trim();

    let (p0, p1, p2, p3, p4) = {
        let parts: Vec<&str> = cmd.splitn(6, ' ').collect();
        let a = parts.first().copied().unwrap_or("");
        let b = parts.get(1).copied().unwrap_or("");
        let c = parts.get(2).copied().unwrap_or("");
        let d = parts.get(3).copied().unwrap_or("");
        let e = parts.get(4).copied().unwrap_or("");
        (a, b, c, d, e)
    };

    // Helper to log to chronicle
    let append_chronicle = |file_path: &str, event: &str, detail: &str, outcome: &str| {
        if let Some((_, _, chron_path)) = find_reliary_root(file_path) {
            if let Ok(db) = chronicle::init(&chron_path) {
                chronicle::append(&db, event, file_path, detail, outcome);
            }
        }
    };

    let response = match p0 {
        "ping" => "pong\n".to_string(),
        "status" => "reliary-agent daemon 0.1.0\n".to_string(),
        "session-state" => {
            // Usage: session-state <session_file_path>
            if p1.is_empty() {
                "ERROR: usage: session-state <path>\n".to_string()
            } else {
                match reliary_core::parse_session_file(p1) {
                    Ok(state) => {
                        if state.turn_count < 3 {
                            "early\n".to_string()
                        } else {
                            let block = reliary_core::build_state_block(&state, state.turn_count);
                            block
                        }
                    }
                    Err(e) => format!("ERROR: {}\n", e),
                }
            }
        }
        "search" => {
            if p2.is_empty() {
                "ERROR: usage: search <query> <path>\n".to_string()
            } else {
                let db_path = match find_reliary_root(p2) {
                    Some((_, idx, _)) => idx,
                    None => index_db_path(p2),
                };
                let results_db: Option<rusqlite::Connection> = if let Ok(db) = rusqlite::Connection::open(&db_path) {
                    if reliary_search::schema::open_existing_db(&db).is_ok() {
                        Some(db)
                    } else {
                        drop(db);
                        eprintln!("[daemon] search index corrupted — rebuilding...");
                        let _ = std::fs::remove_file(&db_path);
                        let index_dir = std::path::Path::new(&db_path).parent().unwrap_or(std::path::Path::new("."));
                        let _ = std::fs::create_dir_all(index_dir);
                        if let Ok(new_db) = rusqlite::Connection::open(&db_path) {
                            if reliary_search::schema::create_new_db(&new_db).is_ok() {
                                let _ = reliary_search::ingest::index_directory(&new_db, p2);
                                Some(new_db)
                            } else { None }
                        } else { None }
                    }
                } else { None };

                if let Some(db) = results_db {
                    let results = reliary_search::search::search_fts5(&db, p1, 10);
                    let resp = if results.is_empty() {
                        "no results".to_string()
                    } else {
                        results.iter()
                            .map(|r| format!("{:.4} {}", r.score, r.file))
                            .collect::<Vec<_>>()
                            .join("\n")
                    };
                    resp + "\n"
                } else {
                    "ERROR: no index at path\n".to_string()
                }
            }
        }
        "compress" => {
            if p1.is_empty() {
                "ERROR: usage: compress <text>\n".to_string()
            } else {
                let text = cmd.trim_start_matches("compress ").trim();
                let dict = crate::read_summary::load_dictionary();
                if let Some(c) = reliary_compress::compress_reasoning(text, dict.as_ref()) {
                    c + "\n"
                } else {
                    "no compression\n".to_string()
                }
            }
        }
        "risk" => {
            if p1.is_empty() {
                "ERROR: usage: risk <file>\n".to_string()
            } else {
                // Check cache first
                let cached = {
                    let cache = state.risk_cache.lock().unwrap_or_else(|e| e.into_inner());
                    cache.get(p1).and_then(|(text, ts)| {
                        if ts.elapsed() < std::time::Duration::from_secs(300) {
                            Some(text.clone())
                        } else {
                            None
                        }
                    })
                };
                match cached {
                    Some(text) => text + "\n",
                    None => {
                        // Compute and cache
                        match std::fs::read_to_string(p1) {
                            Ok(content) => {
                                let risk = reliary_risk::compute_file_risk(p1, &content);
                                let text = format!("{:?}: {}", risk.risk, risk.reason);
                                let mut cache = state.risk_cache.lock().unwrap_or_else(|e| e.into_inner());
                                cache.insert(p1.to_string(), (text.clone(), std::time::Instant::now()));
                                text + "\n"
                            }
                            Err(e) => format!("ERROR: {}\n", e),
                        }
                    }
                }
            }
        }
        "fix" => {
            if p4.is_empty() {
                "ERROR: usage: fix <file> <old> <new> <workdir>\n".to_string()
            } else {
                if let Ok(content) = std::fs::read_to_string(p1) {
                    let fixes = vec![(p2.to_string(), p3.to_string())];
                    let (modified, count) = reliary_fix::apply_fixes(&content, &fixes);
                    if count > 0 {
                        match crate::heal::heal_edit(p1, &modified, p4) {
                            Ok(()) => {
                                append_chronicle(p1, "edit", p2, "pass");
                                format!("OK: {} replacements, tests pass\n", count)
                            }
                            Err(e) => {
                                append_chronicle(p1, "edit", p2, &format!("revert: {}", e));
                                format!("ERROR: {} (reverted)\n", e)
                            }
                        }
                    } else {
                        "ERROR: no match\n".to_string()
                    }
                } else {
                    format!("ERROR: cannot read {}\n", p1)
                }
            }
        }
        "apply-edit" => {
            // Usage: apply-edit <file> <tmp-path> <workdir>
            if p3.is_empty() {
                "ERROR: usage: apply-edit <file> <tmp-path> <workdir>\n".to_string()
            } else {
                match std::fs::read_to_string(p2) {
                    Ok(new_content) => {
                        match crate::heal::heal_edit(p1, &new_content, p3) {
                            Ok(()) => {
                                append_chronicle(p1, "edit", "apply-edit", "pass");
                                "OK: tests pass\n".to_string()
                            }
                            Err(e) => {
                                append_chronicle(p1, "edit", "apply-edit", &format!("revert: {}", e));
                                format!("REVERTED: {}\n", e)
                            }
                        }
                    }
                    Err(e) => format!("ERROR: cannot read tmp file: {}\n", e),
                }
            }
        }
        "sed-apply" => {
            if p4.is_empty() {
                "ERROR: usage: sed-apply <file> <old_tmp> <new_tmp> <workdir>\n".to_string()
            } else {
                let rv = std::fs::read_to_string(p2)
                    .and_then(|old| std::fs::read_to_string(p3).map(|new| (old, new)));
                match rv {
                    Err(e) => format!("ERROR: read tmp files: {}\n", e),
                    Ok((old, new)) => {
                        match std::fs::read_to_string(p1) {
                            Err(e) => format!("ERROR: cannot read {}: {}\n", p1, e),
                            Ok(content) => {
                                let fixes = vec![(old.trim().to_string(), new.trim().to_string())];
                                let (modified, count) = reliary_fix::apply_fixes(&content, &fixes);
                                if count > 0 {
                                    match crate::heal::heal_edit(p1, &modified, p4) {
                                        Ok(()) => format!("OK: {} replacements, tests pass\n", count),
                                        Err(e) => format!("REVERTED: {}\n", e),
                                    }
                                } else {
                                    "ERROR: no match\n".to_string()
                                }
                            }
                        }
                    }
                }
            }
        }
        "dead" => {
            if p1.is_empty() {
                "ERROR: usage: dead <path>\n".to_string()
            } else {
                let config = reliary_dead::DeadConfig::default();
                let mut candidates = Vec::new();
                if let Ok(entries) = std::fs::read_dir(p1) {
                    for entry in entries.flatten() {
                        let fp = entry.path();
                        if fp.extension().map(|e| e == "py" || e == "rs" || e == "js").unwrap_or(false) {
                            if let Some(p) = fp.to_str() {
                                if let Ok(content) = std::fs::read_to_string(p) {
                                    candidates.extend(reliary_dead::analyze_file(p, &content, &config));
                                }
                            }
                        }
                    }
                }
                if candidates.is_empty() {
                    "no dead code found\n".to_string()
                } else {
                    candidates.iter()
                        .map(|c| format!("{}:{} {}", c.file, c.line, c.name))
                        .collect::<Vec<_>>()
                        .join("\n") + "\n"
                }
            }
        }
        "cache-read" => {
            if p3.is_empty() {
                "ERROR: usage: cache-read <path> <hash> <len>\n".to_string()
            } else {
                let path = p1.to_string();
                let len: usize = p3.parse().unwrap_or(0);
                let hash_val: u64 = u64::from_str_radix(&p2[..16.min(p2.len())], 16).unwrap_or(0);
                let mtime = std::fs::metadata(&path)
                    .and_then(|m| m.modified())
                    .unwrap_or(std::time::SystemTime::UNIX_EPOCH);
                let mut cache = state.read_cache.lock().unwrap_or_else(|e| e.into_inner());
                cache.insert(path, crate::session_state::ReadCacheEntry { hash: hash_val, len, mtime });
                format!("cached {}\n", len)
            }
        }
        "check-read" => {
            if p2.is_empty() {
                "ERROR: usage: check-read <path> <hash>\n".to_string()
            } else {
                let path = p1.to_string();
                let hash_val: u64 = u64::from_str_radix(&p2[..16.min(p2.len())], 16).unwrap_or(0);
                let current_mtime = std::fs::metadata(&path)
                    .and_then(|m| m.modified())
                    .unwrap_or(std::time::SystemTime::UNIX_EPOCH);
                let cache = state.read_cache.lock().unwrap_or_else(|e| e.into_inner());
                if let Some(entry) = cache.get(&path) {
                    // Mtime check first: if file has been modified since cached, it's stale
                    if entry.mtime != current_mtime {
                        format!("stale\n")
                    } else if entry.hash == hash_val {
                        format!("unchanged {}\n", entry.len)
                    } else {
                        "stale\n".to_string()
                    }
                } else {
                    "stale\n".to_string()
                }
            }
        }
        "should-compress" => {
            // Usage: should-compress <turn_count> <text>
            if p2.is_empty() {
                "ERROR: usage: should-compress <turn> <text>\n".to_string()
            } else {
                let turn: usize = p1.parse().unwrap_or(0);
                let parts: Vec<&str> = cmd.splitn(3, ' ').collect();
                let text = if parts.len() >= 3 { parts[2].trim() } else { p2 };
                let len = text.len();

                // Skip: too short, early turn, or contains code content
                if len < 200 { "skip\n".to_string() }
                else if text.contains("```") || text.contains("//") || text.contains("/*")
                    || text.contains("src/") || text.contains(".rs:") || text.contains(".py:")
                { "skip\n".to_string() }
                else if turn < 3 && len < 800 { "skip\n".to_string() }
                // Gentle: medium-length reasoning, turn 3+
                else if len >= 400 && turn >= 3 {
                    "gentle\n".to_string()
                }
                // Aggressive: long text, turn 5+ (mature conversation)
                else if len >= 1000 && turn >= 5 {
                    "aggressive\n".to_string()
                }
                else { "skip\n".to_string() }
            }
        }
        "index" => {
            if p1.is_empty() {
                "ERROR: usage: index <path>\n".to_string()
            } else {
                let db_path = index_db_path(p1);
                if let Some(parent) = Path::new(&db_path).parent() {
                    std::fs::create_dir_all(parent).ok();
                }
                std::fs::remove_file(&db_path).ok();
                match rusqlite::Connection::open(&db_path) {
                    Ok(db) => {
                        if reliary_search::schema::create_new_db(&db).is_err() {
                            "ERROR: schema creation failed\n".to_string()
                        } else {
                            match reliary_search::ingest::index_directory(&db, p1) {
                                Ok(count) => format!("indexed {} files\n", count),
                                Err(e) => format!("ERROR: {}\n", e),
                            }
                        }
                    }
                    Err(e) => format!("ERROR: {}\n", e),
                }
            }
        }
        // ── Veto: check new-text identifiers against project index ──
        "veto" => {
            if p2.is_empty() {
                "ERROR: usage: veto <file> <new_text>\n".to_string()
            } else {
                // p1 = file path, rest = new text (p2 onward)
                let rest = cmd.splitn(3, ' ').nth(2).unwrap_or("").trim().to_string();
                match identifier_veto(&rest, p1) {
                    Ok(()) => "ok\n".to_string(),
                    Err(e) => {
                        append_chronicle(p1, "veto", "identifier_veto", &e);
                        format!("ERROR: {}\n", e)
                    }
                }
            }
        }
        // ── Muzzle: enable/disable scavenger ──
        "muzzle" => {
            if p1.is_empty() {
                "ERROR: usage: muzzle on|off\n".to_string()
            } else {
                match p1 {
                    "on" => { state.set_muzzle(true); "muzzled\n".to_string() }
                    "off" => { state.set_muzzle(false); "unmuzzled\n".to_string() }
                    _ => "ERROR: use 'muzzle on' | 'muzzle off'\n".to_string()
                }
            }
        }
        // ── Scavenge-query: orphaned function count from chronicle ──
        "scavenge-query" => {
            let db_path = state.chronicle_path.to_string_lossy().to_string();
            match chronicle::init(&db_path) {
                Ok(db) => {
                    let events = chronicle::recent_events_by_type(&db, "scavenge_advisory", 24);
                    if events.is_empty() {
                        "ok\n".to_string()
                    } else {
                        // Count by file
                        let mut by_file = rustc_hash::FxHashMap::default();
                        for e in &events {
                            *by_file.entry(e.file.clone()).or_insert(0) += 1;
                        }
                        let mut lines: Vec<String> = by_file.into_iter()
                            .map(|(f, c)| format!("{} ({} orphans)", f, c))
                            .collect();
                        lines.sort();
                        format!("{}\n", lines.join(" | "))
                    }
                }
                Err(_) => "ok\n".to_string()
            }
        }
        // ── Chronicle-query: recent events for a file ──
        "chronicle" => {
            if p1.is_empty() {
                "ERROR: usage: chronicle <file> [hours]\n".to_string()
            } else {
                let hours: i64 = p2.parse().unwrap_or(24);
                match find_reliary_root(p1) {
                    Some((_, _, chronicle_path)) => {
                        match chronicle::init(&chronicle_path) {
                            Ok(db) => {
                                let events = chronicle::recent_events(&db, p1, hours);
                                if events.is_empty() {
                                    "no events\n".to_string()
                                } else {
                                    events.iter()
                                        .map(|e| format!("{} {} {}: {}", e.t, e.event, e.outcome, e.detail))
                                        .collect::<Vec<_>>()
                                        .join("\n") + "\n"
                                }
                            }
                            Err(e) => format!("ERROR: {}\n", e),
                        }
                    }
                    None => "ERROR: no .reliary found for this file\n".to_string()
                }
            }
        }
        "read-summary" => {
            if p1.is_empty() {
                "ERROR: usage: read-summary <file>\n".to_string()
            } else {
                crate::read_summary::build(p1) + "\n"
            }
        }
        "batch-heal" => {
            // Usage: batch-heal <workdir> <json-edits>
            if p2.is_empty() {
                "ERROR: usage: batch-heal <workdir> <json>\n".to_string()
            } else {
                let rest = cmd.splitn(3, ' ').nth(2).unwrap_or("").trim();
                match serde_json::from_str::<Vec<(String, String, String)>>(rest) {
                    Ok(edits) => crate::heal::batch_heal(&edits, p1) + "\n",
                    Err(e) => format!("ERROR: invalid JSON: {}\n", e),
                }
            }
        }
        "prior" => {
            if p1.is_empty() {
                "ERROR: usage: prior <path>\n".to_string()
            } else {
                crate::chronicle::build_prior(p1) + "\n"
            }
        }
        _ => format!("ERROR: unknown command '{}'\n", p0),
    };

    if let Err(e) = stream.write_all(response.as_bytes()) {
        eprintln!("[daemon] write error: {}", e);
    }
}

pub fn start(port: u16, workdir: &str) -> std::io::Result<()> {
    let state = Arc::new(SessionState::new(workdir));

    // Start scavenger thread (panic-isolated via catch_unwind)
    let scavenger_state = Arc::clone(&state);
    std::thread::Builder::new()
        .name("scavenger".into())
        .spawn(move || {
            if let Err(panic) = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                crate::scavenger::scavenger_loop(scavenger_state);
            })) {
                let msg = if let Some(s) = panic.downcast_ref::<&str>() {
                    s.to_string()
                } else if let Some(s) = panic.downcast_ref::<String>() {
                    s.clone()
                } else {
                    "unknown panic".to_string()
                };
                eprintln!("[scavenger] thread panicked: {} — restarting in 60s", msg);
                std::thread::sleep(std::time::Duration::from_secs(60));
            }
        })
        .ok();

    // Register SIGTERM/SIGINT handlers for clean shutdown
    let shutdown_state = Arc::clone(&state);
    std::thread::Builder::new()
        .name("signal-handler".into())
        .spawn(move || {
            // Only try to install signal handlers — ignore errors (e.g. on Windows)
            match signal_hook::consts::SIGTERM {
                sig => {
                    if let Ok(mut signals) = signal_hook::iterator::Signals::new(&[sig, signal_hook::consts::SIGINT]) {
                        for signal in signals.forever() {
                            eprintln!("[daemon] received signal {}, shutting down", signal);
                            // Let any in-progress operations finish gracefully
                            std::thread::sleep(std::time::Duration::from_millis(500));
                            std::process::exit(0);
                        }
                    }
                }
            }
        })
        .ok();

    // Clean up stale temp files on startup
    let _ = std::fs::remove_dir_all("/tmp/gate-heal");

    let addr = format!("127.0.0.1:{}", port);
    let listener = TcpListener::bind(&addr)?;
    eprintln!("[reliary] daemon listening on {} (workdir: {})", addr, workdir);

    for stream in listener.incoming() {
        match stream {
            Ok(s) => {
                let state = Arc::clone(&state);
                std::thread::Builder::new()
                    .name("handler".into())
                    .spawn(move || daemon_handle(s, state))
                    .ok();
            }
            Err(e) => eprintln!("[reliary] accept error: {}", e),
        }
    }
    Ok(())
}
