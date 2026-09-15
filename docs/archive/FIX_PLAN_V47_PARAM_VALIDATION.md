# V47 — Pre-Prompt Parameter Validation (Break the Dead-End Retry Loop)

## Root Cause

The model enters a dead-end retry loop when it generates a wrong parameter value:

1. Model generates `name="reliaary_find_references"` (typo) — confident
2. Tool says "not found" (reactive)
3. Model generates `name="reliary_find_references"` (corrected) — still confident
4. Tool says "no definition found" — model's confidence drops
5. Model generates `name="find_references"` (different concept) — confused
6. Dead-end accumulates: 14 per session (V42 stable)

**All previous fixes (V43-V46) were reactive** — they changed the tool's "not found" message format. The model ignored all of them.

## The Fix — Proactive Validation at the MCP Server Level

**Validate the parameter BEFORE running the tool.** If the parameter doesn't exist in the index, return a constraint + suggestions. The model never executes a tool call with an invalid param.

```
Model: find_references(name="reliaary_find_references")
Server: "Parameter 'reliaary_find_references' not found. Did you mean: reliary_find_references? (3 similar symbols exist. Use one of them.)"
Model: find_references(name="reliary_find_references")  ← forced to use a valid param
Server: (runs the tool, returns results)
```

**This breaks the retry loop because the model CANNOT execute the tool with an invalid param.** The MCP server rejects the call before it reaches the tool implementation.

## The Plan — 5 Phases

### Phase 1: Parameter validation in MCP server (1 hour)

Add a `validate_params` function to mcp.rs that runs BEFORE the tool dispatch:

```rust
// In the tool dispatch loop, before matching tool name:
let validation = validate_params(tool_name, args, &db);
match validation {
    Ok(()) => { /* proceed to tool dispatch */ }
    Err(hint) => {
        return DispatchResult::Success(serde_json::json!({
            "content": [{ "type": "text", "text": format!("// @rel param error: {}\n", hint) }]
        }));
    }
}
```

The `validate_params` function checks:
- `name` exists in phrases table
- `anchor_file` exists in file_map
- `path` exists and is indexed
- `path_filter` matches some indexed files
- `methods` and `dead_only` don't require `name` (already fixed in V45)

### Phase 2: Suggestion engine — "Did you mean?" (1 hour)

When validation fails, return top-3 closest valid params from the index:

```rust
fn suggest_closest(query: &str, candidates: &[String]) -> Vec<String> {
    // String similarity (Levenshtein or simple prefix/substring matching)
    // Return top 3 closest matches
}
```

**Grammar-free**: pure string matching, no AST, no language detection. Works on any repo, any language, any symbol naming convention.

### Phase 3: Dead-end tracking (30 min)

The MCP server counts consecutive failed validations per session. After 3 consecutive failures on the same tool/param, return a HARD STOP message:

```
// @rel hard stop: 3 consecutive failures on find_references(name="..."). 
// Move on. Use describe(name=...) or search(query=...) for different angles.
```

This prevents infinite retry loops even when the model can't find a valid param.

### Phase 4: System prompt update (15 min)

Update RELIARY_SYS to:
1. Acknowledge that param errors will be returned immediately
2. Instruct the model to try the suggested param on the next call
3. Explain that hard stops exist after 3 consecutive failures

```
V47 PARAM VALIDATION:
- Invalid params are rejected before the tool runs. The response lists similar valid params.
- On the next call, use one of the suggested params — don't try to "fix" the original.
- After 3 consecutive failures, move on. Use a different tool.
```

### Phase 5: Run 3-way bench with V47 (2 hours)

Run the same 3-seed 3-way comparison as V42 baseline. Compare:
- **Score** — should be same or better (model gets valid params faster)
- **Dead-ends** — should DROP (model can't retry with invalid params)
- **Billed cost** — should DROP (fewer tool calls when params are correct first time)
- **Tool calls** — should DROP (fewer dead-end retries)

## Expected Impact

| Metric | V42 | V47 target | Why |
|--------|-----|-----------|-----|
| **Score** | 22.0 | **23-25** | Model gets valid params, better answers |
| **Dead-ends** | 14 | **3-5** | Invalid params rejected at MCP level, not at tool level |
| **Billed cost** | 48k | **35-40k** | Fewer dead-end retries = fewer tool calls |
| **Tool calls** | 50 | **35-40** | Dead-end retries eliminated |
| **Wall time** | 75s | **55-65s** | Fewer turns needed |

## Grammar-Free Verification

| Component | Grammar-free? | Why |
|-----------|--------------|-----|
| `validate_params` | Yes | Queries existing index tables |
| `suggest_closest` | Yes | String similarity, no language detection |
| Dead-end tracking | Yes | Integer counter per session |
| System prompt | N/A | Documentation, not code |

## Is This Fitting?

| Aspect | Fitting? | Why |
|--------|---------|-----|
| Pre-prompt validation | **No** | Universal — any tool that takes params benefits from validation |
| Suggestion engine | **No** | Google does "did you mean?" for ALL search queries |
| Dead-end hard stop | **No** | Universal — any interactive tool needs infinite-loop prevention |
| System prompt update | **No** | Documentation for a new feature |

None of these are tokio-specific, Python-specific, or benchmark-specific. They benefit any user of any tool on any repo.

## The Key Insight (Meta)

**Every previous fix (V43-V46) tried to change the tool's output format.** V47 doesn't change any tool output. It rejects the tool call BEFORE it runs. The model never sees "not found" — it sees "param invalid, try one of these." The retry loop is broken at the input layer, not the output layer.

**This is the first fix that operates at the MCP server level, not the tool implementation level.** It intercepts the model's behavior, not the tool's response.

## Why V43-V46 Failed (and V47 Won't)

| Version | What the model saw | What the model did |
|---------|-------------------|-------------------|
| V43 | "No matches. Try search." | Tried search (also failed) |
| V44 | "Did you mean X?" | Ignored, tried original again |
| V45 | "Closest files: A, B, C" | Ignored, tried different param |
| V46 | "/// @rel no definition" | Ignored, tried different param |
| **V47** | **"// @rel param error: param doesn't exist. Valid: [list]."** | **Must use valid param to proceed** |

V47's response is not a "hint" — it's a **gate**. The tool doesn't run unless the param is valid. The model has no choice but to use a valid param.

## Risks

1. **Model might loop on suggestion-acceptance** — "Did you mean X?" → use X → "Did you mean Y?" → use Y → infinite loop. Mitigation: dead-end tracking caps at 3 consecutive failures.
2. **Validation might be too strict** — some params are legitimately user-defined (path names, arbitrary strings). Mitigation: only validate params that should exist in the index (`name`, `anchor_file`, `path_filter`).
3. **Suggestions might be wrong** — "Did you mean X?" when X is also wrong. Mitigation: show top 3 candidates, model can pick.

## Effort

| Phase | Time |
|-------|------|
| 1. Parameter validation | 1h |
| 2. Suggestion engine | 1h |
| 3. Dead-end tracking | 30m |
| 4. System prompt | 15m |
| 5. Bench + score | 2h |
| **Total** | **~5 hours** |
