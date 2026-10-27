//! End-to-end tests for the MCP stdio protocol.
//!
//! These drive the real `reliary mcp` binary over the same line-delimited
//! JSON-RPC transport that Claude Code, OpenCode, Cline and Pi use. They
//! assert protocol conformance (handshake, tools/list, tools/call, error
//! codes, malformed input survivability) and that every advertised tool is
//! actually callable.
//!
//! Run: `cargo test -p reliary-agent --test e2e_mcp`
mod common;

use common::{Fixture, Mcp};
use serde_json::json;
use std::path::Path;
use std::time::Duration;

// ── 1. Handshake ──────────────────────────────────────────────────────────

#[test]
fn e2e_mcp_initialize_handshake() {
    let fx = Fixture::new();
    let mut mcp = Mcp::start(fx.path());

    let resp = mcp.initialize();
    assert_eq!(resp["jsonrpc"], "2.0", "must be JSON-RPC 2.0: {}", resp);
    assert_eq!(resp["result"]["protocolVersion"], "2024-11-05");
    assert!(
        resp["result"]["capabilities"]["tools"].is_object(),
        "must advertise tools capability: {}",
        resp
    );
    assert!(
        resp["result"]["serverInfo"]["name"].is_string(),
        "must report serverInfo.name: {}",
        resp
    );
    assert!(mcp.is_alive(), "server must stay alive after initialize");
}

#[test]
fn e2e_mcp_initialize_creates_index_when_absent() {
    let fx = Fixture::new();
    let index = fx.path().join(".reliary/index.sqlite");
    assert!(
        !index.exists(),
        "fixture must start unindexed"
    );

    let mut mcp = Mcp::start(fx.path());
    let resp = mcp.initialize();
    assert!(mcp.is_alive());

    // The fixture is a git project, so auto-trust is required to run: the
    // index must exist afterwards and the response must say it was created.
    assert!(
        index.exists(),
        "initialize on an unindexed git project must auto-trust and create \
         the index; response was {}",
        resp
    );
    assert_eq!(
        resp["result"]["serverInfo"]
            .get("auto_trusted")
            .and_then(|v| v.as_bool()),
        Some(true),
        "auto_trusted must be true when the index was just created: {}",
        resp
    );

    // The freshly created index must actually be queryable.
    let text = mcp.call_tool_text(
        "reliary_find_references",
        json!({ "name": "alpha", "def_only": true }),
    );
    assert!(
        text.contains("alpha"),
        "auto-created index must answer queries: {}",
        text
    );
}

/// The auto-trust must NOT fire for a non-git directory (no index is created).
#[test]
fn e2e_mcp_initialize_skips_autotrust_outside_git() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("loose.rs"), "pub fn loose() {}\n").unwrap();
    let index = dir.path().join(".reliary/index.sqlite");

    let mut mcp = Mcp::start(dir.path());
    let resp = mcp.initialize();
    assert!(mcp.is_alive());
    assert!(
        !index.exists(),
        "a non-git directory must not be auto-trusted (would surprise users \
         by creating indexes in arbitrary dirs); response was {}",
        resp
    );
}

// ── 2. tools/list conformance ─────────────────────────────────────────────

#[test]
fn e2e_mcp_tools_list_schema_conformance() {
    let fx = Fixture::new();
    let mut mcp = Mcp::start(fx.path());
    mcp.initialize();

    let tools = mcp.list_tools();
    assert!(
        tools.len() >= 6,
        "default menu should expose at least 6 tools, got {}",
        tools.len()
    );

    let mut names = Vec::new();
    for t in &tools {
        let name = t["name"].as_str().unwrap_or("");
        assert!(!name.is_empty(), "tool missing name: {}", t);
        assert!(
            name.starts_with("reliary_"),
            "tool name must be namespaced: {}",
            name
        );

        let desc = t["description"].as_str().unwrap_or("");
        assert!(
            desc.len() > 20,
            "tool {} needs a useful description (got {} chars)",
            name,
            desc.len()
        );

        let schema = t.get("inputSchema").expect("inputSchema required");
        assert_eq!(
            schema["type"], "object",
            "tool {} inputSchema.type must be object",
            name
        );
        assert!(
            schema.get("properties").is_some(),
            "tool {} inputSchema needs properties",
            name
        );
        names.push(name.to_string());
    }

    // No duplicates.
    let mut sorted = names.clone();
    sorted.sort();
    sorted.dedup();
    assert_eq!(sorted.len(), names.len(), "tools/list has duplicate names");

    // The entry points the shipped prompt tells the model to use.
    for required in [
        "reliary_search",
        "reliary_find_references",
        "reliary_call_graph",
        "reliary_list_methods",
        "reliary_find_dead_code",
        "reliary_describe",
    ] {
        assert!(
            names.iter().any(|n| n == required),
            "required tool {} missing from tools/list: {:?}",
            required,
            names
        );
    }
}

#[test]
fn e2e_mcp_hook_scripts_only_name_advertised_tools() {
    // The SessionStart reminder and the Claude sift hook are injected into
    // agent prompts. A hook that names a tool the agent cannot call is worse
    // than no hook: it manufactures dead-ends. Every `reliary_*` identifier a
    // shipped hook mentions must be present in the live tools/list.
    let fx = Fixture::new();
    let mut mcp = Mcp::start(fx.path());
    mcp.initialize();
    let advertised: std::collections::HashSet<String> = mcp
        .list_tools()
        .iter()
        .filter_map(|t| t["name"].as_str().map(|s| s.to_string()))
        .collect();

    let hooks_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("hooks");
    let mut checked = 0;
    for entry in std::fs::read_dir(&hooks_dir).expect("hooks dir must exist") {
        let path = entry.expect("dir entry").path();
        if path.extension().and_then(|e| e.to_str()) != Some("sh") {
            continue;
        }
        let Some(fname) = path.file_name().and_then(|n| n.to_str()) else { continue };
        // Only scripts that reach an agent prompt. (The pretooluse hook emits
        // no tool guidance — it only rewrites commands.)
        if fname != "claude-session-reminder.sh" {
            continue;
        }
        let body = std::fs::read_to_string(&path).expect("read hook");
        for cap in regex_lite_tool_names(&body) {
            checked += 1;
            assert!(
                advertised.contains(&cap),
                "{} advertises `{}`, which tools/list does not expose. \
                 The agent cannot call it. Advertised: {:?}",
                fname,
                cap,
                advertised
            );
        }
    }
    assert!(checked >= 3, "expected the reminder to name several tools, found {}", checked);
}

/// Minimal extractor for `reliary_<lower_snake>` tokens, avoiding a regex
/// dependency: scan for the prefix and take the following identifier run.
fn regex_lite_tool_names(body: &str) -> Vec<String> {
    let mut out = Vec::new();
    let bytes = body.as_bytes();
    let needle = b"reliary_";
    let mut i = 0;
    while i + needle.len() < bytes.len() {
        if &bytes[i..i + needle.len()] == needle {
            let start = i;
            let mut j = i + needle.len();
            while j < bytes.len()
                && (bytes[j].is_ascii_lowercase() || bytes[j].is_ascii_digit() || bytes[j] == b'_')
            {
                j += 1;
            }
            let tok = std::str::from_utf8(&bytes[start..j]).unwrap_or("");
            // Trim a trailing underscore from prose like "reliary_ ".
            let tok = tok.trim_end_matches('_');
            if tok.len() > needle.len() {
                out.push(tok.to_string());
            }
            i = j;
        } else {
            i += 1;
        }
    }
    out.sort();
    out.dedup();
    out
}

#[test]
fn e2e_mcp_tools_list_exposes_input_examples() {
    let fx = Fixture::new();
    let mut mcp = Mcp::start(fx.path());
    mcp.initialize();

    let tools = mcp.list_tools();
    let fr = tools
        .iter()
        .find(|t| t["name"] == "reliary_find_references")
        .expect("find_references must be advertised");
    let examples = fr["inputSchema"]
        .get("input_examples")
        .and_then(|e| e.as_array())
        .expect("find_references should provide input_examples (Anthropic guidance)");
    assert!(!examples.is_empty(), "input_examples must not be empty");
}

// ── 3. tools/call — every advertised tool is callable ─────────────────────

fn minimal_args_for(tool: &str) -> serde_json::Value {
    match tool {
        "reliary_search" => json!({ "query": "alpha" }),
        "reliary_find_references" => json!({ "name": "alpha" }),
        "reliary_goto_def" => json!({ "name": "alpha" }),
        "reliary_call_graph" => json!({ "name": "alpha" }),
        "reliary_list_methods" => json!({ "name": "Fixture" }),
        "reliary_find_dead_code" => json!({ "path": "src" }),
        "reliary_describe" => json!({ "name": "alpha" }),
        "reliary_similar" => json!({ "name": "alpha" }),
        "reliary_verify" => json!({ "text": "alpha at src/lib.rs:3" }),
        _ => json!({}),
    }
}

#[test]
fn e2e_mcp_every_advertised_tool_responds() {
    let fx = Fixture::new();
    fx.trust();
    let mut mcp = Mcp::start(fx.path());
    mcp.initialize();

    for tool in mcp.list_tools() {
        let name = tool["name"].as_str().unwrap().to_string();
        let resp = mcp.call_tool(&name, minimal_args_for(&name));

        assert!(
            resp["jsonrpc"] == "2.0",
            "{}: response must be JSON-RPC 2.0: {}",
            name,
            resp
        );

        if resp.get("error").is_none() {
            let content = resp["result"]["content"]
                .as_array()
                .unwrap_or_else(|| panic!("{}: success must carry content array: {}", name, resp));
            assert!(
                !content.is_empty(),
                "{}: content array must not be empty",
                name
            );
            assert!(
                content[0]["type"] == "text",
                "{}: first content item must be text: {}",
                name,
                content[0]
            );
        }
        assert!(mcp.is_alive(), "{}: server died", name);
    }
}

#[test]
fn e2e_mcp_find_references_returns_definition_with_source() {
    let fx = Fixture::new();
    fx.trust();
    let mut mcp = Mcp::start(fx.path());
    mcp.initialize();

    let text = mcp.call_tool_text(
        "reliary_find_references",
        json!({ "name": "beta", "def_only": true }),
    );
    assert!(text.contains("beta"), "answer must name the symbol: {}", text);
    assert!(
        text.contains("other.rs"),
        "must locate beta in src/other.rs: {}",
        text
    );
    // V38+: raw code evidence accompanies the answer.
    assert!(
        text.contains("pub fn beta"),
        "must include raw source evidence: {}",
        text
    );
}

#[test]
fn e2e_mcp_search_never_returns_empty() {
    let fx = Fixture::new();
    fx.trust();
    let mut mcp = Mcp::start(fx.path());
    mcp.initialize();

    let text = mcp.call_tool_text("reliary_search", json!({ "query": "zzz_no_such_symbol_zzz" }));
    assert!(
        !text.trim().is_empty(),
        "search must be silent never-empty (closest files or an explanation)"
    );
}

// ── 4. Error handling / protocol robustness ───────────────────────────────

/// `search` and `find_references` must agree about the index.
///
/// They did not: the index stores `stem_identifier` of every token, but
/// `search` lowercased the query without stemming it. `getEnumValues` is
/// indexed as `getenumvalu`; the query `getenumvalues` matched nothing and
/// every fallback returned empty, so `search` answered `[]` for a symbol that
/// `find_references` found. Reported as 70/80 misses on zod.
#[test]
fn e2e_mcp_search_finds_a_camel_case_symbol_that_porter_stems() {
    let fx = Fixture::new();
    fx.trust();
    // The fixture has `beta` only; add a name whose Porter stem differs from
    // its lowercase form, which is the shape that broke.
    std::fs::write(
        fx.path().join("src/stemmed.rs"),
        "/// Collects enum values.\npub fn getEnumValues(entries: &[u8]) -> Vec<u8> {\n    entries.to_vec()\n}\n",
    )
    .unwrap();
    fx.trust();

    let mut mcp = Mcp::start(fx.path());
    mcp.initialize();

    let text = mcp.call_tool_text("reliary_search", json!({ "query": "getEnumValues" }));
    assert!(
        text.contains("stemmed.rs"),
        "search must find a camelCase symbol whose stem differs from its lowercase form; got: {text}"
    );
    // ...and the two tools must agree.
    let refs = mcp.call_tool_text(
        "reliary_find_references",
        json!({ "name": "getEnumValues", "def_only": true }),
    );
    assert!(
        refs.contains("stemmed.rs"),
        "find_references must find the same symbol; got: {refs}"
    );
}

/// `search` must answer a symbol query with the definition, not just files.
///
/// Models call `search` for symbol questions even though it is documented for
/// "I do not know the name" (the q9 failure). A file list leaves them without
/// the definition, so the exact-identifier hint was added. Two bugs had to be
/// fixed for it to work at all:
///
///  1. a bare `trust` populates `phrases` but not `occurrence`, so the hint
///     was silently absent until someone ran `build-all` — the V77 class;
///  2. ordering definitions by line number across files answered
///     `classify_structural` from a 1-use test file because 80 < 184. Rank by
///     the name's use count in the defining file instead.
#[test]
fn e2e_mcp_search_answers_a_symbol_query_with_its_definition() {
    let fx = Fixture::new();
    fx.trust();          // bare trust: no occurrence rows yet
    let mut mcp = Mcp::start(fx.path());
    mcp.initialize();

    // `path: "."` is the shape that shipped broken in 0.9.3: the root came back
    // as `<cwd>/.`, the project-relative prefix strip failed, and the hint
    // emitted an absolute path while every other tool emitted a relative one.
    let text = mcp.call_tool_text("reliary_search", json!({ "query": "alpha", "path": "." }));
    let first = text.lines().next().unwrap_or("");
    assert!(
        first.starts_with("alpha is defined at "),
        "search must lead with the definition for an exact identifier; got: {first:?}"
    );
    assert!(
        first.contains("lib.rs"),
        "the hint must name the defining file; got: {first:?}"
    );
    assert!(
        !first.contains(fx.path().to_string_lossy().as_ref()),
        "the hint must be project-relative however `path` is spelled; got: {first:?}"
    );

    // A natural-language query must be unchanged: no definition line.
    let prose = mcp.call_tool_text("reliary_search", json!({ "query": "how does alpha work" }));
    assert!(
        !prose.lines().next().unwrap_or("").contains(" is defined at "),
        "a multi-word query must not emit a definition hint; got: {prose:?}"
    );

    // def_only must use the SAME path convention as search. It used to print a
    // bare basename (`a.rs`), which is ambiguous whenever two modules share a
    // filename — tokio has 97 colliding basenames, 54 of them `mod.rs`.
    let def = mcp.call_tool_text(
        "reliary_find_references",
        json!({ "name": "alpha", "path": ".", "def_only": true }),
    );
    let def_first = def.lines().next().unwrap_or("");
    // The DIRECTORY component is what distinguishes the two conventions: a
    // bare basename check passes vacuously, which is how the first version of
    // this test passed with the bug still in place.
    assert!(
        def_first.contains("src/lib.rs:"),
        "def_only must print the project-relative path, not a bare basename; got: {def_first:?}"
    );
    assert!(
        !def_first.contains(fx.path().to_string_lossy().as_ref()),
        "def_only must be project-relative; got: {def_first:?}"
    );
    // The two tools must agree on the spelling of the same file.
    assert!(
        first.contains("src/lib.rs:") && def_first.contains("src/lib.rs:"),
        "search and def_only must use one path convention: {first:?} vs {def_first:?}"
    );
}

#[test]
fn e2e_mcp_unknown_method_returns_method_not_found() {
    let fx = Fixture::new();
    let mut mcp = Mcp::start(fx.path());
    mcp.initialize();

    let resp = mcp.request("tools/nonexistent", json!({}));
    assert_eq!(
        resp["error"]["code"], -32601,
        "unknown method must be -32601: {}",
        resp
    );
    assert!(resp["error"]["message"].is_string());
    assert!(mcp.is_alive());
}

#[test]
fn e2e_mcp_notifications_get_no_response() {
    let fx = Fixture::new();
    let mut mcp = Mcp::start(fx.path());
    mcp.initialize();

    mcp.send_raw(&json!({
        "jsonrpc": "2.0",
        "method": "notifications/initialized",
    }));
    // A notification has no id; the server must stay silent.
    assert!(
        mcp.try_read_response(Duration::from_millis(400)).is_none(),
        "server must not respond to notifications"
    );
    assert!(mcp.is_alive());
}

#[test]
fn e2e_mcp_tools_call_missing_params_returns_invalid_params() {
    let fx = Fixture::new();
    let mut mcp = Mcp::start(fx.path());
    mcp.initialize();

    // No `params` key at all — the JSON-RPC request is structurally invalid.
    mcp.send_raw(&json!({
        "jsonrpc": "2.0",
        "id": 777,
        "method": "tools/call",
    }));
    let resp = mcp.read_response(Duration::from_secs(30));
    assert_eq!(
        resp["error"]["code"], -32602,
        "tools/call without params must be -32602: {}",
        resp
    );
    assert!(mcp.is_alive());

    // An empty params object is a different, valid request: it names no tool,
    // so the server reports an unknown-tool error rather than a protocol error.
    let resp = mcp.call_tool("", json!({}));
    assert!(
        resp.get("error").is_some(),
        "empty tool name must produce an error: {}",
        resp
    );
    assert!(mcp.is_alive());
}

#[test]
fn e2e_mcp_unknown_tool_errors_but_stays_alive() {
    let fx = Fixture::new();
    let mut mcp = Mcp::start(fx.path());
    mcp.initialize();

    let resp = mcp.call_tool("reliary_no_such_tool", json!({}));
    assert!(
        resp.get("error").is_some() || resp["result"]["content"][0]["text"].is_string(),
        "unknown tool must produce an error or an explanatory text result: {}",
        resp
    );
    assert!(mcp.is_alive(), "unknown tool must not kill the server");
}

#[test]
fn e2e_mcp_malformed_json_does_not_kill_server() {
    let fx = Fixture::new();
    let mut mcp = Mcp::start(fx.path());
    mcp.initialize();

    // Garbage, truncated JSON, a JSON array, and a JSON scalar.
    mcp.send_line("this is not json at all");
    mcp.send_line("{\"jsonrpc\": \"2.0\", \"id\": 7, \"method\": \"tools/list\"");
    mcp.send_line("[1, 2, 3]");
    mcp.send_line("\"just a string\"");
    mcp.send_line("{}");
    mcp.send_line("");

    // Malformed input is either dropped or answered with an error carrying a
    // null id. Drain those first so the next assertion reads the real reply.
    while let Some(resp) = mcp.try_read_response(Duration::from_millis(250)) {
        assert!(
            resp.get("error").is_some(),
            "a line that is not a valid request must not produce a result: {}",
            resp
        );
    }

    // The server must still answer a well-formed request afterwards.
    let resp = mcp.request("tools/list", json!({}));
    assert!(
        resp["result"]["tools"].is_array(),
        "server must recover after malformed input: {}",
        resp
    );
    assert!(mcp.is_alive());
}

#[test]
fn e2e_mcp_tool_with_wrong_arg_types_does_not_panic() {
    let fx = Fixture::new();
    fx.trust();
    let mut mcp = Mcp::start(fx.path());
    mcp.initialize();

    // `name` should be a string; send numbers, arrays, objects, nulls.
    for bad in [
        json!({ "name": 12345 }),
        json!({ "name": ["alpha"] }),
        json!({ "name": { "x": 1 } }),
        json!({ "name": null }),
        json!({ "name": "alpha", "path_filter": 42 }),
        json!({ "name": "alpha", "limit": "not-a-number" }),
    ] {
        let resp = mcp.call_tool("reliary_find_references", bad.clone());
        assert!(
            resp.get("result").is_some() || resp.get("error").is_some(),
            "malformed args must yield a result or error, not a crash: {} -> {}",
            bad,
            resp
        );
        assert!(mcp.is_alive(), "server died on args {}", bad);
    }
}

#[test]
fn e2e_mcp_path_traversal_is_rejected() {
    let fx = Fixture::new();
    fx.trust();
    let marker = fx.path().join("outside.rs");
    std::fs::write(&marker, "pub fn outside() {}\n").unwrap();
    let mut mcp = Mcp::start(fx.path());
    mcp.initialize();

    // Every path-accepting tool must reject an escape from the workdir with an
    // explicit error — never silently answer from the escaped location.
    let escapes = ["../", "../../", "../../../../etc", "/etc", "/tmp"];
    for escape in escapes {
        for (tool, arg) in [
            ("reliary_search", json!({ "query": "outside", "path": escape })),
            ("reliary_find_dead_code", json!({ "path": escape })),
        ] {
            let resp = mcp.call_tool(tool, arg.clone());
            assert!(
                mcp.is_alive(),
                "{} died on escape {}",
                tool,
                escape
            );
            // V77: `path` now selects a *project*. An escape names a directory
            // with no `.reliary/` index, so resolution fails and the tool reports
            // "cannot open index" (or a path error) — it must never read files
            // from outside an indexed project.
            match resp.get("error") {
                Some(err) => {
                    let msg = err["message"].as_str().unwrap_or("");
                    assert!(
                        msg.contains("index")
                            || msg.contains("path")
                            || msg.contains("escapes")
                            || msg.contains("invalid"),
                        "{} escape {} must be rejected, got: {}",
                        tool,
                        escape,
                        msg
                    );
                }
                None => {
                    // /tmp is a valid absolute path but must still be refused
                    // (outside the workdir). A success here is a real failure.
                    // P3.1: an escaping path_filter is an explicit scoped-empty
                    // message ("escapes the project"), not silent whole-tree
                    // data — so the message itself is the assertion artifact.
                    let content = resp["result"]["content"][0]["text"]
                        .as_str()
                        .unwrap_or("");
                    let is_explicit_rejection = content.contains("escapes the project");
                    assert!(
                        is_explicit_rejection
                            || (!content.contains("outside.rs") && !content.contains("/etc")),
                        "{} escape {} returned data from outside the workdir: {}",
                        tool,
                        escape,
                        content
                    );
                }
            }
        }
    }
}

#[test]
fn e2e_mcp_no_index_degrades_gracefully() {
    // A directory that is not a git repo and has no index: tools must explain
    // rather than crash. Auto-trust must not create an index here (not a git
    // repo), so this exercises the genuine no-index path.
    let dir = tempfile::tempdir().unwrap();
    assert!(
        !dir.path().join(".reliary/index.sqlite").exists(),
        "non-git fixture must stay unindexed"
    );
    let mut mcp = Mcp::start(dir.path());
    mcp.initialize();
    assert!(
        !dir.path().join(".reliary/index.sqlite").exists(),
        "initialize must not auto-trust a non-git directory"
    );

    // Each tool must return an informational result (or explicit error) whose
    // text explains that no index exists — not an empty or confusing answer.
    for (tool, args) in [
        ("reliary_find_references", json!({ "name": "anything" })),
        ("reliary_search", json!({ "query": "anything" })),
    ] {
        let resp = mcp.call_tool(tool, args.clone());
        assert!(mcp.is_alive(), "{} died with no index", tool);
        let text = resp["result"]["content"][0]["text"]
            .as_str()
            .or_else(|| resp["error"]["message"].as_str())
            .unwrap_or("");
        let lower = text.to_ascii_lowercase();
        assert!(
            lower.contains("no index")
                || lower.contains("not indexed")
                || lower.contains("trust")
                || lower.contains("index first"),
            "{} with no index must say so: {} -> {}",
            tool,
            args,
            resp
        );
    }
}

#[test]
fn e2e_mcp_parallel_tool_calls_are_answered_in_order() {
    // Real agents batch parallel tool calls in one assistant message.
    // The server is single-threaded, so it must answer each in turn.
    let fx = Fixture::new();
    fx.trust();
    let mut mcp = Mcp::start(fx.path());
    mcp.initialize();

    let names = ["alpha", "beta", "alpha"];
    for (i, n) in names.iter().enumerate() {
        mcp.send_raw(&json!({
            "jsonrpc": "2.0",
            "id": 100 + i,
            "method": "tools/call",
            "params": { "name": "reliary_find_references", "arguments": { "name": n } },
        }));
    }

    let mut ids = Vec::new();
    let mut texts = Vec::new();
    for _ in 0..names.len() {
        let resp = mcp.read_response(Duration::from_secs(30));
        ids.push(resp["id"].as_i64().unwrap_or(-1));
        assert!(
            resp.get("error").is_none(),
            "batched call must succeed: {}",
            resp
        );
        texts.push(
            resp["result"]["content"][0]["text"]
                .as_str()
                .unwrap_or("")
                .to_string(),
        );
    }
    assert_eq!(ids, vec![100, 101, 102], "responses must preserve request order");
    // Each response must correspond to its own request (alpha/beta/alpha).
    assert!(
        texts[0].contains("alpha"),
        "response 0 must answer the alpha query: {}",
        texts[0]
    );
    assert!(
        texts[1].contains("beta"),
        "response 1 must answer the beta query: {}",
        texts[1]
    );
    assert_eq!(
        texts[0], texts[2],
        "identical parallel requests must yield identical answers"
    );
    assert!(mcp.is_alive());
}

// ── V77: project resolution independent of the server's launch CWD ────────

/// The core bug this fixes: an agent launches `reliary mcp` from somewhere
/// other than the indexed project (a global MCP config, or a client that does
/// not set the spawn cwd). Passing the project as the `path` argument must
/// select it.
#[test]
fn e2e_mcp_resolves_project_from_path_hint() {
    let fx = Fixture::new();
    fx.trust();
    let elsewhere = tempfile::tempdir().expect("tempdir");
    let mut mcp = Mcp::start(elsewhere.path());
    mcp.initialize();

    let proj = fx.path().to_string_lossy().to_string();
    let text = mcp.call_tool_text(
        "reliary_find_references",
        json!({ "name": "beta", "def_only": true, "path": proj }),
    );
    assert!(
        text.contains("other.rs"),
        "a `path` argument must select the project regardless of the server's CWD \
         (expected beta's definition in other.rs): {}",
        text
    );
    assert!(mcp.is_alive());
}

/// A client that advertises the `roots` capability (OpenCode does) must receive
/// a `roots/list` request, and the returned workspace root must become the index
/// for path-less tool calls.
#[test]
fn e2e_mcp_resolves_project_from_roots() {
    let fx = Fixture::new();
    fx.trust();
    let elsewhere = tempfile::tempdir().expect("tempdir");
    let mut mcp = Mcp::start(elsewhere.path());

    mcp.send_raw(&json!({
        "jsonrpc": "2.0",
        "id": 900,
        "method": "initialize",
        "params": { "protocolVersion": "2024-11-05", "capabilities": { "roots": {} } }
    }));
    let init = mcp.read_response(Duration::from_secs(30));
    assert_eq!(init["id"], 900, "initialize must be answered first: {}", init);

    // The server must now ask for the roots.
    let req = mcp.read_response(Duration::from_secs(30));
    assert_eq!(
        req["method"], "roots/list",
        "a client advertising roots must be asked for them: {}",
        req
    );
    let rid = req["id"].clone();
    let uri = format!("file://{}", fx.path().to_string_lossy());
    mcp.send_raw(&json!({
        "jsonrpc": "2.0",
        "id": rid,
        "result": { "roots": [{ "uri": uri, "name": "fixture" }] }
    }));

    // A tool call with no `path` must now resolve via the client root.
    let text = mcp.call_tool_text(
        "reliary_find_references",
        json!({ "name": "beta", "def_only": true }),
    );
    assert!(
        text.contains("other.rs"),
        "the client-advertised root must select the project (expected beta in other.rs): {}",
        text
    );
    assert!(mcp.is_alive());
}

/// Negative control for the stub-shadowing / stub-creation bug: a server with no
/// index anywhere must report so, and must NOT create a 0-table `index.sqlite`.
#[test]
fn e2e_mcp_no_index_does_not_create_a_stub() {
    let elsewhere = tempfile::tempdir().expect("tempdir");
    let mut mcp = Mcp::start(elsewhere.path());
    mcp.initialize();

    let text = mcp.call_tool_text("reliary_search", json!({ "query": "alpha" }));
    assert!(
        text.contains("no index"),
        "an unindexed server must report no index: {}",
        text
    );
    assert!(
        !elsewhere.path().join(".reliary").exists(),
        "a read query must never create a stub index directory"
    );
    assert!(mcp.is_alive());
}

/// The CLI and the MCP server must agree on which project a query targets: a
/// project trusted at `<root>` is addressable from a subdirectory of it.
#[test]
fn e2e_mcp_walks_up_from_subdirectory() {
    let fx = Fixture::new();
    fx.trust();
    let sub = fx.path().join("src");
    let mut mcp = Mcp::start(&sub);
    mcp.initialize();

    let text = mcp.call_tool_text(
        "reliary_find_references",
        json!({ "name": "beta", "def_only": true }),
    );
    assert!(
        text.contains("other.rs"),
        "a server started in a subdirectory must find the project index above it: {}",
        text
    );
}

#[test]
fn e2e_mcp_second_initialize_is_safe() {
    // V61: a repeated initialize must not spawn a second watcher or corrupt state.
    let fx = Fixture::new();
    fx.trust();
    let mut mcp = Mcp::start(fx.path());

    let first = mcp.initialize();
    let second = mcp.initialize();
    assert_eq!(
        first["result"]["protocolVersion"], second["result"]["protocolVersion"],
        "re-initialize must agree on the protocol version"
    );
    assert!(mcp.is_alive());

    // And still serve tools afterwards.
    let text = mcp.call_tool_text("reliary_find_references", json!({ "name": "beta" }));
    assert!(text.contains("beta"), "must still answer after re-initialize");
}

// ── Handler parity ────────────────────────────────────────────────────────

/// Every entry point that accepts `def_only` must answer a trait question the
/// same way. V89's first fix reached only `reliary_find_references`, while the
/// benchmark and the harness call `reliary_find_references_with_source`, so the
/// measured answer did not change. A real two-handler comparison is the only
/// test that catches that; a source grep cannot.
#[test]
fn e2e_mcp_def_only_handlers_agree_on_trait_questions() {
    let fx = Fixture::trait_impls();
    fx.trust();
    let mut mcp = Mcp::start(fx.path());
    mcp.initialize();

    let args = json!({
        "name": "Default",
        "def_only": true,
        "path_filter": "src/",
        "path": fx.path().to_string_lossy(),
    });
    let primary = mcp.call_tool_text("reliary_find_references", args.clone());
    let with_source = mcp.call_tool_text("reliary_find_references_with_source", args);

    let first = |s: &str| s.lines().next().unwrap_or("").trim().to_string();
    assert!(
        first(&primary).contains("implemented by"),
        "the primary handler must answer with implementors: {primary}"
    );
    assert_eq!(
        first(&primary), first(&with_source),
        "both def_only handlers must answer identically\n  primary: {primary}\n  with_source: {with_source}"
    );
}

// ── Output size (V90) ──────────────────────────────────────────────────────

/// A long dead list must list every exported item and summarise the private
/// remainder. Capping the whole list was tried and reverted: with 46 dead items
/// of which 31 are exported, a flat cap of 12 pushed the exported functions the
/// question asks about below the cut, and recall on the dead-code question went
/// to 0. A private helper nobody calls is usually deliberate; an exported one
/// usually is not.
#[test]
fn e2e_mcp_dead_code_lists_exported_and_summarises_private() {
    let fx = Fixture::mixed_dead_functions(6, 18);
    fx.trust();
    let mut mcp = Mcp::start(fx.path());
    mcp.initialize();

    let text = mcp.call_tool_text("reliary_find_dead_code", json!({
        "path": "src/",
        "limit": 40,
        "path_hint": fx.path().to_string_lossy(),
    }));

    let listed: Vec<&str> = text
        .lines()
        .filter(|l| l.contains(" at ") && l.contains(".rs:"))
        .collect();
    // Every exported dead function must appear by name.
    for i in 0..6 {
        let name = format!("pub_unused_{i}");
        assert!(
            listed.iter().any(|l| l.starts_with(&name)),
            "exported `{name}` must be listed:\n{text}"
        );
    }
    // Private helpers are counted, not enumerated one per line.
    assert!(
        text.contains("private helper(s) with no callers"),
        "the private remainder must be summarised:\n{text}"
    );
    // Nothing is hidden: the header states the true total.
    assert!(text.contains("19 dead items"), "the true total must be stated:\n{text}");
}
