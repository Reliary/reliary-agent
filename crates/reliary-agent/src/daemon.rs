/// TCP daemon on :9799. Processes line-delimited commands.
/// Simple protocol: one command per connection, response written back.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::path::Path;
use std::sync::{Mutex, OnceLock};

static READ_CACHE: OnceLock<Mutex<HashMap<String, (u64, usize)>>> = OnceLock::new();
fn read_cache() -> &'static Mutex<HashMap<String, (u64, usize)>> {
    READ_CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

fn index_db_path(path: &str) -> String {
    format!("{}/.reliary/index.sqlite", path.trim_end_matches('/'))
}

fn handle(mut stream: TcpStream) {
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
                let db_path = index_db_path(p2);
                if let Ok(db) = rusqlite::Connection::open(&db_path) {
                    if reliary_search::schema::open_existing_db(&db).is_ok() {
                        let results = reliary_search::search::search_fts5(&db, p1, 10);
                        if results.is_empty() {
                            "no results\n".to_string()
                        } else {
                            results.iter()
                                .map(|r| format!("{:.4} {}", r.score, r.file))
                                .collect::<Vec<_>>()
                                .join("\n") + "\n"
                        }
                    } else {
                        "ERROR: no index at path\n".to_string()
                    }
                } else {
                    "ERROR: cannot open DB\n".to_string()
                }
            }
        }
        "compress" => {
            if p1.is_empty() {
                "ERROR: usage: compress <text>\n".to_string()
            } else {
                let text = cmd.trim_start_matches("compress ").trim();
                if let Some(c) = reliary_compress::aggressive_compress(text) {
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
                match std::fs::read_to_string(p1) {
                    Ok(content) => {
                        let risk = reliary_risk::compute_file_risk(p1, &content);
                        format!("{:?}: {}\n", risk.risk, risk.reason)
                    }
                    Err(e) => format!("ERROR: {}\n", e),
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
                            Ok(()) => format!("OK: {} replacements, tests pass\n", count),
                            Err(e) => format!("ERROR: {} (reverted)\n", e),
                        }
                    } else {
                        "ERROR: no match\n".to_string()
                    }
                } else {
                    format!("ERROR: cannot read {}\n", p1)
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
                let mut cache = read_cache().lock().unwrap();
                cache.insert(path, (hash_val, len));
                format!("cached {}\n", len)
            }
        }
        "check-read" => {
            if p2.is_empty() {
                "ERROR: usage: check-read <path> <hash>\n".to_string()
            } else {
                let path = p1.to_string();
                let hash_val: u64 = u64::from_str_radix(&p2[..16.min(p2.len())], 16).unwrap_or(0);
                let cache = read_cache().lock().unwrap();
                if let Some((cached_hash, len)) = cache.get(&path) {
                    if *cached_hash == hash_val {
                        format!("unchanged {}\n", len)
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
        _ => format!("ERROR: unknown command '{}'\n", p0),
    };

    if let Err(e) = stream.write_all(response.as_bytes()) {
        eprintln!("[daemon] write error: {}", e);
    }
}

pub fn start(port: u16) -> std::io::Result<()> {
    let addr = format!("127.0.0.1:{}", port);
    let listener = TcpListener::bind(&addr)?;
    eprintln!("[daemon] listening on {}", addr);

    for stream in listener.incoming() {
        match stream {
            Ok(s) => { handle(s); }
            Err(e) => eprintln!("[daemon] accept error: {}", e),
        }
    }
    Ok(())
}
