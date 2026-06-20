// NO_COLOR / TERM=dumb aware ANSI helpers. Used by ux.rs and main.rs for all
// user-facing terminal output. See https://no-color.org/.

fn no_color() -> bool {
    std::env::var("NO_COLOR").is_ok() || std::env::var("TERM").map(|t| t == "dumb").unwrap_or(false)
}

pub fn green(s: &str) -> String {
    if no_color() { s.to_string() } else { format!("\x1b[32m{}\x1b[0m", s) }
}

pub fn red(s: &str) -> String {
    if no_color() { s.to_string() } else { format!("\x1b[31m{}\x1b[0m", s) }
}

pub fn yellow(s: &str) -> String {
    if no_color() { s.to_string() } else { format!("\x1b[33m{}\x1b[0m", s) }
}

#[allow(dead_code)]
pub fn blue(s: &str) -> String {
    if no_color() { s.to_string() } else { format!("\x1b[34m{}\x1b[0m", s) }
}

pub fn bold(s: &str) -> String {
    if no_color() { s.to_string() } else { format!("\x1b[1m{}\x1b[0m", s) }
}

pub fn dim(s: &str) -> String {
    if no_color() { s.to_string() } else { format!("\x1b[2m{}\x1b[0m", s) }
}

#[allow(dead_code)]
pub fn reset(_s: &str) -> String {
    if no_color() { String::new() } else { "\x1b[0m".to_string() }
}

#[allow(dead_code)]
pub fn is_enabled() -> bool { !no_color() }

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_color_env_disables_green() {
        std::env::set_var("NO_COLOR", "1");
        assert_eq!(green("test"), "test");
        std::env::remove_var("NO_COLOR");
    }

    #[test]
    fn term_dumb_disables_red() {
        std::env::set_var("TERM", "dumb");
        assert_eq!(red("test"), "test");
        std::env::remove_var("TERM");
    }

    #[test]
    fn default_emits_ansi_when_no_no_color() {
        // If NO_COLOR is set by another test, clear it for this one
        std::env::remove_var("NO_COLOR");
        std::env::remove_var("TERM");
        let g = green("test");
        assert!(g.contains("\x1b["), "expected ANSI codes, got {:?}", g);
    }
}
