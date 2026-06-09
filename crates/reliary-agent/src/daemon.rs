/// TCP daemon on :9799. Processes line-delimited JSON commands.
/// Each connection: read one command, respond, close.

use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::path::Path;

fn index_db_path(path: &str) -> String {
    format!("{}/.reliary/index.sqlite", path.trim_end_matches('/'))
}

fn handle(mut stream: TcpStream) {
    let peer = stream.peer_addr().ok();
    let mut line = String::new();
    let mut reader = BufReader::new(&stream);

    if reader.read_line(&mut line).is_err() || line.trim().is_empty() {
        return;
    }
    let cmd = line.trim();
    let parts: Vec<&str> = cmd.splitn(4, ' ').collect();
    let response = match parts[0] {
        "ping" => "pong\n".to_string(),
        "status" => "reliary-agent daemon 0.1.0\n".to_string(),
        "search" => {
            if parts.len() < 3 {
                "ERROR: usage: search <query> <path>\n".to_string()
            } else {
                let query = parts[1];
                let path = parts[2];
                let db_path = index_db_path(path);
                if let Ok(db) = rusqlite::Connection::open(&db_path) {
                    if reliary_search::schema::open_existing_db(&db).is_ok() {
                        let results = reliary_search::search::search_fts5(&db, query, 10);
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
            if parts.len() < 2 {
                "ERROR: usage: compress <text>\n".to_string()
            } else {
                let text = parts[1..].join(" ");
                if let Some(c) = reliary_compress::compress_reasoning(&text) {
                    c + "\n"
                } else {
                    "no compression\n".to_string()
                }
            }
        }
        "risk" => {
            if parts.len() < 2 {
                "ERROR: usage: risk <file>\n".to_string()
            } else {
                let file = parts[1];
                match std::fs::read_to_string(file) {
                    Ok(content) => {
                        let risk = reliary_risk::compute_file_risk(file, &content);
                        format!("{:?}: {}\n", risk.risk, risk.reason)
                    }
                    Err(e) => format!("ERROR: {}\n", e),
                }
            }
        }
        "fix" => {
            if parts.len() < 4 {
                "ERROR: usage: fix <file> <old> <new>\n".to_string()
            } else {
                let file = parts[1];
                let old = parts[2];
                let new = parts[3];
                match std::fs::read_to_string(file) {
                    Ok(content) => {
                        let fixes = vec![(old.to_string(), new.to_string())];
                        let (_, count) = reliary_fix::apply_fixes(&content, &fixes);
                        if count > 0 {
                            std::fs::write(file, &reliary_fix::apply_fixes(&content, &fixes).0).ok();
                            format!("OK: {} replacements\n", count)
                        } else {
                            "ERROR: no match\n".to_string()
                        }
                    }
                    Err(e) => format!("ERROR: {}\n", e),
                }
            }
        }
        "dead" => {
            if parts.len() < 2 {
                "ERROR: usage: dead <path>\n".to_string()
            } else {
                let path = parts[1];
                let config = reliary_dead::DeadConfig::default();
                let mut candidates = Vec::new();
                if let Ok(entries) = std::fs::read_dir(path) {
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
        "index" => {
            if parts.len() < 2 {
                "ERROR: usage: index <path>\n".to_string()
            } else {
                let path = parts[1];
                let db_path = index_db_path(path);
                if let Some(parent) = Path::new(&db_path).parent() {
                    std::fs::create_dir_all(parent).ok();
                }
                std::fs::remove_file(&db_path).ok();
                match rusqlite::Connection::open(&db_path) {
                    Ok(db) => {
                        if reliary_search::schema::create_new_db(&db).is_err() {
                            "ERROR: schema creation failed\n".to_string()
                        } else {
                            match reliary_search::ingest::index_directory(&db, path) {
                                Ok(count) => format!("indexed {} files\n", count),
                                Err(e) => format!("ERROR: {}\n", e),
                            }
                        }
                    }
                    Err(e) => format!("ERROR: {}\n", e),
                }
            }
        }
        _ => format!("ERROR: unknown command '{}'\n", parts[0]),
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
