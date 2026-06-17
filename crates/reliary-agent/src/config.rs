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
            _ => GateMode::Reactive,
        }
    }
}

const CONFIG_FILENAME: &str = ".reliary/config.json";

pub fn global_config_path() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
    PathBuf::from(home).join(CONFIG_FILENAME)
}

pub fn project_config_path(workdir: &str) -> PathBuf {
    PathBuf::from(workdir).join(CONFIG_FILENAME)
}

fn read_config_file(path: &PathBuf) -> HashMap<String, String> {
    match fs::read_to_string(path) {
        Ok(content) => serde_json::from_str(&content).unwrap_or_default(),
        Err(_) => HashMap::new(),
    }
}

pub fn write_config_file(path: &PathBuf, config: &HashMap<String, String>) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| format!("Cannot create config dir: {}", e))?;
    }
    let content = serde_json::to_string_pretty(config).map_err(|e| format!("Cannot serialize config: {}", e))?;
    crate::heal::atomic_write(path.to_str().unwrap_or("config.json"), &content)
}

pub fn resolve_mode(workdir: Option<&str>) -> GateMode {
    if let Ok(env_mode) = std::env::var("RELIARY_MODE") {
        if !env_mode.is_empty() {
            return GateMode::from_str(&env_mode);
        }
    }

    if let Some(wd) = workdir {
        let project_cfg = read_config_file(&project_config_path(wd));
        if let Some(mode) = project_cfg.get("mode") {
            return GateMode::from_str(mode);
        }
    }

    let global_cfg = read_config_file(&global_config_path());
    if let Some(mode) = global_cfg.get("mode") {
        return GateMode::from_str(mode);
    }

    GateMode::Reactive
}

/// Resolve feature flags with the same cascade as mode.
/// Returns Vec of (feature_name, enabled).
pub fn resolve_features(workdir: Option<&str>) -> Vec<(String, bool)> {
    let defaults: Vec<(String, bool)> = vec![
        ("compress".into(), true),
        ("convWindow".into(), true),
        ("readEnrichment".into(), true),
        ("editMerge".into(), false),
        ("healEdit".into(), true),
        ("priorInjection".into(), false),
    ];

    // Parse env var: RELIARY_FEATURES=+compress,-convWindow format
    let mut overrides: HashMap<String, bool> = HashMap::new();
    if let Ok(env_features) = std::env::var("RELIARY_FEATURES") {
        for part in env_features.split(',') {
            let part = part.trim();
            if let Some(feat) = part.strip_prefix('+') {
                overrides.insert(feat.to_string(), true);
            } else if let Some(feat) = part.strip_prefix('-') {
                overrides.insert(feat.to_string(), false);
            }
        }
    }

    // Read config files
    let mut config_map: HashMap<String, bool> = HashMap::new();
    if let Some(wd) = workdir {
        let project_cfg = read_config_file(&project_config_path(wd));
        if let Some(features_val) = project_cfg.get("features") {
            if let Ok(features_obj) = serde_json::from_str::<HashMap<String, bool>>(features_val) {
                for (k, v) in features_obj {
                    config_map.insert(k, v);
                }
            }
        }
    }
    let global_cfg = read_config_file(&global_config_path());
    if let Some(features_val) = global_cfg.get("features") {
        if let Ok(features_obj) = serde_json::from_str::<HashMap<String, bool>>(features_val) {
            for (k, v) in features_obj {
                config_map.entry(k).or_insert(v);
            }
        }
    }

    // Layer: defaults <- global config <- project config <- env overrides
    let mut result = defaults;
    for (k, v) in &mut result {
        if let Some(gv) = config_map.get(k) {
            *v = *gv;
        }
        if let Some(ov) = overrides.get(k) {
            *v = *ov;
        }
    }

    result
}

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
                format!(" for {}", root.unwrap_or_default())
            } else {
                String::new()
            };
            format!("Set {} = {} in {}{}", key, value, location, root_msg)
        }
        Err(e) => format!("Error: {}", e),
    }
}
