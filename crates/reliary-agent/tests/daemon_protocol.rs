use std::io::{BufRead, Read, Write};
use std::net::TcpStream;
use std::path::Path;
use std::process::{Command, Child};
use std::time::Duration;

const PORT: u16 = 19854;

fn bin() -> String {
    let ws = Path::new(env!("CARGO_MANIFEST_DIR"));
    let p = format!("{}/target/release/reliary-agent", ws.parent().unwrap().parent().unwrap().display());
    if Path::new(&p).exists() { return p; }
    format!("{}/target/debug/reliary-agent", ws.parent().unwrap().parent().unwrap().display())
}

fn start_server() -> Child {
    let b = bin();
    let child = Command::new(&b)
        .args(["serve", &PORT.to_string()])
        .env("RELIARY_UPSTREAM_URL", "http://localhost:19855")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("Failed to start server");
    std::thread::sleep(Duration::from_millis(1000));
    child
}

fn http_get(path: &str) -> String {
    let mut stream = TcpStream::connect(("127.0.0.1", PORT))
        .expect("Connect to server");
    stream.set_read_timeout(Some(Duration::from_secs(5))).ok();
    write!(stream, "GET {} HTTP/1.0\r\nHost: localhost\r\n\r\n", path).ok();
    let mut reader = std::io::BufReader::new(&stream);
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line).ok() == Some(0) || line.trim().is_empty() { break; }
    }
    let mut resp = String::new();
    reader.read_to_string(&mut resp).ok();
    resp.trim().to_string()
}

fn http_post(path: &str, body: &str, auth: &str) -> (u16, String) {
    let mut stream = TcpStream::connect(("127.0.0.1", PORT)).expect("Connect");
    stream.set_read_timeout(Some(Duration::from_secs(5))).ok();
    let req = format!(
        "POST {} HTTP/1.0\r\nHost: localhost\r\nContent-Type: application/json\r\nAuthorization: Bearer {}\r\nContent-Length: {}\r\n\r\n{}",
        path, auth, body.len(), body
    );
    write!(stream, "{}", req).ok();
    let mut reader = std::io::BufReader::new(&stream);
    let mut status = 0u16;
    let mut first = true;
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line).ok() == Some(0) { break; }
        if first {
            status = line.split_whitespace().nth(1).and_then(|s| s.parse().ok()).unwrap_or(0);
            first = false;
        }
        if line.trim().is_empty() { break; }
    }
    let mut resp = String::new();
    reader.read_to_string(&mut resp).ok();
    (status, resp.trim().to_string())
}

// Single test for all endpoints (avoids process conflicts)
#[test]
fn test_all_endpoints() {
    let mut server = start_server();
    let mut all_pass = true;

    let ping = http_get("/ping");
    if ping != "pong" { eprintln!("FAIL: ping got '{}'", ping); all_pass = false; }

    let health = http_get("/health");
    if !health.contains("ok") { eprintln!("FAIL: health got '{}'", health); all_pass = false; }

    let m_on = http_get("/muzzle?state=on");
    if m_on != "muzzled" { eprintln!("FAIL: muzzle on got '{}'", m_on); all_pass = false; }

    let m_off = http_get("/muzzle?state=off");
    if m_off != "unmuzzled" { eprintln!("FAIL: muzzle off got '{}'", m_off); all_pass = false; }

    let search = http_get("/search?q=test&path=/nonexistent");
    if !search.contains("ERROR") && !search.contains("no results") { eprintln!("FAIL: search got '{}'", search); all_pass = false; }

    let risk = http_get("/risk?file=/nonexistent/file.rs");
    if !risk.contains("ERROR") { eprintln!("FAIL: risk got '{}'", risk); all_pass = false; }

    let veto = http_get("/veto?file=/nonexistent&text=test");
    if !veto.contains("ERROR") { eprintln!("FAIL: veto got '{}'", veto); all_pass = false; }

    let compress = http_get("/compress?text=short");
    if compress != "no compression" { eprintln!("FAIL: compress got '{}'", compress); all_pass = false; }

    let unknown = http_get("/nonexistent");
    if !unknown.contains("ERROR") { eprintln!("FAIL: unknown endpoint got '{}'", unknown); all_pass = false; }

    let (status, body) = http_post("/v1/chat/completions", "{\"model\":\"test\",\"messages\":[]}", "");
    if status != 403 { eprintln!("FAIL: proxy status {}", status); all_pass = false; }
    if !body.contains("unknown api key") { eprintln!("FAIL: proxy body '{}'", body); all_pass = false; }
    if all_pass { eprintln!("All 11 endpoints passed"); }
    server.kill().ok();
}
