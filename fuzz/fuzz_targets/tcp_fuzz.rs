use libfuzzer_sys::fuzz_target;

// Fuzz TCP command parsing — simulates malformed daemon protocol input
fuzz_target!(|data: &[u8]| {
    if data.is_empty() {
        return;
    }

    if let Ok(cmd) = std::str::from_utf8(data) {
        let trimmed = cmd.trim();
        if trimmed.is_empty() {
            return;
        }

        // Simulate daemon command dispatch — should never panic
        let parts: Vec<&str> = trimmed.splitn(10, ' ').collect();
        let _cmd = parts[0];

        // Simulate argument parsing for common commands
        for arg in &parts[1..] {
            if arg.starts_with('/') || arg.starts_with("--") || !arg.is_empty() {
                let _ = arg.parse::<u32>();
            }
        }
    }
});
