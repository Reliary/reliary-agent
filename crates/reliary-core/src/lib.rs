// reliary-core: CLI types, config, session state, output formatting
mod session;
mod state_block;
mod ingest;
use std::collections::HashMap;

pub use session::*;
pub use state_block::*;
pub use ingest::*;

#[derive(Debug, Clone)]
pub struct Session {
    pub turn_count: usize,
    pub file_hashes: HashMap<String, u64>,
}

impl Default for Session {
    fn default() -> Self {
        Self::new()
    }
}

impl Session {
    pub fn new() -> Self {
        Self { turn_count: 0, file_hashes: HashMap::new() }
    }
}

#[derive(Debug, Clone, Copy, clap::ValueEnum)]
pub enum OutputFormat {
    Default,
    Compact,
    Json,
}

#[derive(Debug, Clone)]
pub struct FormatConfig {
    pub format: OutputFormat,
    pub color: bool,
}

impl FormatConfig {
    pub fn new(format: OutputFormat) -> Self {
        Self { format, color: matches!(format, OutputFormat::Default) }
    }

    pub fn format_output(&self, label: &str, lines: &[String]) -> String {
        match self.format {
            OutputFormat::Json => {
                serde_json::to_string(&lines).unwrap_or_default()
            }
            OutputFormat::Compact => lines.join("\n"),
            OutputFormat::Default => {
                if lines.is_empty() {
                    format!("{}: (none)", label)
                } else {
                    format!("{}:\n  {}", label, lines.join("\n  "))
                }
            }
        }
    }
}
