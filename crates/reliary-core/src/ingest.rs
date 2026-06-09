use crate::session::{SessionState, ReadRecord, EditRecord, ErrorRecord};
use std::collections::HashMap;
use std::fs::File;
use std::io::{BufRead, BufReader};

pub fn parse_session_file(path: &str) -> Result<SessionState, String> {
    let file = File::open(path).map_err(|e| format!("Cannot open {}: {}", path, e))?;
    let reader = BufReader::new(file);
    let mut state = SessionState::default();
    let mut tool_calls: HashMap<String, (String, serde_json::Value)> = HashMap::new();
    let mut edit_counter: HashMap<String, usize> = HashMap::new();
    let mut read_counter: HashMap<String, usize> = HashMap::new();
    // Track last edit file path per toolCallId
    let mut edit_files: HashMap<String, String> = HashMap::new();

    for line in reader.lines() {
        let line = line.map_err(|e| format!("Read error: {}", e))?;
        let v: serde_json::Value = match serde_json::from_str(&line) { Ok(v) => v, Err(_) => continue };
        let typ = match v.get("type").and_then(|t| t.as_str()) { Some(t) => t, None => continue };
        match typ {
            "message" => {
                let msg = match v.get("message") { Some(m) => m, None => continue };
                let role = msg.get("role").and_then(|r| r.as_str()).unwrap_or("");

                // Record tool calls from assistant messages
                if role == "assistant" {
                    if let Some(content) = msg.get("content").and_then(|c| c.as_array()) {
                        for item in content {
                            if item.get("type").and_then(|t| t.as_str()) == Some("toolCall") {
                                let tool_id = item.get("id").and_then(|s| s.as_str()).unwrap_or("").to_string();
                                let tool_name = item.get("name").and_then(|s| s.as_str()).unwrap_or("").to_string();
                                let args = item.get("arguments").cloned().unwrap_or(serde_json::Value::Null);
                                tool_calls.insert(tool_id.clone(), (tool_name.clone(), args.clone()));
                                // Pre-extract edit file
                                if tool_name == "edit" {
                                    if let Some(obj) = args.as_object() {
                                        if let Some(file) = obj.get("file").and_then(|f| f.as_str()) {
                                            edit_files.insert(tool_id.clone(), file.to_string());
                                        }
                                    }
                                }
                            }
                        }
                    }
                }

                if role == "user" {
                    // Track user turns
                    state.turn_count += 1;
                }

                if role == "toolResult" {
                    let call_id = msg.get("toolCallId").and_then(|s| s.as_str()).unwrap_or("");
                    let tool_name = msg.get("toolName").and_then(|s| s.as_str()).unwrap_or("");

                    let has_content = msg.get("content").and_then(|c| c.as_array()).map(|a| !a.is_empty()).unwrap_or(false);

                    // Match tool call to tool result
                    if let Some((_name, args)) = tool_calls.remove(call_id) {
                        match tool_name {
                            "read" => {
                                let path = args.get("path").and_then(|p| p.as_str()).unwrap_or("").to_string();
                                if !path.is_empty() && has_content {
                                    *read_counter.entry(path.clone()).or_insert(0) += 1;
                                    let is_rerun = read_counter.get(&path).copied().unwrap_or(0) > 1;
                                    let content = msg.get("content").and_then(|c| c.as_array());
                                    let total_size: usize = content.map(|arr| {
                                        arr.iter().filter_map(|b| b.get("text").and_then(|t| t.as_str()).map(|s| s.len())).sum()
                                    }).unwrap_or(0);
                                    state.reads.push(ReadRecord { path, size: total_size, hash: format!("{:x}", total_size), is_rerun });
                                }
                            }
                            "bash" => {
                                let cmd = args.get("command").and_then(|c| c.as_str()).unwrap_or("");
                                if cmd.contains("cargo test") || cmd.contains("pytest") {
                                    if let Some(content) = msg.get("content").and_then(|c| c.as_array()) {
                                        for block in content {
                                            if block.get("type").and_then(|t| t.as_str()) == Some("text") {
                                                let text = block.get("text").and_then(|t| t.as_str()).unwrap_or("");
                                                // Detect pass/fail from test output directly
                                                let is_fail = text.contains("FAILED") || text.contains("failures:");
                                                let is_pass = !is_fail && text.contains("test result:");
                                                if is_fail || is_pass {
                                                    state.last_test_output = Some(text.chars().take(200).collect());
                                                    state.last_test_pass = is_pass;
                                                }
                                                if is_fail {
                                                    state.errors.push(ErrorRecord {
                                                        turn: state.turn_count,
                                                        summary: extract_error_summary(text),
                                                    });
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                            "edit" => {
                                let file = edit_files.remove(call_id).unwrap_or_default();
                                if let Some(edits_arr) = args.get("edits").and_then(|e| e.as_array()) {
                                    for edit in edits_arr {
                                        let old = edit.get("oldText").and_then(|e| e.as_str()).unwrap_or("").to_string();
                                        let new = edit.get("newText").and_then(|e| e.as_str()).unwrap_or("").to_string();
                                        let cnt = edit_counter.entry(file.clone()).or_insert(0);
                                        *cnt += 1;
                                        state.edits.push(EditRecord {
                                            file: file.clone(),
                                            line: old.lines().next().unwrap_or("").to_string(),
                                            attempt: *cnt,
                                            old_snippet: old.chars().take(40).collect(),
                                            new_snippet: new.chars().take(40).collect(),
                                            success: false,
                                        });
                                    }
                                }
                            }
                            _ => {}
                        }
                    }
                }
            }
            _ => {}
        }
    }

    // Mark last edit as successful if last test passed
    if state.last_test_pass {
        if let Some(last) = state.edits.last_mut() {
            last.success = true;
        }
    }

    Ok(state)
}

fn extract_error_summary(output: &str) -> String {
    for line in output.lines() {
        let t = line.trim();
        if t.contains("FAILED") || t.contains("panicked at") || t.contains("assertion") {
            return t.chars().take(120).collect();
        }
        if t.contains("expected `true`, got `false`") {
            return t.chars().take(120).collect();
        }
    }
    // Return first line that mentions "failure" or "failures:"
    for line in output.lines() {
        if line.contains("failure") {
            return line.chars().take(120).collect();
        }
    }
    output.lines().next().unwrap_or("").chars().take(120).collect()
}
