# FIX PLAN V14 — RTK Parity via Cache-Safe Auto-Rewrite

**Status:** Approved, building. Branch: `feat/sift-bash-auto-rewrite` off `5972fff0`.

**Goal:** Make `reliary wrap` competitive with RTK's 60-90% bash output compression without
breaking provider KV cache. All work is grammar-free.

**Cache safety principle:** Identical command output → identical compressed bytes on every
call. Verified via `bench/cache_safety.py` BEFORE shipping auto-rewrite.

## PR 1 — Determinism audit + cache-safety tests (Day 1)

**Files:** `crates/reliary-sift/src/*.rs`, `bench/cache_safety.py` (new)

### Tasks
1. Audit `compress_unified` pipeline for non-determinism:
   - `find_clusters_global` uses `HashMap` — sort groups before output
   - `skeleton_groups` uses `HashMap` — sort by skeleton_key before output
   - `aggressive_skeleton_groups` — same
   - `compress_tabular` — check for any random sampling
   - `extract_error_blocks` — already deterministic (linear pass)
2. Make `compress_unified` output byte-identical for identical input:
   - Add `BTreeMap<u64, Vec<usize>>` instead of `HashMap` where order matters
   - Ensure all iteration is sorted by some stable key
   - Strip timestamps from output (`Finished ` … `in 1.23s` is OK to keep, but `14:32:15.123` should not appear in compressed output if it appears in raw)
3. Add `bench/cache_safety.py`:
   - Run identical commands twice, assert byte-identical output
   - Run with simulated non-determinism (timestamps), verify same input hashes to same output
   - Add unit tests `crates/reliary-sift/tests/determinism.rs`

### Acceptance criteria
- `cargo test -p reliary-sift` passes including determinism tests
- `bench/cache_safety.py` shows 100% byte-identical output across N=5 runs of identical commands
- No regression in `bench/long_session_bench.py` (sift OFF remains current behavior)

### Risk
LOW. Determinism audit might expose existing non-determinism that needs fixing (likely no-impact
since current usage is single-shot).

## PR 2 — Lower 200-char threshold + tune MaxwellGate (Day 1)

**Files:** `crates/reliary-output/src/unified.rs`, `crates/reliary-sift/src/lib.rs`

### Tasks
1. Lower `compress_unified`'s short-circuit: `text.len() < 200` → `text.len() < 50`
   - Many tool outputs are 50-200 chars and worth compressing
2. Tune MaxwellGate defaults:
   - `entropy_threshold: 3.5` → `3.0` (allow more structured output through)
   - `compression_ratio_max: 3.0` → `5.0` (allow more repetitive output to be considered compressible)
   - `diversity_min: 0.25` → `0.20` (relax lexical diversity requirement)
3. Add `--always-sift` flag for `reliary wrap` that bypasses MaxwellGate
4. Add `RELIARY_SIFT_AGGRESSIVE=1` env var that uses aggressive thresholds

### Acceptance criteria
- All existing tests pass
- New tests: `compress_unified` operates on 50-200 char inputs without early-return
- New tests: `compress_unified` with `aggressive=true` produces more compact output on
  repetitive inputs (cargo output, test summaries)

### Risk
LOW. Tunable defaults, can be rolled back via env var.

## PR 3 — Auto-rewrite hook (Day 2-3)

**Files:** `hooks/claude-pretooluse.sh`, `hooks/opencode-reliary-sift.js`, `crates/reliary-agent/src/main.rs`

### Tasks
1. Update `hooks/claude-pretooluse.sh`:
   - When `RELIARY_SIFT_BASH=1`, rewrite the following commands:
     - `git status|diff|log|show|add|commit|push|pull|fetch|stash|branch` → `reliary git <subcmd>`
     - `cargo test|build|check|run|clippy|fmt` → `reliary cargo <subcmd>`
     - `pytest|jest|vitest|go test|rake test` → `reliary test <full-cmd>`
     - `ls|find|tree` → `reliary ls|find <args>`
     - `grep|rg|ag` → `reliary grep <pattern>`
     - `cat|head|tail|less|more` → `reliary read <args>` (uses `sift_file_read`)
     - `diff` → `reliary diff <args>`
   - Skip rewrite if command contains `|`, `>`, `<`, `&&`, `;` (shell chaining breaks re-execution)
   - Skip rewrite if user added `--no-sift` flag
   - Print `[reliary: rewrote X → Y]` to stderr (not visible to LLM)
2. Update `hooks/opencode-reliary-sift.js` with the same rewrite logic
3. Add `reliary_agent_sift_bash()` to `main.rs` that detects program + args, routes to
   `sift_file_read` / `sift_test_output` / `compress_unified` (already partially exists
   as `exec_sift` at line 971)
4. Wire `RELIARY_SIFT_BASH=1` env var in `main.rs:exec_sift` (currently always-on when
   `sift` subcommand is invoked, but bash auto-rewrite is opt-in)

### Acceptance criteria
- Running `claude --no-sift git status` → no rewrite (user override)
- Running `claude git status` with `RELIARY_SIFT_BASH=1` → `reliary git status` runs,
  LLM sees compressed output
- Running `claude git status && cargo test` → no rewrite (shell chain detected)
- Existing tests pass

### Risk
MEDIUM. Hook must be 100% correct or it breaks user workflows. Mitigation:
- Opt-in via env var (default OFF)
- `--no-sift` override at command level
- Detect shell chaining and skip rewrite
- Log to stderr for debugging

## PR 4 — Real-world benchmark corpus (Day 3-4)

**Files:** `bench/sift_corpus/*.txt` (new), `bench/sift_bench.py` (new)

### Tasks
1. Collect 10-20 real bash output samples:
   - `cargo test` (passing, failing, mixed)
   - `cargo build` (full output)
   - `cargo clippy` (warnings)
   - `git diff` (small, large)
   - `git status` (clean, modified)
   - `git log -n 20`
   - `pytest -v`
   - `ls -la` (small dir, large dir)
   - `grep -rn 'pattern'` (sparse, dense matches)
   - `docker ps`, `docker logs`
   - `kubectl get pods`
2. Create `bench/sift_bench.py`:
   - For each fixture: measure original length, compressed length, compression ratio
   - Run `compress_unified` on each fixture, assert ratio ≥ 60% (RTK baseline)
   - Print table: fixture name, original, compressed, ratio
3. Add `bench/sift_corpus/cargo_test_pass.txt` etc. — captured from real runs
4. CI integration: `bench/sift_bench.py` runs in PR checks

### Acceptance criteria
- Average compression ratio ≥ 60% across corpus (RTK range)
- No fixture has ratio < 40% (worst-case floor)
- Compression is deterministic across runs (byte-identical for identical input)

### Risk
LOW. Pure measurement + corpus collection.

## PR 5 — New sift features for hard cases (Day 4-5)

**Files:** `crates/reliary-sift/src/classify.rs`, `crates/reliary-sift/src/filter.rs`,
`crates/reliary-output/src/unified.rs`

### Tasks
1. **Section header preservation**: detect lines like `branch:`, `Changes not staged`,
   `Untracked files:` — keep these, drop surrounding separators
2. **Numeric line collapse**: consecutive `line 12`, `line 12`, `line 12` →
   `[3× line 12]`
3. **Pass-only-test collapse in arbitrary bash output**: detect `test_X ... ok` runs
   outside `sift_test_output` and collapse
4. **Tail-truncation with summary preservation**: if output > N lines, keep first 10 +
   last 30 + summary markers (errors, test results). Stable across runs.
5. **Multi-error block merging**: compiler errors span 3-5 lines, merge to 1 with `┃`
   separator. Already in `extract_error_blocks` (line 8) but currently DISABLED in LLM mode
   (line 126). Add a third mode: `merge_errors=true` that preserves the `┃` merge even
   for LLM (since the multi-line merge is deterministic and byte-identical).
6. **Optional `RELIARY_SIFT_AGGRESSIVE` mode**: combine all of the above for max compression

### Acceptance criteria
- New tests for each feature
- Re-run `bench/sift_bench.py` — expect avg ratio to improve by 5-10 percentage points
- All existing tests pass
- No regression in `bench/long_session_bench.py` (sift OFF remains current behavior)

### Risk
LOW-MEDIUM. Features are additive and can be disabled individually.

## Bench validation plan

After all PRs merged:
1. Run `bench/sift_bench.py` on full corpus — expect avg ratio ≥ 70%
2. Run `bench/cache_safety.py` — expect 100% byte-identical
3. Run `bench/long_session_bench.py --conditions A` with sift OFF (current best: 25.5/30)
4. Run with `RELIARY_SIFT_BASH=1` and sift auto-rewrite — expect equal or better score,
   lower WC
5. If regression: investigate before merging

## Rollback plan

All work on branch `feat/sift-bash-auto-rewrite`. No changes to main until merged.
Auto-rewrite is opt-in (`RELIARY_SIFT_BASH=1`, default OFF). MCP tool sift remains opt-in
(`RELIARY_SIFT_TOOLS=1`, default OFF). Existing behavior preserved if env vars unset.

## Grammar-free check

All five PRs use ONLY:
- Line text patterns (substring, prefix, suffix)
- Skeleton hashing (byte DFA)
- Counters (line counts, group counts)
- No tree-sitter, no AST, no language detection
- No new keyword lists beyond what `is_error_line` already has

Confirmed grammar-free by code review.