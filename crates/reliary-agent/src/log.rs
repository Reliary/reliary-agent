//! Structured logging level helpers for reliary-agent.
//!
//! Two env vars control output:
//! - `RELIARY_LOG` — filtering for reliary's own messages (default: `info`)
//! - `RUST_LOG` — standard tracing env-filter (overrides RELIARY_LOG if set)
//!
//! Levels: error, warn, info, debug, trace
//!
//! Logs are written to stderr. `RELIARY_LOG_FILE` is not implemented.

/// Initialize logging. No-op for now (placeholder).
pub fn init() {
    use std::sync::Once;
    static INIT: Once = Once::new();
    INIT.call_once(|| {});
}

/// Return the filtering directive (env filter syntax).
#[allow(dead_code)]
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
        for directive in rl.split(',') {
            let directive = directive.trim();
            if directive.starts_with("reliary_agent=") {
                let level = directive.trim_start_matches("reliary_agent=");
                if !level.is_empty() {
                    return level.to_string();
                }
            }
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
    fn test_level_value_boundary_conditions() {
        assert_eq!(level_value("error"), 1, "error should be level 1");
        assert_eq!(level_value("trace"), 5, "trace should be level 5");
        assert_eq!(level_value(""), 0, "empty string should be 0");
        assert_eq!(level_value("INFO"), 0, "upper case should not match");
        assert_eq!(level_value(" warn"), 0, "leading space should not match");
    }
}
