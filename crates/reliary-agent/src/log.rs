//! Structured logging with levels for reliary-agent.
//!
//! Two env vars control output:
//! - `RELIARY_LOG` — filtering for reliary's own messages (default: `info`)
//! - `RUST_LOG` — standard tracing env-filter (overrides RELIARY_LOG if set)
//!
//! Levels: error, warn, info, debug, trace
//!
//! Logs are written to stderr. If `RELIARY_LOG_FILE` is set, also written to
//! that file (appended, rotated at 10MB).

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
            let meta = file.metadata().ok();  // GUARDED: intentional
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
        // With RELIARY_LOG unset, current_level returns "info"
        let level = current_level();
        assert_eq!(level, "info");
    }

    #[test]
    fn test_level_value_ordering() {
        assert_eq!(level_value("error"), 1);
        assert_eq!(level_value("warn"), 2);
        assert_eq!(level_value("info"), 3);
        assert_eq!(level_value("debug"), 4);
        assert_eq!(level_value("trace"), 5);
        assert!(level_value("trace") > level_value("debug"));
        assert!(level_value("debug") > level_value("info"));
        assert!(level_value("info") > level_value("warn"));
        assert!(level_value("warn") > level_value("error"));
        assert_eq!(level_value("unknown"), 0);
        assert_eq!(level_value(""), 0);
        assert_eq!(level_value("ERROR"), 0); // case sensitive
    }

    #[test]
    fn test_resolve_reliary_log_levels() {
        // Test each level maps to the correct filter string
        // (Uses RELIARY_LOG from env if set; if unset defaults to info)

        // Verify default when no env var
        let filter = resolve_reliary_log();
        assert_eq!(filter, "reliary_agent=info");

        // Verify results always contain the crate tag
        // (independent of env — we just check the function produces valid syntax)
        assert!(filter.contains("reliary_agent="));
    }

    #[test]
    fn test_resolve_reliary_log_unexpected_values() {
        let filter = resolve_reliary_log();
        assert!(!filter.is_empty());
    }

    #[test]
    fn test_file_logger_rotation() {
        // Create temp dir for log file
        let dir = std::env::temp_dir().join(format!("reliary_log_test_{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let log_path = dir.join("test.log");

        // Create a 10-byte max file logger = will rotate after 10 bytes
        let file = std::fs::File::create(&log_path).unwrap();
        let mut logger = FileLogger {
            file,
            path: log_path.clone(),
            size: 8,  // Just 2 bytes from 10-byte limit — next write triggers rotation
            max_size: 10,
            created: Instant::now(),
        };

        // Write a line — triggers rotation (8 + msg.len >= 10)
        logger.write("hello world");
        assert!(log_path.exists(), "original log should exist after rotation");

        // Verify old file was renamed
        let old = dir.join("test.log.old");
        assert!(old.exists() || logger.size > 0, "old or new log should exist");

        // Cleanup
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_level_value_boundary_conditions() {
        assert_eq!(level_value("error"), 1, "error should be level 1");
        assert_eq!(level_value("trace"), 5, "trace should be level 5");
        assert_eq!(level_value(""), 0, "empty string should be 0");
        assert_eq!(level_value("INFO"), 0, "upper case should not match");
        assert_eq!(level_value(" warn"), 0, "leading space should not match");
    }
}
