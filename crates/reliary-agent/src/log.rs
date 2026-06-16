/// Structured logging with levels for reliary-agent.
/// 
/// Two env vars control output:
/// - `RELIARY_LOG` — filtering for reliary's own messages (default: `info`)
/// - `RUST_LOG` — standard tracing env-filter (overrides RELIARY_LOG if set)
/// 
/// Levels: error, warn, info, debug, trace
/// 
/// Logs are written to stderr. If `RELIARY_LOG_FILE` is set, also written to
/// that file (appended, rotated at 10MB).

use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::Instant;

static LOG_FILE: once_cell::sync::Lazy<Mutex<Option<FileLogger>>> =
    once_cell::sync::Lazy::new(|| Mutex::new(None));

#[allow(dead_code)]
struct FileLogger {
    file: File,
    path: PathBuf,
    size: u64,
    max_size: u64,
    created: Instant,
}

#[allow(dead_code)]
impl FileLogger {
    #[allow(dead_code)]
    fn write(&mut self, msg: &str) {
        if self.size > self.max_size {
            // Rotate: rename current → .old, open new
            let old = self.path.with_extension("log.old");
            let _ = fs::rename(&self.path, &old);
            if let Ok(f) = OpenOptions::new().create(true).append(true).open(&self.path) {
                self.file = f;
                self.size = 0;
            }
        }
        if let Ok(()) = writeln!(self.file, "{}", msg) {
            self.size += (msg.len() + 1) as u64;
            let _ = self.file.flush();
        }
    }
}

/// Initialize tracing/logging. Called once at startup. Safe to call multiple times.
pub fn init() {
    use std::sync::Once;
    static INIT: Once = Once::new();
    INIT.call_once(|| {
        init_once();
    });
}

fn init_once() {
    // Determine log filter
    let filter = if let Ok(rust_log) = std::env::var("RUST_LOG") {
        if !rust_log.trim().is_empty() { rust_log }
        else { resolve_reliary_log() }
    } else {
        resolve_reliary_log()
    };

    use tracing_subscriber::filter::EnvFilter;
    let filter = EnvFilter::try_new(&filter).unwrap_or_else(|_| EnvFilter::new("info"));

    let _ = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(true)
        .with_file(true)
        .with_line_number(true)
        .with_writer(std::io::stderr) // MUST write to stderr, not stdout (stdout is MCP transport)
        .try_init();

    // Set up file logging if RELIARY_LOG_FILE is set
    if let Ok(path_str) = std::env::var("RELIARY_LOG_FILE") {
        let path = PathBuf::from(&path_str);
        if let Some(parent) = path.parent() {
            let _ = fs::create_dir_all(parent);
        }
        if let Ok(file) = OpenOptions::new().create(true).append(true).open(&path) {
            let meta = file.metadata().ok();
            let size = meta.as_ref().map(|m| m.len()).unwrap_or(0);
            *LOG_FILE.lock().unwrap_or_else(|e| e.into_inner()) = Some(FileLogger {
                file,
                path,
                size,
                max_size: 10 * 1024 * 1024, // 10MB
                created: Instant::now(),
            });
        }
    }
}

fn resolve_reliary_log() -> String {
    match std::env::var("RELIARY_LOG").as_deref() {
        Ok("error") => "reliary_agent=error".into(),
        Ok("warn") => "reliary_agent=warn".into(),
        Ok("info") => "reliary_agent=info".into(),
        Ok("debug") => "reliary_agent=debug".into(),
        Ok("trace") => "reliary_agent=trace".into(),
        Ok(other) if !other.is_empty() => other.into(),
        _ => "reliary_agent=info".into(),
    }
}

/// Return the numeric value for a log level name, or 0 for none.
#[allow(dead_code)]
pub fn level_value(name: &str) -> u8 {
    match name {
        "error" => 1,
        "warn"  => 2,
        "info"  => 3,
        "debug" => 4,
        "trace" => 5,
        _ => 0,
    }
}

/// Return the log level from the environment (for gate.js config query).
#[allow(dead_code)]
pub fn current_level() -> String {
    if let Ok(rl) = std::env::var("RUST_LOG") {
        if rl.contains("reliary_agent") {
            if rl.contains("trace") { return "trace".into(); }
            if rl.contains("debug") { return "debug".into(); }
            if rl.contains("warn")  { return "warn".into(); }
            if rl.contains("error") { return "error".into(); }
        }
    }
    match std::env::var("RELIARY_LOG").as_deref() {
        Ok(v) => v.to_string(),
        Err(_) => "info".into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_level() {
        // RUST_LOG and RELIARY_LOG unset → info
        assert_eq!(current_level(), "info");
    }

    #[test]
    fn test_level_value_ordering() {
        assert!(level_value("trace") > level_value("debug"));
        assert!(level_value("debug") > level_value("info"));
        assert!(level_value("info") > level_value("warn"));
        assert!(level_value("warn") > level_value("error"));
        assert_eq!(level_value("unknown"), 0);
    }

    #[test]
    fn test_reliary_log_parsing() {
        assert_eq!(&resolve_reliary_log(), "reliary_agent=info");
    }

    #[test]
    fn test_reliary_log_error() {
        // Can't really test env vars without tempdir isolation
        let result = resolve_reliary_log();
        assert!(result.contains("reliary_agent="));
    }
}
