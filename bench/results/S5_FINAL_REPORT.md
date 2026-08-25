# S5 End-to-End Report: Holographic Pack Regen via OpenCode

**Test**: `bench/test_s5_end_to_end.py`
**Date**: 2026-07-09
**Result**: ALL 4 STEPS PASSED

## What Was Tested

The S5 hypothesis: after a file edit in the opencode build agent, does the holographic pack stay in sync? Can the model retrieve the new entry via `reliary_pack_query`?

Steps verified:

| # | Action | Result |
|---|---|---|
| 1 | opencode agent uses `write` tool to create `test_s5_e2e.rs` | ✓ file created (44 bytes, exact content) |
| 2 | `reliary reindex-file` rebuilds occurrence rows | ✓ 3 tokens reindexed |
| 3 | `reliary pack` regenerates `.reliary/pack_l2l3.md` | ✓ cache contains `s5_test_function_e2e` (196 chars total) |
| 4 | Fresh opencode session queries via MCP `reliary_pack_query` | ✓ entry returned with file/line/signature |

## The Real MCP Fix (Discovered Mid-S5)

While testing step 4, `reliary_reliary_pack_query` returned `unknown tool: -32601`.

The root cause was **NOT** gate.js wiring (the earlier diagnosis was wrong).

The actual cause was a Rust match arm ordering bug in `crates/reliary-agent/src/mcp.rs`:

```rust
match name {
    "reliary_search" => ...,
    // ... many other arms ...
    _ => DispatchResult::Error(-32601, format!("unknown tool: {}", name)),  // line 456
    "reliary_pack" => { ... }                                                   // line 994 — UNREACHABLE
    "reliary_pack_query" => { ... }                                             // line 1009 — UNREACHABLE
}
```

Rust evaluates match arms top-to-bottom; the `_ =>` catch-all at line 456 was hit first and returned "unknown tool" for every name. The arms for `reliary_pack` (994) and `reliary_pack_query` (1009) were dead code — the compiler warned `unreachable_pattern` but the binary shipped.

**Fix**: moved both match arms above line 456, removed the unreachable duplicates, added a size threshold (`>1KB`) for the cached pack file so empty/stale packs trigger auto-regeneration.

## End-to-End Verification (opencode build agent, v4-flash, real MCP)

```json
{"type":"tool_use","tool":"reliary_reliary_pack_query",
 "input":{"name":"s5_test_function_e2e","path":"$HOME/src/reliary8"},
 "status":"completed",
 "output":"## s5_test_function_e2e/reliary-sift\nL2: pub fn s5_test_function_e2e() -> i32 { 42 }  [$HOME/src/reliary8/crates/reliary-sift/src/test_s5_e2e.rs:0]"}

Subsequent turn answer:
"The symbol is `s5_test_function_e2e`, located at
 $HOME/src/reliary8/crates/reliary-sift/src/test_s5_e2e.rs:0
 The signature is `pub fn s5_test_function_e2e() -> i32 { 42 }`. It's a trivial
 test stub returning 42. No callers, no surprise facts."
```

The model correctly identified the new function's file, line, signature, and behavior — all from a single `pack_query` call. The auto-regeneration path works end-to-end through the real opencode flow.

## What's NOT Automatic

The S5 regen chain requires **explicit invocation**:

```bash
# After writing a file:
reliary reindex-file path/to/file.rs    # populate occurrence
reliary pack . --format l2l3 > pack.md  # regenerate to stdout
```

To make this fully automatic on every opencode `write`/`edit` tool call, the `@reliary/opencode` plugin needs a `tool.execute.after` hook at `$HOME/src/autopsylab-agent/packages/opencode/src/plugin.ts`:

```typescript
'tool.execute.after': async (input, output) => {
  if (input.tool === 'write' || input.tool === 'edit') {
    const filePath = input.args?.file;
    if (filePath && /\.(rs|py|ts|tsx|go|c|h|cpp|hpp|js|jsx|java|rb|swift)$/.test(filePath)) {
      execSync(`${RELIARY_BIN} reindex-file ${filePath}`, ...);
      // Bundle with pack_query calls so the cache stays warm.
    }
  }
},
```

That hook isn't in this repo — it's in `autopsylab-agent`. The S5 test works **manually**: the test harness calls `reliary reindex-file` and `reliary pack` between steps.

## Files Changed

| File | Purpose |
|---|---|
| `crates/reliary-agent/src/mcp.rs` | Moved `reliary_pack` and `reliary_pack_query` match arms above `_ =>` catch-all. Added size threshold (>1KB) for cached pack file to trigger auto-regen when missing/stale. |
| `bench/test_s5_end_to_end.py` | Reproducible end-to-end test simulating the S5 flow. |

## Honest Limitations

1. **No opencode plugin hook yet**: the regen-on-edit automation isn't in place. The test does it manually. Adding the `tool.execute.after` hook to `autopsylab-agent/packages/opencode/src/plugin.ts` is the next step (requires rebuild + reinstall of the npm package).

2. **Path scoping**: `reliary_pack_query` enforces `safe_path()` — paths outside the opencode workspace root are rejected. We had to run the test from `$HOME/src/reliary8` (not `llm-semantic-transport`) for the MCP path check to succeed.

3. **Pack content is minimal** for trivial functions. `s5_test_function_e2e` has only an L2 line (signature/lineage) — no L3 surprise facts because the body is too short. The "No surprise facts" answer from the model is correct.

4. **Occurrence table rebuilding**: the second `reindex-file` after deletion succeeded because the index table had a stale entry from the first reindex that's now stale; the reindex processes the deletion. There's a small window where concurrent edits could race.

## Bottom Line

The holographic pack + `pack_query` MCP tool is **verified end-to-end** through the real opencode agent flow. The architecture works. The only remaining gap is the automation hook in the opencode plugin.

**Commits on `optimize-efficiency`**:
```
817a6370 test(s5): end-to-end regeneration via opencode + pack_query
c0d11105 fix(s1): 20% pack size reduction — remove L3 noise from common patterns
7a51b7df feat(s3): warmup integration into persistent session test
ed7fe4e3 feat(s2): reliary_pack_query MCP tool — query pack by symbol name
688e177f drop(S4): 50-turn stress test was test-fitting
3345c861 fix(s2): pack_query unreachable due to _ => catch-all at line 456
```

Branch is ahead of `main` by 7 commits. Ready for the automation hook or wrap-up.
