# reliary-sift

Grammar-free bash-output compression engine. Powers `reliary wrap` and `reliary sift`.

## How it works

1. **Classify** every line (`classify.rs`): skeleton shape, error/progress/summary detection.
2. **Detect strategy** from the first 20 lines (`detect_strategy`): JSON / Diff / Tabular / Prefixed (grep-like) / Normal.
3. **Format** per strategy (`filter.rs`): prefix-aware grouping, tabular column pruning, hunk-header preservation, OK-line collapse.
4. **Entropy guard** (MaxwellGate): if the output is information-dense, don't force compression.

Pure byte-level structural analysis — no keyword lists, no per-command rules.

## Determinism and cache safety

Same input bytes always produce the same output bytes (BTreeMap ordering, first-appearance freeze). Compression happens before tool output enters the conversation, so provider-side KV caching is never invalidated.

## The `wrap` path (`reliary wrap <cmd>`)

- Command executes, exit code propagates on both success and failure.
- Raw stdout is stored in `.reliary/cache.sqlite`; the output footer carries the retrieval hash (`reliary cache-retrieve <hash>`).
- Test runners collapse: passing suites print `[reliary: N tests passed]`.
- **Edit safety**: content readers on source-like files (`cat`/`head`/`tail`/`less`/`bat <source.rs>`) pass through uncompressed — the model never builds edits from mangled code.
- Flags: `--llm` (plain markers), `--aggressive` (drop last 30% of kept lines).

## Benchmarks

46.3% average across the 6 fixtures in the V14 benchmark. Suite and raw fixtures: `~/src/sift/scripts/bench_vs_rtk.py`.

Part of the [reliary-agent](https://github.com/Reliary/reliary-agent) workspace. See the main repository for full documentation.
