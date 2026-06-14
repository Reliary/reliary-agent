mod common;

fn build_multi_turn_conversation(turns: usize) -> Vec<serde_json::Value> {
    let mut messages = vec![
        serde_json::json!({"role": "system", "content": "You are a helpful coding assistant."}),
    ];
    for i in 0..turns {
        messages.push(serde_json::json!({"role": "user", "content": format!("what is {} + {}", i, i)}));
        messages.push(serde_json::json!({"role": "assistant", "content": format!(
            "Let me think about this carefully. First, I need to consider the mathematical operation. \
            The user is asking for the sum of {} and {}. Adding them together gives {}. \
            I should double-check my work. Yes, {} + {} = {}. The answer is correct.",
            i, i, i*2, i, i, i*2
        )}));
    }
    messages
}

#[test]
fn e2e_compression_reduces_tokens() {
    if common::skip_without_live() {
        eprintln!("skipping e2e_compression_reduces_tokens: set RELIARY_E2E_LIVE=1");
        return;
    }
    let key = common::load_api_key().expect("no API key found");
    let _guard = common::start_daemon();
    let client = common::http_client();

    // Build a 10-turn conversation
    let messages = build_multi_turn_conversation(10);

    let resp = client
        .post("http://127.0.0.1:9090/v1/chat/completions")
        .header("Authorization", format!("Bearer {}", key))
        .json(&serde_json::json!({
            "model": "deepseek/deepseek-v4-flash",
            "messages": messages,
            "stream": false,
        }))
        .send()
        .expect("request failed");

    assert_eq!(resp.status(), 200, "expected 200 OK");

    let savings = resp.headers().get("x-reliaty-savings-pct")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(0);

    eprintln!("compression savings: {}%", savings);
    assert!(savings > 0, "expected non-zero compression savings, got {}%", savings);
}

#[test]
fn e2e_compression_no_regression_on_short_conversation() {
    if common::skip_without_live() {
        return;
    }
    let key = common::load_api_key().expect("no API key found");
    let _guard = common::start_daemon();
    let client = common::http_client();

    // Short conversation (2 turns) — compression may not fire, but should not error
    let messages = build_multi_turn_conversation(2);

    let resp = client
        .post("http://127.0.0.1:9090/v1/chat/completions")
        .header("Authorization", format!("Bearer {}", key))
        .json(&serde_json::json!({
            "model": "deepseek/deepseek-v4-flash",
            "messages": messages,
            "stream": false,
        }))
        .send()
        .expect("request failed");

    assert_eq!(resp.status(), 200, "expected 200 OK on short conversation");
}
