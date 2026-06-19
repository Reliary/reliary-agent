use std::path::PathBuf;

/// Discover upstream URL for an auth key by scanning all known agent configs.
pub fn discover_upstream(auth_key: &str) -> Option<String> {
    // 1. Local proxy-routes.json (explicit user override, highest priority)
    if let Some(url) = scan_proxy_routes(auth_key) {
        return Some(url);
    }
    // 2. OpenCode provider configs
    if let Some(url) = scan_opencode_configs(auth_key) {
        return Some(url);
    }
    // 3. Claude Code config
    if let Some(url) = scan_claude_config(auth_key) {
        return Some(url);
    }
    // 4. Cline config
    if let Some(url) = scan_cline_config(auth_key) {
        return Some(url);
    }
    // 5. Pi provider configs
    if let Some(url) = scan_pi_configs(auth_key) {
        return Some(url);
    }
    // 6. Environment variables via RELIARY_UPSTREAM_URL (global fallback)
    //    (handled by proxy.rs resolve_upstream fallback)
    None
}

/// Scan ~/.reliary/proxy-routes.json for explicit auth→upstream mappings.
fn scan_proxy_routes(auth_key: &str) -> Option<String> {
    let routes_path = home_dir().join(".reliary/proxy-routes.json");
    let content = std::fs::read_to_string(routes_path).ok()?;
    let routes: std::collections::HashMap<String, String> =
        serde_json::from_str(&content).ok()?;
    routes.get(auth_key).map(|url| normalize_url(url))
}

/// Scan OpenCode's opencode.json for provider API keys matching the auth key.
fn scan_opencode_configs(auth_key: &str) -> Option<String> {
    let cfg_path = opencode_config_path()?;
    let content = std::fs::read_to_string(cfg_path).ok()?;
    let config: serde_json::Value = serde_json::from_str(&content).ok()?;
    let providers = config.get("provider")?.as_object()?;
    for (_name, provider) in providers {
        let options = provider.get("options")?.as_object()?;
        let api_key = options.get("apiKey")?.as_str()?;
        if api_key == auth_key {
            let base_url = options.get("baseURL")?.as_str()?;
            // Normalize: append /chat/completions if missing
            let url = normalize_url(base_url);
            return Some(url);
        }
    }
    None
}

/// Scan Claude Code's ~/.claude.json for provider API keys.
fn scan_claude_config(auth_key: &str) -> Option<String> {
    let home = home_dir();
    let claude_path = home.join(".claude.json");
    let content = std::fs::read_to_string(claude_path).ok()?;
    // Claude Code can have api keys under providers or directly
    // Also check top-level project configs
    let config: serde_json::Value = serde_json::from_str(&content).ok()?;
    // Check for Anthropic API key
    if let Some(api_key) = config.get("apiKey").and_then(|v| v.as_str()) {
        if api_key == auth_key {
            return Some("https://api.anthropic.com/v1/messages".to_string());
        }
    }
    // Check for custom keys under providers
    if let Some(providers) = config.get("providers").and_then(|v| v.as_object()) {
        for (_name, provider) in providers {
            if let Some(api_key) = provider.get("apiKey").and_then(|v| v.as_str()) {
                if api_key == auth_key {
                    if let Some(base_url) = provider.get("baseUrl").and_then(|v| v.as_str()) {
                        return Some(normalize_url(base_url));
                    }
                }
            }
        }
    }
    None
}

/// Scan Cline's cline_mcp_settings.json for provider API keys.
fn scan_cline_config(auth_key: &str) -> Option<String> {
    let home = home_dir();
    let cline_paths = vec![
        if cfg!(target_os = "macos") {
            home.join("Library/Application Support/Code/User/globalStorage/rooveterinery.cline/cline_mcp_settings.json")
        } else {
            home.join(".config/Code/User/globalStorage/rooveterinery.cline/cline_mcp_settings.json")
        },
        home.join(".config/Code/User/globalStorage/rooveterinary.cline/cline_mcp_settings.json"),
        home.join("AppData/Roaming/Code/User/globalStorage/rooveterinery.cline/cline_mcp_settings.json"),
    ];
    for path in &cline_paths {
        if !path.exists() { continue; }
        if let Some(url) = scan_single_cline_config(path, auth_key) {
            return Some(url);
        }
    }
    None
}

fn scan_single_cline_config(path: &PathBuf, auth_key: &str) -> Option<String> {
    let content = std::fs::read_to_string(path).ok()?;
    let config: serde_json::Value = serde_json::from_str(&content).ok()?;
    // Cline stores API keys inline in MCP server args, not in a providers structure.
    // Check for anthropic key
    if let Some(api_key) = config.get("apiKey").and_then(|v| v.as_str()) {
        if api_key == auth_key {
            return Some("https://api.anthropic.com/v1/messages".to_string());
        }
    }
    None
}

/// Scan Pi's ~/.pi/agent/models.json for provider API keys matching the auth key.
fn scan_pi_configs(auth_key: &str) -> Option<String> {
    let pi_config = home_dir().join(".pi/agent/models.json");
    let content = std::fs::read_to_string(pi_config).ok()?;
    let config: serde_json::Value = serde_json::from_str(&content).ok()?;
    let providers = config.get("providers")?.as_object()?;
    for (_name, provider) in providers {
        let api_field = provider.get("apiKey")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let resolved = resolve_env_var(api_field);
        if resolved == auth_key {
            if let Some(base_url) = provider.get("baseUrl").and_then(|v| v.as_str()) {
                let url = normalize_url(base_url);
                return Some(url);
            }
        }
    }
    None
}

/// Normalize a base URL: append /v1/chat/completions if missing and not Anthropic-style.
fn normalize_url(base_url: &str) -> String {
    let trimmed = base_url.trim_end_matches('/');
    if trimmed.ends_with("/chat/completions") || trimmed.ends_with("/v1/messages") || trimmed.contains("/v1/messages") {
        trimmed.to_string()
    } else if trimmed.starts_with("https://api.anthropic.com") || trimmed.contains("anthropic") {
        if trimmed.ends_with("/v1") {
            format!("{}/messages", trimmed)
        } else {
            format!("{}/v1/messages", trimmed)
        }
    } else if trimmed.ends_with("/v1") {
        format!("{}/chat/completions", trimmed)
    } else {
        format!("{}/v1/chat/completions", trimmed)
    }
}

/// Resolve an env var reference like "$OPENAI_API_KEY" to its value.
fn resolve_env_var(val: &str) -> String {
    if let Some(rest) = val.strip_prefix('$') {
        std::env::var(rest).unwrap_or_default()
    } else {
        val.to_string()
    }
}

fn opencode_config_path() -> Option<PathBuf> {
    let home = home_dir();
    if cfg!(target_os = "windows") {
        std::env::var("APPDATA").ok().map(|d| PathBuf::from(d).join("opencode").join("opencode.json"))
    } else if cfg!(target_os = "macos") {
        Some(home.join("Library/Application Support/opencode/opencode.json"))
    } else {
        Some(home.join(".config/opencode/opencode.json"))
    }
}

fn home_dir() -> PathBuf {
    std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .map(PathBuf::from)
        .unwrap_or_default()
}
