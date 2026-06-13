/// Configuration cascade: env var > project config > global config > default
use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;

#[derive(Debug, Clone, PartialEq)]
pub enum GateMode {
    Fast,
    Reactive,
    Strict,
}

impl GateMode {
    pub fn as_str(&self) -> &'static str {
        match self {
            GateMode::Fast => "fast",
            GateMode::Reactive => "reactive",
            GateMode::Strict => "strict",
        }
    }

    pub fn from_str(s: &str) -> Self {
        match s.trim().to_lowercase().as_str() {
            "fast" => GateMode::Fast,
            "strict" => GateMode::Strict,
            _ => GateMode::Reactive, // default
        }
    }
}

const CONFIG_FILENAME: &str = ".reliary/config.json";

fn global_config_path() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
    PathBuf::from(home).join(CONFIG_FILENAME)
}

fn project_config_path(workdir: &str) -> PathBuf {
    PathBuf::from(workdir).join(CONFIG_FILENAME)
}

fn read_config_file(path: &PathBuf) -> HashMap<String, String> {
    match fs::read_to_string(path) {
        Ok(content) => serde_json::from_str(&content).unwrap_or_default(),
        Err(_) => HashMap::new(),
    }
}

fn write_config_file(path: &PathBuf, config: &HashMap<String, String>) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| format!("Cannot create config dir: {}", e))?;
    }
    let content = serde_json::to_string_pretty(config).map_err(|e| format!("Cannot serialize config: {}", e))?;
    fs::write(path, content).map_err(|e| format!("Cannot write config: {}", e))?;
    Ok(())
}

pub fn resolve_mode(workdir: Option<&str>) -> GateMode {
    // 1. Environment variable (highest priority)
    if let Ok(env_mode) = std::env::var("RELIARY_MODE") {
        if !env_mode.is_empty() {
            return GateMode::from_str(&env_mode);
        }
    }

    // 2. Project-local config
    if let Some(wd) = workdir {
        let project_cfg = read_config_file(&project_config_path(wd));
        if let Some(mode) = project_cfg.get("mode") {
            return GateMode::from_str(mode);
        }
    }

    // 3. Global config
    let global_cfg = read_config_file(&global_config_path());
    if let Some(mode) = global_cfg.get("mode") {
        return GateMode::from_str(mode);
    }

    // 4. Default
    GateMode::Reactive
}

/// Get all config (global or project-local)
/// Set a config key. Returns human-readable confirmation.
pub fn set_config(key: &str, value: &str, project: bool, root: Option<&str>) -> String {
    let path = if project {
        let base = root.unwrap_or(".");
        project_config_path(base)
    } else {
        global_config_path()
    };
    let mut cfg = read_config_file(&path);
    cfg.insert(key.to_string(), value.to_string());
    match write_config_file(&path, &cfg) {
        Ok(()) => {
            let location = if project { "project" } else { "global" };
            let root_msg = if project && root.is_some() {
                format!(" for {}", root.unwrap())
            } else {
                String::new()
            };
            format!("Set {} = {} in {}{}", key, value, location, root_msg)
        }
        Err(e) => format!("Error: {}", e),
    }
}
