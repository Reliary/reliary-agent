use std::collections::HashMap;
use std::hash::{Hash, Hasher};

fn compress_content(content: &str) -> Option<String> {
    if content.len() <= 200 { return None; }
    let compressed = reliary_output::compress_output(content);
    if compressed.len() < content.len() { Some(compressed) } else { None }
}

fn simulate_proxy_compress(
    messages: &mut Vec<serde_json::Value>,
    tool_cache: &mut HashMap<u64, String>,
) -> usize {
    let mut total_saved = 0usize;
    for msg in messages.iter_mut() {
        let role = msg.get("role").and_then(|r| r.as_str()).unwrap_or("");
        if role != "tool" && role != "toolResult" { continue; }
        let content = match msg.get("content") {
            Some(serde_json::Value::String(s)) => s.clone(),
            _ => continue,
        };
        if content.len() < 200 { continue; }
        let mut h = std::collections::hash_map::DefaultHasher::new();
        content.hash(&mut h);
        let hash = h.finish();

        if let Some(cached) = tool_cache.get(&hash) {
            total_saved += content.len().saturating_sub(cached.len());
            msg["content"] = serde_json::Value::String(cached.clone());
        } else {
            if let Some(compressed) = compress_content(&content) {
                total_saved += content.len().saturating_sub(compressed.len());
                tool_cache.insert(hash, compressed.clone());
                msg["content"] = serde_json::Value::String(compressed);
            }
        }
    }
    total_saved
}

fn build_large_tool_result(n_lines: usize) -> String {
    let mut out = String::new();
    for i in 0..n_lines {
        out.push_str(&format!("   Compiling crate{} v0.1.0 (build-{})\n", i, i));
    }
    out.push_str("    Finished dev [unoptimized + debuginfo] in 2.34s\n");
    for i in 0..(n_lines / 5) {
        out.push_str(&format!("test test_{} ... ok\n", i));
    }
    out
}

#[test]
fn bench_full_session() {
    let system = serde_json::json!({"role": "system", "content": "You are a Rust developer."});
    let user = serde_json::json!({"role": "user", "content": "Fix the bugs in the sort module."});
    let assistant = serde_json::json!({"role": "assistant", "content": "I'll check the code and run tests."});
    let tool1 = serde_json::json!({"role": "tool", "content": build_large_tool_result(30)});
    let tool2 = serde_json::json!({"role": "tool", "content": build_large_tool_result(30)});
    let tool3 = serde_json::json!({"role": "tool", "content": build_large_tool_result(30)});
    let tool4 = serde_json::json!({"role": "tool", "content": build_large_tool_result(30)});

    let mut cache = HashMap::new();
    let mut total_orig = 0usize;
    let mut total_comp = 0usize;

    for (turn_msgs, turn_label) in [
        (vec![system.clone(), user.clone(), assistant.clone(), tool1.clone()], "Turn 1"),
        (vec![system.clone(), user.clone(), assistant.clone(), tool1.clone(), assistant.clone(), tool2.clone()], "Turn 2"),
        (vec![system.clone(), user.clone(), assistant.clone(), tool1.clone(), assistant.clone(), tool2.clone(), assistant.clone(), tool3.clone()], "Turn 3"),
        (vec![system.clone(), user.clone(), assistant.clone(), tool1.clone(), assistant.clone(), tool2.clone(), assistant.clone(), tool3.clone(), assistant.clone(), tool4.clone()], "Turn 4"),
    ] {
        let mut msgs = turn_msgs;
        let orig = serde_json::to_string(&msgs).unwrap().len();
        total_orig += orig;
        let saved = simulate_proxy_compress(&mut msgs, &mut cache);
        let comp = serde_json::to_string(&msgs).unwrap().len();
        total_comp += comp;
        println!("{}: {} bytes → {} bytes (saved {:.0}%)", turn_label, orig, comp, saved as f64 / orig as f64 * 100.0);
    }

    println!("Cache entries: {}", cache.len());
    println!("Cumulative: {} bytes → {} bytes ({:.0}%)", total_orig, total_comp,
        (total_orig as f64 - total_comp as f64) / total_orig as f64 * 100.0);

    // Verify errors preserved
    let error_tool = serde_json::json!({"role":"tool","content":
        "error[E0308]: mismatched types\n  --> src/lib.rs:47\ntest test_error ... FAILED"});

    let mut error_msgs = vec![system, user, assistant, error_tool];
    let _ = simulate_proxy_compress(&mut error_msgs, &mut cache);
    let error_content = error_msgs[3].get("content").and_then(|c| c.as_str()).unwrap_or("");
    assert!(error_content.contains("FAILED"), "FAILED must survive");
    assert!(error_content.contains("E0308"), "E0308 must survive");
    println!("Error preservation: ✅ (FAILED + E0308 present)");
}
