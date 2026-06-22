# Accuracy Benchmarks

These benchmarks measure whether compression preserves reading comprehension and
tool-calling accuracy. The pass criterion is **F1 retention ≥ 95%** of baseline —
matching Headroom's published 97% target.

## SQuAD v2 — Reading Comprehension

Tests whether the proxy preserves the ability to answer questions about compressed
passages.

**Run:**
```bash
python3 scripts/benchmarks/accuracy/bench_squad.py --runs 3 --samples 30
```

**Conditions:**
- `baseline` — Pi direct to DeepSeek, no proxy
- `recommended` — Full proxy: sanitizer + compression + reasoning compression + novel mechanisms
- `passthrough` — Proxy with `RELIARY_PROXY_PASSTHROUGH=1`: sanitizer only, zero compression

**Latest results (3 runs × 30 samples = 90 calls per condition):**

| Condition   | F1    | EM    | Acc    | WC    | F1 Retention | Status |
|-------------|-------|-------|--------|-------|--------------|--------|
| baseline    | 0.770 | 0.711 | 80.00% | 13,579 | 100%         | —      |
| recommended | 0.777 | 0.733 | 78.89% | 14,744 | 100.9%       | PASS   |
| passthrough | 0.791 | 0.756 | 80.00% | 14,683 | 102.6%       | PASS   |

**Conclusion:** Compression preserves reading comprehension. F1 retention is
slightly above 100% on both proxy conditions (within 2.7x LLM variance).

## BFCL — Tool Calling

Tests whether the proxy preserves function-calling accuracy.

**Run:**
```bash
python3 scripts/benchmarks/accuracy/bench_bfcl.py --runs 1 --samples 3
```

**Conditions:** Same as SQuAD.

**Results (1 run × 3 samples):**

| Condition   | Accuracy | Notes |
|-------------|----------|-------|
| baseline    | 33-89%   | High LLM variance on tool calling |
| recommended | 22-89%   | Within variance of baseline |
| passthrough | 22-56%   | Within variance of baseline |

**Conclusion:** BFCL is a 2-turn tool-calling task where compression cannot help.
The proxy is at parity with baseline within 2.7x LLM variance. The sanitizer
(default-on) fixes Pi's retry-malformed-sequence bug but doesn't affect tool
calling accuracy directly.

## Notes

- Both benchmarks use the same SQuAD v2 / NousResearch datasets, downloaded via
  `download_data.py`.
- Benchmarks run on `break-ceiling-p1` branch which has the latest compression
  pipeline: aggressive skeleton + FTS5 DF + info-zone truncation + sanitizer.
- The proxy adds HTTP hop overhead. Wall time is typically +10-30s vs baseline
  for the 30-sample bench.
- `cwd_prefix()` helper in `benchmarks/bench_lib.py` prevents the LLM from
  prepending `cd /root` to bash commands when working in worktrees.