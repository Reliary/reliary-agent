/// Configuration cascade: env var > project config > global config > default
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

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

/// The layer from which a config value was resolved.
#[derive(Debug, Clone, PartialEq)]
pub enum ConfigSource {
    Env,
    Project,
    Global,
    Default,
}

impl ConfigSource {
    pub fn as_str(&self) -> &'static str {
        match self {
            ConfigSource::Env => "env",
            ConfigSource::Project => "project .reliary/config.json",
            ConfigSource::Global => "global ~/.reliary/config.json",
            ConfigSource::Default => "default",
        }
    }
}

/// A resolved config value with its source.
#[derive(Debug, Clone)]
pub struct ResolvedMode {
    pub value: GateMode,
    pub source: ConfigSource,
}

/// A resolved feature with its source.
#[derive(Debug, Clone)]
pub struct ResolvedFeature {
    pub name: String,
    pub enabled: bool,
    pub source: ConfigSource,
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

pub fn write_config_file(path: &Path, config: &HashMap<String, String>) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| format!("Cannot create config dir: {}", e))?;
    }
    let content = serde_json::to_string_pretty(config).map_err(|e| format!("Cannot serialize config: {}", e))?;
    crate::heal::atomic_write(path.to_str().unwrap_or("config.json"), &content)
}

/// Resolve mode with source tracking.
pub fn resolve_mode_with_source(workdir: Option<&str>) -> ResolvedMode {
    if let Ok(env_mode) = std::env::var("RELIARY_MODE") {
        if !env_mode.is_empty() {
            return ResolvedMode {
                value: GateMode::from_str(&env_mode),
                source: ConfigSource::Env,
            };
        }
    }

    if let Some(wd) = workdir {
        let project_cfg = read_config_file(&project_config_path(wd));
        if let Some(mode) = project_cfg.get("mode") {
            return ResolvedMode {
                value: GateMode::from_str(mode),
                source: ConfigSource::Project,
            };
        }
    }

    let global_cfg = read_config_file(&global_config_path());
    if let Some(mode) = global_cfg.get("mode") {
        return ResolvedMode {
            value: GateMode::from_str(mode),
            source: ConfigSource::Global,
        };
    }

    ResolvedMode {
        value: GateMode::Strict,
        source: ConfigSource::Default,
    }
}

/// Resolve mode (simple API, no source tracking).
pub fn resolve_mode(workdir: Option<&str>) -> GateMode {
    resolve_mode_with_source(workdir).value
}

/// Resolve feature flags with source tracking.
/// Single source of truth for feature names and their default state.
/// Bug 38/44: previously duplicated in main.rs valid_keys and gate.js.
pub const FEATURE_DEFAULTS: &[(&str, bool)] = &[
    ("compress", true),
    ("convWindow", true),
    ("readEnrichment", true),
    ("editMerge", false),
    ("healEdit", true),
    ("priorInjection", false),
];

/// Public list of valid config keys (used by CLI validation)
pub const VALID_CONFIG_KEYS: &[&str] = &[
    "mode",
    "features.compress",
    "features.convWindow",
    "features.readEnrichment",
    "features.editMerge",
    "features.healEdit",
    "features.priorInjection",
    "apiMode",
    "privacyMode",
    "apiBaseUrl",
    "serverUrl",
];

pub fn resolve_features_with_source(workdir: Option<&str>) -> Vec<ResolvedFeature> {
    let defaults: Vec<(&str, bool)> = FEATURE_DEFAULTS.to_vec();

    // Parse env var
    let mut env_overrides: HashMap<String, bool> = HashMap::new();
    if let Ok(env_features) = std::env::var("RELIARY_FEATURES") {
        for part in env_features.split(',') {
            let part = part.trim();
            if let Some(feat) = part.strip_prefix('+') {
                env_overrides.insert(feat.to_string(), true);
            } else if let Some(feat) = part.strip_prefix('-') {
                env_overrides.insert(feat.to_string(), false);
            }
        }
    }

    // Read config files
    let mut project_features: HashMap<String, bool> = HashMap::new();
    let mut global_features: HashMap<String, bool> = HashMap::new();

    if let Some(wd) = workdir {
        let project_cfg = read_config_file(&project_config_path(wd));
        if let Some(features_val) = project_cfg.get("features") {
            if let Ok(obj) = serde_json::from_str::<HashMap<String, bool>>(features_val) {
                project_features = obj;
            }
        }
    }
    let global_cfg = read_config_file(&global_config_path());
    if let Some(features_val) = global_cfg.get("features") {
        if let Ok(obj) = serde_json::from_str::<HashMap<String, bool>>(features_val) {
            global_features = obj;
        }
    }

    // Resolve each feature with source priority
    defaults.into_iter().map(|(name, default_val)| {
        if let Some(val) = env_overrides.get(name) {
            ResolvedFeature { name: name.to_string(), enabled: *val, source: ConfigSource::Env }
        } else if let Some(val) = project_features.get(name) {
            ResolvedFeature { name: name.to_string(), enabled: *val, source: ConfigSource::Project }
        } else if let Some(val) = global_features.get(name) {
            ResolvedFeature { name: name.to_string(), enabled: *val, source: ConfigSource::Global }
        } else {
            ResolvedFeature { name: name.to_string(), enabled: default_val, source: ConfigSource::Default }
        }
    }).collect()
}

/// Resolve features (simple API, no source tracking).
#[allow(dead_code)]
pub fn resolve_features(workdir: Option<&str>) -> Vec<(String, bool)> {
    resolve_features_with_source(workdir)
        .into_iter()
        .map(|f| (f.name, f.enabled))
        .collect()
}

pub fn set_config(key: &str, value: &str, project: bool, root: Option<&str>) -> String {
    // Bug 45: validate values for known keys before storing
    if key.starts_with("features.") && !matches!(value, "true" | "false") {
        return format!("Error: feature value must be 'true' or 'false', got '{}'", value);
    }
    if key == "mode" && !matches!(value, "fast" | "reactive" | "strict") {
        return format!("Error: mode must be 'fast', 'reactive', or 'strict', got '{}'", value);
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Mutex, LazyLock};

    /// Serializes config tests because they mutate process-global env vars.
    static CONFIG_TEST_MUTEX: LazyLock<Mutex<()>> = LazyLock::new(|| Mutex::new(()));

    fn isolated_test<F>(f: F)
    where
        F: FnOnce(),
    {
        use std::sync::atomic::{AtomicU64, Ordering};
        static TEST_CTR: AtomicU64 = AtomicU64::new(0);
        let ctr = TEST_CTR.fetch_add(1, Ordering::Relaxed);
        let _guard = CONFIG_TEST_MUTEX.lock().unwrap(); // GUARDED: intentional - test mutex
        std::env::remove_var("RELIARY_MODE");
        std::env::remove_var("RELIARY_FEATURES");
        let tmp = std::env::temp_dir().join(format!("reliary_config_test_{}_{}", std::process::id(), ctr));
        let _ = std::fs::create_dir_all(&tmp);
        let old_home = std::env::var("HOME").ok();  // GUARDED: intentional — Option::ok() in test helper
        std::env::set_var("HOME", tmp.to_str().unwrap());
        f();
        if let Some(h) = old_home { std::env::set_var("HOME", h); } else { std::env::remove_var("HOME"); }
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn test_resolve_mode_default() {
        isolated_test(|| {
            let result = resolve_mode_with_source(Some("/tmp/nonexistent_test_dir_12345"));
            assert_ne!(result.source, ConfigSource::Env);
            // v0.6.8: default is now strict (was reactive)
            assert_eq!(result.value, GateMode::Strict);
        });
    }

    #[test]
    fn test_resolve_mode_env() {
        isolated_test(|| {
            std::env::set_var("RELIARY_MODE", "fast");
            let result = resolve_mode_with_source(None);
            assert_eq!(result.value, GateMode::Fast);
            assert_eq!(result.source, ConfigSource::Env);
        });
    }

    #[test]
    fn test_resolve_features_default() {
        isolated_test(|| {
            let features = resolve_features_with_source(Some("/nonexistent"));
            let compress = features.iter().find(|f| f.name == "compress").unwrap();
            assert!(compress.enabled);
            assert_eq!(compress.source, ConfigSource::Default);

            let edit_merge = features.iter().find(|f| f.name == "editMerge").unwrap();
            assert!(!edit_merge.enabled);
            assert_eq!(edit_merge.source, ConfigSource::Default);
        });
    }

    #[test]
    fn test_resolve_features_env_override() {
        isolated_test(|| {
            std::env::set_var("RELIARY_FEATURES", "-compress,+editMerge");
            let features = resolve_features_with_source(Some("/nonexistent"));
            let compress = features.iter().find(|f| f.name == "compress").unwrap();
            assert!(!compress.enabled);
            assert_eq!(compress.source, ConfigSource::Env);

            let edit_merge = features.iter().find(|f| f.name == "editMerge").unwrap();
            assert!(edit_merge.enabled);
            assert_eq!(edit_merge.source, ConfigSource::Env);
        });
    }
}
