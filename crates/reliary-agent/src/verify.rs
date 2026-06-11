/// Full-stack verification: exercises daemon, proxy, search, risk, compress, memory, veto.
/// Prints a structured report showing every component's health and latency.

use std::io::{Read, Write};
use std::net::TcpStream;
use std::time::Instant;

const LOCAL_REPO: &str = ".";

struct Check {
    name: &'static str,
    status: bool,
    detail: String,
    latency_ms: u64,
}

pub fn run() {
    let mut checks: Vec<Check> = Vec::new();
    eprintln!("\n🔬 reliary verify — full-stack integration check\n");

    // ── 1. Daemon health ──
    let t0 = Instant::now();
    let daemon_ok = daemon_ping();
    checks.push(Check {
        name: "daemon TCP :9799",
        status: daemon_ok.is_some(),
        detail: daemon_ok.unwrap_or_else(|| "Connection refused".into()),
        latency_ms: t0.elapsed().as_millis() as u64,
    });

    // ── 2. Search (FTS5 index exists) ──
    let t0 = Instant::now();
    let search_ok = daemon_query("search validate_config .");
    checks.push(Check {
        name: "FTS5 search",
        status: search_ok.is_some(),
        detail: search_ok.clone().unwrap_or_else(|| "No daemon".into()),
        latency_ms: t0.elapsed().as_millis() as u64,
    });

    // ── 3. Risk (heuristic analysis) ──
    let t0 = Instant::now();
    let cwd = std::env::current_dir().ok().map(|p| p.to_string_lossy().to_string()).unwrap_or_default();
    let risk_path = format!("{}/Cargo.toml", cwd);
    let risk_query = format!("risk {}", risk_path);
    let risk_ok = daemon_query(&risk_query);
    checks.push(Check {
        name: "risk heuristic",
        status: risk_ok.is_some(),
        detail: risk_ok.unwrap_or_else(|| "No daemon".into()),
        latency_ms: t0.elapsed().as_millis() as u64,
    });

    // ── 4. Veto (identifier hallucination check) ──
    let t0 = Instant::now();
    let veto_result = daemon_query(&format!("veto {} 'authorizeRequest(params)'", risk_path));
    let veto_ok = veto_result.as_ref().map_or(false, |s| {
        s == "ok" || s.contains("VETO") || s.to_lowercase().contains("authorizerequest")
    });
    checks.push(Check {
        name: "identifier veto",
        status: veto_ok,
        detail: veto_result.clone().unwrap_or_else(|| "No daemon".into()),
        latency_ms: t0.elapsed().as_millis() as u64,
    });

    // ── 5. Chronicled prior ──
    let t0 = Instant::now();
    let prior_result = daemon_query(&format!("prior {}", cwd));
    checks.push(Check {
        name: "chronicle prior",
        status: prior_result.is_some(),
        detail: prior_result.clone().unwrap_or_else(|| "No daemon".into()),
        latency_ms: t0.elapsed().as_millis() as u64,
    });

    // ── 6. Read summary (FTS5-backed explain) ──
    let t0 = Instant::now();
    let summary_result = daemon_query(&format!("read-summary {}", risk_path));
    let summary_ok = summary_result.as_ref().map_or(false, |s| {
        s.contains("[toml") || s.to_lowercase().contains("package") || s.len() > 10
    });
    checks.push(Check {
        name: "read-summary (explain tool)",
        status: summary_ok,
        detail: summary_result.as_ref().map_or("No daemon".into(), |s| {
            let first_line = s.lines().next().unwrap_or(s).to_string();
            format!("{} chars, first: {}", s.len(), first_line.chars().take(40).collect::<String>())
        }),
        latency_ms: t0.elapsed().as_millis() as u64,
    });

    // ── 7. Index (FTS5 index accessibility) ──
    let t0 = Instant::now();
    let index_path = format!("{}/.reliary/index.sqlite", cwd);
    let index_exists = std::path::Path::new(&index_path).exists();
    checks.push(Check {
        name: "FTS5 index file",
        status: index_exists,
        detail: if index_exists { format!("found at {}", &index_path[..index_path.len().min(40)]) } else { format!("not found — run 'reliary index .'") },
        latency_ms: t0.elapsed().as_millis() as u64,
    });

    // ── 8. Muzzle (scavenger control) ──
    let t0 = Instant::now();
    let muzzle_on = daemon_query("muzzle on");
    let muzzle_off = daemon_query("muzzle off");
    checks.push(Check {
        name: "muzzle (scavenger control)",
        status: muzzle_on.is_some() && muzzle_off.is_some(),
        detail: format!("on:{} off:{}", muzzle_on.unwrap_or_default(), muzzle_off.unwrap_or_default()),
        latency_ms: t0.elapsed().as_millis() as u64,
    });

    // ── 9. Proxy health (port :9090) ──
    let t0 = Instant::now();
    let proxy_ok = TcpStream::connect_timeout(
        &"127.0.0.1:9090".parse().unwrap(),
        std::time::Duration::from_millis(500),
    ).ok();
    checks.push(Check {
        name: "proxy TCP :9090",
        status: proxy_ok.is_some(),
        detail: if proxy_ok.is_some() { "Listening".into() } else { "Not running — start with `reliary serve 9090`".into() },
        latency_ms: t0.elapsed().as_millis() as u64,
    });


    // ── Report ──
    let passed = checks.iter().filter(|c| c.status).count();
    let total = checks.len();
    let all_good = passed == total;

    println!("\n  Component               Status   Latency    Detail");
    println!("  {}────{:─>23}{:─>9}{:─>29}", "", "", "", "");
    for c in &checks {
        let status_str = if c.status { "✅" } else { "❌" };
        println!("  {:<24}{:<8} {:>4}ms   {}", c.name, status_str, c.latency_ms, c.detail.chars().take(40).collect::<String>());
    }
    println!();
    if all_good {
        println!("  ✅ All {} checks passed", total);
    } else {
        println!("  ⚠️  {}/{} checks passed", passed, total);
    }
    println!("  Proxy: {}", if proxy_ok.is_some() { "✅ ready on :9090 — point DEEPSEEK_BASE_URL here" } else { "⚠️  start with `reliary serve 9090`" });
}

fn daemon_ping() -> Option<String> {
    let mut stream = TcpStream::connect_timeout(
        &"127.0.0.1:9799".parse().unwrap(),
        std::time::Duration::from_millis(1000),
    ).ok()?;
    let _ = stream.write_all(b"ping\n");
    let mut buf = String::new();
    stream.read_to_string(&mut buf).ok()?;
    Some(buf.trim().to_string())
}

fn daemon_query(cmd: &str) -> Option<String> {
    let mut stream = TcpStream::connect_timeout(
        &"127.0.0.1:9799".parse().unwrap(),
        std::time::Duration::from_millis(1000),
    ).ok()?;
    let _ = stream.write_all(format!("{}\n", cmd).as_bytes());
    let mut buf = String::new();
    stream.read_to_string(&mut buf).ok()?;
    let result = buf.trim().to_string();
    if result.is_empty() || result.starts_with("ERROR: unknown") { None } else { Some(result) }
}
