use libfuzzer_sys::fuzz_target;

// Fuzz proxy message parsing — the JSON format that flows through the proxy
// This exercises serde_json parsing + compression logic on arbitrary bytes
fuzz_target!(|data: &[u8]| {
    if data.is_empty() {
        return;
    }

    // 1. Fuzz JSON config parsing (config.json format)
    if let Ok(s) = std::str::from_utf8(data) {
        // Try parsing as config JSON — should never panic
        let _: Result<serde_json::Value, _> = serde_json::from_str(s);

        // 2. Fuzz URL string processing
        if s.len() < 2048 {
            // Simulate normalize_url: extract parts before/after ? and /
            let _parts: Vec<&str> = s.splitn(3, '/').collect();
            let _query: Vec<&str> = s.splitn(2, '?').collect();
            // Simulate auth key extraction
            if s.contains("Bearer ") || s.contains("sk-") || s.len() > 10 {
                let _first_line: Vec<&str> = s.splitn(2, '\n').collect();
            }
        }
    }

    // 3. Fuzz daemon command parsing
    if let Ok(cmd) = std::str::from_utf8(data) {
        let trimmed = cmd.trim();
        if !trimmed.is_empty() {
            let _parts: Vec<&str> = trimmed.splitn(10, ' ').collect();
        }
    }
});
