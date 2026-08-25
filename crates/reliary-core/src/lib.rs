// reliary-core: CLI types, config, session state, output formatting
mod session;
mod state_block;
mod ingest;
mod fs_safe;
mod content_cache;

pub use session::*;
pub use state_block::*;
pub use ingest::*;
pub use fs_safe::*;
pub use content_cache::*;

#[derive(Debug, Clone, Copy, PartialEq, clap::ValueEnum)]
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
