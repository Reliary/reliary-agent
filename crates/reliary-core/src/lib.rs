// reliary-core: CLI types, config, session state, output formatting
use std::collections::HashMap;

#[derive(Debug, Clone)]
pub struct Session {
    pub turn_count: usize,
    pub file_hashes: HashMap<String, u64>,
}

impl Session {
    pub fn new() -> Self {
        Self { turn_count: 0, file_hashes: HashMap::new() }
    }
}

#[derive(Debug, Clone, Copy, clap::ValueEnum)]
pub enum OutputFormat {
    Default, // human-readable tables
    Compact, // agent-optimized (~50t per result)
    Json,    // machine-parseable
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
                let map: HashMap<&str, &[String]> = HashMap::from([(label, lines)]);
                serde_json::to_string(&map).unwrap_or_default()
            }
            OutputFormat::Compact => lines.join(" | "),
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
