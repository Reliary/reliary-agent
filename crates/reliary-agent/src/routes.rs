use std::path::PathBuf;

/// Discover upstream URL for an auth key by scanning all known agent configs.
pub fn discover_upstream(auth_key: &str) -> Option<String> {
    // 1. Local proxy-routes.json (explicit user override, highest priority)
    if let Some(url) = scan_proxy_routes(auth_key) {
        return Some(url);
    }
    // 2. Pi provider configs
    if let Some(url) = scan_pi_configs(auth_key) {
        return Some(url);
    }
    // 3. Environment variables (generic fallback)
    if let Some(url) = scan_env_vars(auth_key) {
        return Some(url);
    }
    None
}

/// Scan ~/.reliary/proxy-routes.json for explicit auth→upstream mappings.
fn scan_proxy_routes(auth_key: &str) -> Option<String> {
    let routes_path = home_dir().join(".reliary/proxy-routes.json");
    let content = std::fs::read_to_string(routes_path).ok()?;
    let routes: std::collections::HashMap<String, String> =
        serde_json::from_str(&content).ok()?;
    routes.get(auth_key).cloned()
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
                // Append chat completions path if needed
                let url = if base_url.ends_with("/chat/completions") || base_url.ends_with("/v1/messages") {
                    base_url.to_string()
                } else {
                    format!("{}/chat/completions", base_url.trim_end_matches('/'))
                };
                return Some(url);
            }
        }
    }
    None
}

/// Scan environment variables for common API key patterns.
fn scan_env_vars(auth_key: &str) -> Option<String> {
    let env_key_map: [(&str, &str); 4] = [
        ("ANTHROPIC_API_KEY", "https://api.anthropic.com/v1/messages"),
        ("OPENAI_API_KEY", "https://api.openai.com/v1/chat/completions"),
        ("DEEPSEEK_API_KEY", "https://api.deepseek.com/v1/chat/completions"),
        ("RELIARY_UPSTREAM_URL", ""),  // Direct URL override, checked below
    ];

    for (env_var, default_url) in &env_key_map {
        if let Ok(val) = std::env::var(env_var) {
            if val == auth_key {
                if *env_var == "RELIARY_UPSTREAM_URL" {
                    // The env var IS the URL, not the key
                    return Some(auth_key.to_string());
                }
                return Some(default_url.to_string());
            }
        }
    }
    None
}

/// Resolve an env var reference like "$DEEPSEEK_API_KEY" to its value.
fn resolve_env_var(val: &str) -> String {
    if val.starts_with('$') {
        std::env::var(&val[1..]).unwrap_or_default()
    } else {
        val.to_string()
    }
}

fn home_dir() -> PathBuf {
    std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .map(PathBuf::from)
        .unwrap_or_default()
}
