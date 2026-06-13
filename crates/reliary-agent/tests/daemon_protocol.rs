use std::io::{BufRead, Write};
use std::net::TcpStream;
use std::path::Path;
use std::process::Command;
use std::time::Duration;

const PORT: u16 = 9799;

fn bin() -> String {
    let ws = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent().and_then(|p| p.parent())
        .expect("workspace root");
    for p in &["debug", "release"] {
        let c = ws.join("target").join(p).join("reliary-agent");
        if c.exists() { return c.to_string_lossy().to_string(); }
    }
    "reliary-agent".to_string()
}

fn send(cmd: &str) -> String {
    let mut s = TcpStream::connect(("127.0.0.1", PORT)).unwrap();
    s.set_read_timeout(Some(Duration::from_secs(3))).ok();
    writeln!(s, "{}", cmd).ok();
    let mut r = String::new();
    std::io::BufReader::new(&s).read_line(&mut r).ok();
    r.trim().to_string()
}

#[test]
fn daemon_protocol() {
    let _ = Command::new("sh")
        .arg("-c")
        .arg(format!("lsof -ti:{} 2>/dev/null | xargs kill -9 2>/dev/null", PORT))
        .status();
    std::thread::sleep(Duration::from_millis(500));

    let mut d = Command::new(bin()).arg("daemon").spawn().expect("start daemon");
    std::thread::sleep(Duration::from_millis(1000));

    assert_eq!(send("ping"), "pong");
    assert_eq!(send("ping"), "pong");
    assert!(send("status").contains("reliary-agent"));
    let r = send("search test .");
    assert!(r.contains("ERROR") || r == "no results");
    assert!(send("risk /nonexistent").contains("ERROR"));
    assert_eq!(send("check-read /tmp/nonexistent abc123"), "stale");
    assert_eq!(send("muzzle on"), "muzzled");
    assert_eq!(send("muzzle off"), "unmuzzled");
    let r = send("veto /nonexistent/file.rs someText");
    assert!(r.starts_with("ERROR") || r == "ok");

    let _ = d.kill();
    let _ = d.wait();
}
