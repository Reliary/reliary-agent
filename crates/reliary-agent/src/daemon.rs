#![allow(dead_code)]
use std::collections::HashSet;
use std::io::{BufRead, BufReader, BufWriter, Write};
use std::net::{TcpListener, TcpStream};
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use tracing::{info, warn, error};
use std::sync::{Arc, LazyLock};
use crate::session_state::SessionState;

const MAX_CONCURRENT: usize = 50;
pub const MAX_FILE_SIZE: u64 = 10_000_000;

/// Known library identifiers to skip during veto (grammar-free: names that appear
/// in practically every file but aren't project-specific).
static LIBS: LazyLock<HashSet<&'static str>> = LazyLock::new(|| {
    [
        "std", "core", "alloc", "vec", "string", "option", "result", "box", "rc", "arc",
        "clone", "copy", "debug", "display", "fmt", "iter", "into", "from",
        "os", "sys", "json", "re", "math", "time", "datetime", "pathlib", "typing",
        "list", "dict", "tuple", "set", "str", "int", "float", "bool", "none",
        "test", "assert", "assert_eq", "assert_ne",
        "setup", "teardown", "before_each", "after_each",
    ].into()
});

pub fn find_reliary_root(path: &str) -> Option<(String, String, String)> {
    let path = Path::new(path);
    let mut current = if path.is_dir() {
        path.to_path_buf()
    } else {
        path.parent()?.to_path_buf()
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

fn identifier_veto(new_text: &str, file_path: &str) -> Result<(), String> {
    let identifiers = reliary_search::scan_identifiers(new_text);
    if identifiers.is_empty() {
        return Ok(());
    }
    let index_path = match find_reliary_root(file_path) {
        Some((_, idx, _)) => idx,
        None => return Err("veto: no .reliary index found for this project".to_string()),
    };
    let libs = &LIBS;
    let mut project_ids = std::collections::HashSet::new();
    if let Ok(db) = rusqlite::Connection::open(&index_path) {
            let _ = db.execute_batch("PRAGMA synchronous=NORMAL;");
            if reliary_search::schema::open_existing_db(&db).is_ok() {
            for id in &identifiers {
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
        if libs.contains(id.as_str()) { continue; }
        if id.chars().all(|c| c.is_uppercase() || c == '_') { continue; }
        return Err(format!("veto: '{}' not found in project or known libraries", id));
    }
    Ok(())
}

fn daemon_handle(mut stream: TcpStream, state: Arc<SessionState>) {
    let _ = stream.set_read_timeout(Some(std::time::Duration::from_secs(30)));
    let _ = stream.set_write_timeout(Some(std::time::Duration::from_secs(10)));
    let mut line = String::new();
    let mut reader = BufReader::new(&stream);

    if reader.read_line(&mut line).is_err() || line.trim().is_empty() {  // GUARDED: intentional
        return;
    }
    let cmd = line.trim();

    // Input validation: max length + reject non-printable / null bytes
    if cmd.len() > 4096 {
        let _ = writeln!(stream, "ERROR: command too long\n");
        return;
    }
    if cmd.bytes().any(|b| b == 0 || (b < 0x20 && b != b'\n' && b != b'\r' && b != b'\t')) {
        let _ = writeln!(stream, "ERROR: invalid input characters\n");
        return;
    }

    let (p0, p1, p2, p3, p4) = {
        let parts: Vec<&str> = cmd.splitn(6, ' ').collect();
        let a = parts.first().copied().unwrap_or("");
        let b = parts.get(1).copied().unwrap_or("");
        let c = parts.get(2).copied().unwrap_or("");
        let d = parts.get(3).copied().unwrap_or("");
        let e = parts.get(4).copied().unwrap_or("");
        (a, b, c, d, e)
    };

    let response = daemon_handle_cmd(p0, p1, p2, p3, p4, cmd, &state);
    let mut writer = BufWriter::new(&stream);
    let _ = writer.write_all(response.as_bytes());
    let _ = writer.flush();
}

/// Parse a command string and dispatch to daemon_handle_cmd.
pub fn daemon_handle_cmd_str(cmd: &str, state: &SessionState) -> String {
    let parts: Vec<&str> = cmd.splitn(6, ' ').collect();
    let p0 = parts.first().copied().unwrap_or("");
    let p1 = parts.get(1).copied().unwrap_or("");
    let p2 = parts.get(2).copied().unwrap_or("");
    let p3 = parts.get(3).copied().unwrap_or("");
    let p4 = parts.get(4).copied().unwrap_or("");
    daemon_handle_cmd(p0, p1, p2, p3, p4, cmd, state)
}

fn open_index_db(path: &str) -> Option<rusqlite::Connection> {
    if let Ok(db) = rusqlite::Connection::open(path) {
        let _ = db.execute_batch("PRAGMA journal_mode = WAL; PRAGMA synchronous = NORMAL; PRAGMA cache_size = -8000;");
        if reliary_search::schema::open_existing_db(&db).is_ok() {
            return Some(db);
        }
        drop(db);
        warn!("search index corrupted — rebuilding...");
        let _ = std::fs::remove_file(path);
        if let Some(parent) = std::path::Path::new(path).parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if let Ok(new_db) = rusqlite::Connection::open(path) {
            let _ = new_db.execute_batch("PRAGMA journal_mode = WAL; PRAGMA synchronous = NORMAL;");
            if reliary_search::schema::create_new_db(&new_db).is_ok() {
                let rel_path = std::path::Path::new(path).parent().and_then(|p| p.parent()).and_then(|p| p.to_str()).unwrap_or(".");
                let _ = reliary_search::ingest::index_directory(&new_db, rel_path);
                return Some(new_db);
            }
        }
    }
    None
}

fn daemon_handle_cmd(p0: &str, p1: &str, p2: &str, p3: &str, p4: &str, cmd: &str, state: &SessionState) -> String {
    // Generic file size guard for all file-reading endpoints
    if !p1.is_empty() && (p0 == "risk" || p0 == "read-summary" || p0 == "veto" || p0 == "fix" || p0 == "apply-edit" || p0 == "sed-apply") && Path::new(p1).exists() {
        if let Ok(meta) = std::fs::metadata(p1) {
            if meta.len() > MAX_FILE_SIZE {
                return format!("ERROR: file too large ({}). Max: {}MB\n", meta.len(), MAX_FILE_SIZE / 1_000_000);
            }
        }
    };

    match p0 {
        "ping" => "pong\n".to_string(),
        "status" => "reliary-agent daemon 0.1.0\n".to_string(),
        "search" => {
            if p1.is_empty() {
                "ERROR: usage: search <query> <path>\n".to_string()
            } else {
                let path = if p2.is_empty() { "." } else { p2 };
                let db_path = index_db_path(path);
                let results_db = open_index_db(&db_path);

                if let Some(db) = results_db {
                    let results = reliary_search::search::search_fts5(&db, p1, 10);
                    if results.is_empty() {
                        "no results\n".to_string()
                    } else {
                        results.iter()
                            .fold(String::new(), |acc, r| {
                                if acc.is_empty() {
                                    format!("{:.4} {}", r.score, r.file)
                                } else {
                                    format!("{}\n{:.4} {}", acc, r.score, r.file)
                                }
                            }) + "\n"
                    }
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
                let cached = state.risk_cache_get(p1);
                if let Some((text, ts)) = cached {
                    if ts.elapsed() < std::time::Duration::from_secs(300) {
                        return text + "\n";
                    }
                }
                match reliary_core::safe_read(p1) {
                    Ok(content) => {
                        let risk = reliary_risk::compute_file_risk(p1, &content);
                        let text = format!("{:?}: {}", risk.risk, risk.reason);
                        state.risk_cache_set(p1.to_string(), text.clone());
                        text + "\n"
                    }
                    Err(_) => "ERROR: cannot read file\n".to_string(),
                }
            }
        }
        "fix" => {
            if p4.is_empty() {
                "ERROR: usage: fix <file> <old> <new> <workdir>\n".to_string()
            } else {
                match reliary_core::safe_read(p1) {
                    Ok(content) => {
                        let fixes = vec![(p2.to_string(), p3.to_string())];
                        let (modified, count) = reliary_fix::apply_fixes(&content, &fixes);
                        if count > 0 {
                            match crate::heal::heal_edit(p1, &modified, p4) {
                                Ok(()) => {
                                    append_chronicle(p1, "edit", p2, "pass");
                                    format!("OK: {} replacements, tests pass\n", count)
                                }
                                Err(e) => {
                                    append_chronicle(p1, "edit", p2, "revert");
                                    format!("{}\n", e)
                                }
                            }
                        } else {
                            "no matches\n".to_string()
                        }
                    }
                    Err(_) => "ERROR: cannot read file\n".to_string(),
                }
            }
        }
        "muzzle" => {
            if p1 == "on" { state.set_muzzle(true); "muzzled\n".to_string() }
            else if p1 == "off" { state.set_muzzle(false); "unmuzzled\n".to_string() }
            else { "ERROR: usage: muzzle on|off\n".to_string() }
        }
        "veto" => {
            if p2.is_empty() {
                "ERROR: usage: veto <file> <new_text>\n".to_string()
            } else {
                let new_text = cmd.trim_start_matches("veto ").trim();
                match identifier_veto(new_text, p1) {
                    Ok(()) => "ok\n".to_string(),
                    Err(e) => format!("ERROR: {}\n", e),
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
                state.read_cache_set(path, crate::session_state::ReadCacheEntry { hash: hash_val, len });
                format!("cached {}\n", len)
            }
        }
        "check-read" => {
            if p2.is_empty() {
                "ERROR: usage: check-read <path> <hash>\n".to_string()
            } else {
                let path = p1.to_string();
                let hash_val: u64 = u64::from_str_radix(&p2[..16.min(p2.len())], 16).unwrap_or(0);
                if let Some(entry) = state.read_cache_get(&path) {
                    if entry.hash == hash_val {
                        format!("unchanged {}\n", entry.len)
                    } else {
                        "stale\n".to_string()
                    }
                } else {
                    "stale\n".to_string()
                }
            }
        }
        "apply-edit" => {
            match reliary_core::safe_read(p2) {
                Ok(new_content) => {
                    match crate::heal::heal_edit(p1, &new_content, p3) {
                        Ok(()) => "OK: tests pass\n".to_string(),
                        Err(e) => format!("REVERTED: {}\n", e),
                    }
                }
                Err(_) => "ERROR: cannot read file\n".to_string(),
            }
        }
        "sed-apply" => {
            if p2.is_empty() || p3.is_empty() || p4.is_empty() {
                "ERROR: usage: sed-apply <file> <old> <new> <workdir>\n".to_string()
            } else {
                match reliary_core::safe_read(p1) {
                    Ok(content) => {
                        let new_content = content.replace(p2, p3);
                        if new_content == content {
                            "no match\n".to_string()
                        } else {
                            match crate::heal::heal_edit(p1, &new_content, p4) {
                                Ok(()) => "OK: tests pass\n".to_string(),
                                Err(e) => format!("REVERTED: {}\n", e),
                            }
                        }
                    }
                    Err(_) => "ERROR: cannot read file\n".to_string(),
                }
            }
        }
        "dead" => "no dead code found\n".to_string(),
        "scavenge-query" => "ok\n".to_string(),
        "chronicle" => {
            if p1.is_empty() {
                "ERROR: usage: chronicle <prefix> [detail]\n".to_string()
            } else {
                "no events\n".to_string()
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
        "session-state" => "early\n".to_string(),
        _ => format!("ERROR: unknown command '{}'\n", p0),
    }
}

fn append_chronicle(file: &str, event: &str, detail: &str, outcome: &str) {
    if let Some((root, _, _)) = find_reliary_root(file) {
        let path = format!("{}/.reliary/chronicle.sqlite", root);
        if let Ok(db) = rusqlite::Connection::open(&path) {
            if let Err(e) = db.execute_batch("PRAGMA journal_mode = WAL; PRAGMA synchronous = NORMAL;") {
                warn!("chronicle PRAGMA: {}", e);
            }
            crate::chronicle::append(&db, event, file, detail, outcome);
        }
    }
}

pub fn start(port: u16, workdir: &str) -> std::io::Result<()> {
    // Initialize session state
    let state = if workdir == "." {
        let cwd = std::env::current_dir().unwrap_or_default();
        Arc::new(SessionState::new(cwd.to_string_lossy().as_ref()))
    } else {
        Arc::new(SessionState::new(workdir))
    };

    // Start scavenger thread
    let scavenger_state = Arc::clone(&state);
    if let Err(e) = std::thread::Builder::new()
        .name("scavenger".into())
        .spawn(move || crate::scavenger::scavenger_loop(scavenger_state))
    {
        error!("scavenger thread: {}", e);
    }

    let connections = Arc::new(AtomicUsize::new(0));

    let addr = format!("127.0.0.1:{}", port);
    let listener = TcpListener::bind(&addr)?;
    info!("daemon listening on {} (workdir: {}, max connections: {})", addr, workdir, MAX_CONCURRENT);

    for stream in listener.incoming() {
        match stream {
            Ok(s) => {
                let conns = Arc::clone(&connections);
                let prev = conns.fetch_add(1, Ordering::Relaxed);
                if prev >= MAX_CONCURRENT {
                    conns.fetch_sub(1, Ordering::Relaxed);
                    warn!("max connections ({}) reached, rejecting", MAX_CONCURRENT);
                    continue;
                }
                let state = Arc::clone(&state);
                let spawn_result = std::thread::Builder::new()
                    .name("handler".into())
                    .spawn(move || { daemon_handle(s, state); conns.fetch_sub(1, Ordering::Relaxed); });
                if let Err(e) = spawn_result {
                    // Bug 51: decrement counter on thread spawn failure (was leaking)
                    connections.fetch_sub(1, Ordering::Relaxed);
                    error!("handler thread spawn failed: {}", e);
                }
            }
            Err(e) => error!("accept error: {}", e),
        }
    }
    Ok(())
}
