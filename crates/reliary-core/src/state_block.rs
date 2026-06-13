use crate::session::SessionState;

/// Build a ~150-token state block from session state + memory recall + risk analysis
pub fn build_state_block(state: &SessionState, turn_count: usize) -> String {
    if turn_count < 3 {
        return "early\n".to_string();
    }

    let mut parts: Vec<String> = Vec::new();

    // Turn counter
    parts.push(format!("turn: {}", turn_count));

    // Reads section: top unique reads by recency
    let reads = state.read_summary();
    if !reads.is_empty() {
        let read_lines: Vec<String> = reads.iter().rev().take(4).map(|r| {
            let dedup = if r.is_rerun { "[re-read]" } else { "" };
            let name = r.path.rsplit('/').next().unwrap_or(&r.path);
            let name = name.chars().take(25).collect::<String>().replace('\n', " ");
            format!("{}({}b){}", name, r.size, dedup)
        }).collect();
        parts.push(format!("reads: {}", read_lines.join(" | ")));
    }

    // Tests section
    if let Some(tout) = &state.last_test_output {
        let status = if state.last_test_pass { "PASS" } else { "FAIL" };
        let truncated: String = tout.chars().take(100).collect();
        parts.push(format!("tests: {} — {}", status, truncated.replace('\n', " ")));
    }

    // Edits section
    if !state.edits.is_empty() {
        let edit_lines: Vec<String> = state.edits.iter().rev().take(4).map(|e| {
            let marker = if e.success { "✓" } else { "✗" };
            format!("L{} \"…{}…\"→\"…{}…\"{}", e.attempt, &e.old_snippet.chars().take(20).collect::<String>(), &e.new_snippet.chars().take(20).collect::<String>(), marker)
        }).collect();
        parts.push(format!("edits: {}", edit_lines.join(" | ")));
    }

    // Error summary
    if !state.errors.is_empty() {
        let last_err = &state.errors[state.errors.len() - 1];
        parts.push(format!("error[t{}]: {}", last_err.turn, last_err.summary));
    }

    // Counts
    let re_reads = state.reads.iter().filter(|r| r.is_rerun).count();
    let edits_count = state.edits.len();
    parts.push(format!("session: {} turns, {} files read ({} re-reads), {} edits", 
        state.turn_count, state.read_summary().len(), re_reads, edits_count));

    format!("[state]\n{}\n", parts.join("\n"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::*;

    fn sample_state() -> SessionState {
        let mut s = SessionState {
            turn_count: 5,
            ..Default::default()
        };
        s.reads.push(ReadRecord { path: "/src/main.rs".into(), size: 400, hash: "a1b2".into(), is_rerun: false });
        s.reads.push(ReadRecord { path: "/src/lib.rs".into(), size: 800, hash: "c3d4".into(), is_rerun: true });
        s.edits.push(EditRecord { file: "main.rs".into(), line: "fn run(".into(), attempt: 1, old_snippet: "old_code".into(), new_snippet: "new_code".into(), success: false });
        s.edits.push(EditRecord { file: "main.rs".into(), line: "fn run(".into(), attempt: 2, old_snippet: "old_v2".into(), new_snippet: "new_v2".into(), success: true });
        s.last_test_output = Some("test result: FAILED. 3 passed; 1 failed".into());
        s.last_test_pass = false;
        s.errors.push(ErrorRecord { turn: 4, summary: "assertion failed at main.rs:12".into() });
        s
    }

    #[test]
    fn test_early_session() {
        let state = SessionState::default();
        let block = build_state_block(&state, 2);
        assert_eq!(block, "early\n");
    }

    #[test]
    fn test_builds_block() {
        let state = sample_state();
        let block = build_state_block(&state, 5);
        assert!(block.contains("[state]"));
        assert!(block.contains("reads:"));
        assert!(block.contains("tests:"));
    }

    #[test]
    fn test_read_summary_dedup() {
        let mut state = SessionState::default();
        state.reads.push(ReadRecord { path: "a.rs".into(), size: 100, hash: "h1".into(), is_rerun: false });
        state.reads.push(ReadRecord { path: "b.rs".into(), size: 200, hash: "h2".into(), is_rerun: true });
        let summary = state.read_summary();
        assert_eq!(summary.len(), 2);
    }
}
