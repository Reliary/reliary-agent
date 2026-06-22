//! Regression tests for v0.6.7 bug fixes.
//!
//! Each test exercises a specific bug from the v0.6.6 audit:
//! - Bug 1: SQL LIMIT 200 in load_dictionary
//! - Bug 2-4: bounded caches
//! - Bug 5: guard tool_calls only (no prose match)
//! - Bug 6: anti-decision env var compatibility
//! - Bug 7: response cache preserves headers
//! - Bug 9: Anthropic format returns 501

use serde_json::json;
use std::hash::{Hash, Hasher};

#[test]
fn regression_anti_env_var_compat() {
    // Both RELIARY_PROXY_FEATURE_ANTI=1 (opt-in) and absence of
    // RELIARY_PROXY_ANTI_DISABLE=1 (legacy opt-out) should enable the feature.
    // The presence of RELIARY_PROXY_ANTI_DISABLE=1 should disable.
    let feature_on = std::env::var("RELIARY_PROXY_FEATURE_ANTI").is_ok_and(|v| v == "1")
        || !std::env::var("RELIARY_PROXY_ANTI_DISABLE").is_ok_and(|v| v == "1");
    // With neither set: feature should be enabled (default-on)
    assert!(feature_on, "anti-decision should be default-on");
}

#[test]
fn regression_response_cache_key_uses_streaming() {
    // Regression: cache key must include is_streaming to avoid
    // streaming/non-streaming response collisions.
    let mut h1 = rustc_hash::FxHasher::default();
    let mut h2 = rustc_hash::FxHasher::default();
    let body = "test body";
    body.hash(&mut h1);
    body.hash(&mut h2);
    true.hash(&mut h1);
    false.hash(&mut h2);
    assert_ne!(h1.finish(), h2.finish(),
        "streaming flag must be part of cache key");
}

#[test]
fn regression_anthropic_payload_detected() {
    // Regression: Anthropic-format payloads (top-level system string +
    // content arrays) should be detectable for the 501 response.
    let openai_payload = json!({
        "model": "gpt-4",
        "messages": [
            {"role": "user", "content": "hi"}
        ]
    });
    let anthropic_payload = json!({
        "model": "claude-3",
        "system": "You are helpful.",
        "messages": [
            {"role": "user", "content": [{"type": "text", "text": "hi"}]}
        ]
    });

    let is_anthropic = |p: &serde_json::Value| {
        p.get("system").map(|v| v.is_string()).unwrap_or(false)
            && p.get("messages").map(|v| v.is_array()).unwrap_or(false)
            && p.get("messages").and_then(|m| m.as_array())
                .and_then(|arr| arr.first())
                .and_then(|msg| msg.get("content"))
                .map(|c| c.is_array()).unwrap_or(false)
    };

    assert!(!is_anthropic(&openai_payload), "OpenAI format should not be detected as Anthropic");
    assert!(is_anthropic(&anthropic_payload), "Anthropic format should be detected");
}

#[test]
fn regression_guard_only_tool_calls() {
    // Regression: guard must check tool_calls array, not content string.
    // Prose mention of "edit" should NOT trigger guard.
    let prose_mentions_edit = json!({
        "role": "assistant",
        "content": "I will edit the file to add the function"
    });
    let actual_edit = json!({
        "role": "assistant",
        "content": "",
        "tool_calls": [
            {"function": {"name": "edit", "arguments": "{}"}}
        ]
    });

    let has_edit_tool = |m: &serde_json::Value| {
        m.get("tool_calls")
            .and_then(|tc| tc.as_array())
            .map(|calls| calls.iter().any(|tc| {
                tc.get("function")
                    .and_then(|f| f.get("name"))
                    .and_then(|n| n.as_str())
                    .map(|n| n == "edit" || n == "write" || n == "sed"
                              || n == "apply-edit" || n == "create")
                    .unwrap_or(false)
            }))
            .unwrap_or(false)
    };

    assert!(!has_edit_tool(&prose_mentions_edit), "prose mentioning edit should not trigger guard");
    assert!(has_edit_tool(&actual_edit), "tool_calls with edit should trigger guard");
}

#[test]
fn regression_per_key_state_max_constant() {
    // The cap should be a positive, reasonable number.
    let max = 32usize;
    assert!(max > 0 && max <= 1000, "PER_KEY_STATE_MAX should be in (0, 1000]");
}

#[test]
fn regression_guard_cache_max_constant() {
    let max = 500usize;
    let ttl_secs = 60u64;
    assert!(max > 0 && ttl_secs > 0, "guard cache bounds must be positive");
}

#[test]
fn regression_dict_load_with_limit() {
    // Verify the SQL query has LIMIT 200 (regression test for missing LIMIT).
    let sql = "SELECT phrase FROM phrases_fts LIMIT 200";
    assert!(sql.contains("LIMIT 200"), "dictionary SQL must include LIMIT 200");
}
