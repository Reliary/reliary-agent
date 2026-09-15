# FIX PLAN V20 — All Synergies + Pure Math Wins

**Date:** 2026-07-13
**Scope:** Cross-cutting synergies (24) + pure-math wins (40+) across 8 crates
**Total items:** 60 (excluding 4 deferred)
**Estimated effort:** ~20 hours

---

## Phase 1: Cross-Cutting Quick Wins (~3 hours)

### S1: Fix Pack's tokenize grammar bug (CRITICAL, 15 min)
**File:** `crates/reliary-pack/src/lib.rs:570-575`
**Issue:** Pack re-implements `tokenize` using `char::is_alphanumeric()` (Unicode-aware) instead of `is_ascii_alphanumeric()` (ASCII-only). This causes CJK/Unicode identifiers to fuse with adjacent ASCII tokens, diverging BM25 from the main search index.
**Fix:** Delete `pack::tokenize`, use `reliary_search::tokenize` from `crates/reliary-search/src/lib.rs:162`.
**Impact:** Correctness fix — pack BM25 now matches search BM25.

### S2: Pack reuses file_meta cache (HIGH, 1 hr)
**Files:** `crates/reliary-pack/src/lib.rs:948, 2218, 2260`
**Issue:** Pack has 3 per-call file caches (`lines_cache`, `file_cache`, `file_cache`) that re-read files from disk + re-split into `Vec<String>`. `file_meta::get(path)` already has a bounded LRU cache with the same data.
**Fix:** Add `reliary-search` dependency to pack (already present). Replace `std::fs::read_to_string(path).map(|c| c.lines().map(String::from).collect())` with `reliary_search::file_meta::get(path).map(|m| m.lines.clone())`. Fall back to disk read only on cache miss.
**Impact:** ~700 fewer disk reads per pack generation on large repos.

### S3: select_nth_unstable_by for search top-N (CRITICAL, 30 min)
**File:** `crates/reliary-search/src/search.rs:177`
**Issue:** Full O(N log N) sort then truncate to top-N. N can be 50K candidates.
**Fix:**
```rust
let k = top_n.min(results.len());
if k > 0 {
    results.select_nth_unstable_by(k.saturating_sub(1), |a, b|
        b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal));
}
results.truncate(k);
```
**Impact:** O(N) vs O(N log N) per search query.

### S4: Pre-load all phrase_ids in ensure_occurrence_for_file (HIGH, 1 hr)
**File:** `crates/reliary-search/src/lazy_occurrence.rs:464-475`
**Issue:** Per-token `INSERT OR IGNORE INTO phrases` + `SELECT id` = 2 SQL roundtrips per token per file. For 5K unique phrases, 10K SQL calls.
**Fix:** At start of `ensure_occurrence_for_file`, pre-load `SELECT id, phrase FROM phrases` into `FxHashMap<String, i64>`. Use for lookups. Batch-insert new phrases at end.
**Impact:** Eliminates ~10K SQL calls per file during JIT build.

### S5: Deduplicate has_top_level_eq and find_top_level_eq_pos (HIGH, 30 min)
**File:** `crates/reliary-search/src/type_flow.rs:213-262, 629-649`
**Issue:** Two functions with identical body logic.
**Fix:** Delete `has_top_level_eq`, replace all call sites with `find_top_level_eq_pos(s).is_some()`.
**Impact:** 100+ LOC deleted, single source of truth.

### S6: Shared is_indexable_dir const (MEDIUM, 30 min)
**Files:** `crates/reliary-search/src/ingest.rs:173-186`, `crates/reliary-pack/src/lib.rs:1090-1115`
**Issue:** Directory skip list duplicated between ingest (walkdir filter) and pack (is_source_file).
**Fix:** Add `pub const SKIP_DIRS: &[&str] = ["target", "node_modules", ...]` to `reliary-search::ingest`. Both crates import it.
**Impact:** DRY, prevents drift.

### S7: Shared BM25 from reliary-search in pack (MEDIUM, 15 min)
**File:** `crates/reliary-pack/src/lib.rs:679-705`
**Issue:** Pack re-implements BM25 inline with its own k1/b constants.
**Fix:** Import `reliary_search::bm25_score` and `reliary_search::bm25_idf` in pack. Use the canonical functions.
**Impact:** Drift prevention.

### S8: Porter stem returns Cow (MEDIUM, 1 hr)
**File:** `crates/reliary-search/src/lib.rs:118-132`
**Issue:** `porter_stem` always allocates a new String even when the stem is identical to the input (no suffix matched).
**Fix:** Return `Cow<'_, str>` — return `Cow::Borrowed(input)` when no suffix matched, `Cow::Owned(stemmed)` otherwise.
**Impact:** Eliminates allocation for ~60% of tokens (those with no matching suffix).

### S9: scan_identifiers streaming iterator (HIGH, 2 hr)
**File:** `crates/reliary-search/src/lib.rs:101-114`
**Issue:** Returns `Vec<String>` — allocates per identifier per line. Called in JIT hot path (lazy_occurrence.rs:464).
**Fix:** Add `pub fn scan_identifiers_iter(text: &str) -> impl Iterator<Item = &str>` that yields borrowed slices. Callers that need owned Strings can `.to_string()` lazily.
**Impact:** Eliminates per-line Vec<String> allocation in JIT indexing.

---

## Phase 2: Pure Math in Search + Structural (~4 hours)

### M1: search.rs — select_nth_unstable_by (CRITICAL, already S3)
Covered by S3 above.

### M2: search.rs — Arc<str> for file_path in SearchResult (HIGH, 30 min)
**File:** `crates/reliary-search/src/search.rs:153-174`
**Issue:** `file_path.clone()` allocates per occurrence. Same path appears multiple times.
**Fix:** Store `Arc<str>` in SearchResult. Clone is refcount bump, not string copy.
**Impact:** Eliminates O(occurrences) String allocations per search.

### M3: structural.rs — Lazy scan_delimiters (HIGH, 1 hr)
**File:** `crates/reliary-search/src/structural.rs:75, 113-153`
**Issue:** `scan_delimiters` runs unconditionally before the `if !is_block_start` early return. ~60% of lines are non-block-start.
**Fix:** Move the cheap prefix checks (comments, control-flow keywords) BEFORE `scan_delimiters`. Only call scan when we need the delimiter positions for function-signature classification.
**Impact:** Eliminates 60% of O(n) scans during indexing.

### M4: structural.rs — Forward-tracked last_non_ws (MEDIUM, 30 min)
**File:** `crates/reliary-search/src/structural.rs:445-454`
**Issue:** `>` handler walks backward to find `prev_non_ws` — O(n) per `>` byte, quadratic worst case.
**Fix:** Track `last_non_ws: u8` as loop state. Update whenever a non-whitespace byte is encountered.
**Impact:** Eliminates backward scan.

### M5: structural.rs — scan_last_identifier via rposition (LOW, 15 min)
**File:** `crates/reliary-search/src/structural.rs:523-563`
**Issue:** Manual backward while loop.
**Fix:** Use `bytes.iter().rposition(|&b| b.is_ascii_alphanumeric() || b == b'_')` (auto-vectorized).
**Impact:** SIMD-accelerated identifier boundary detection.

### M6: type_flow.rs — memchr-based contains_method_call (HIGH, 1 hr)
**File:** `crates/reliary-search/src/type_flow.rs:265-282`
**Issue:** Byte-by-byte scan for `.` + identifier + whitespace + `(`.
**Fix:** Early-exit: `if !bytes.contains(&b'.') || !bytes.contains(&b'(') { return false; }`. Use `memchr::memchr(b'.', bytes)` to find candidates.
**Impact:** Early-exit for 50%+ of lines that don't contain `.`.

### M7: type_flow.rs — memchr-based stem search (MEDIUM, 30 min)
**File:** `crates/reliary-search/src/type_flow.rs:123-136`
**Issue:** Per-byte slice comparison to find stem in line.
**Fix:** Use `memchr::memmem::find(line_bytes, stem_bytes)` to find candidates, verify word boundaries only at hit positions.
**Impact:** 5× faster stem search on long lines.

### M8: type_flow.rs — Pre-compute lines with `=` per file (HIGH, 1 hr)
**File:** `crates/reliary-search/src/type_flow.rs:485-625`
**Issue:** `resolve_let_binding_type` walks 500 lines calling `has_top_level_eq` per line.
**Fix:** Pre-compute `Vec<usize>` of line indices containing `=` outside strings. Skip lines not in this set.
**Impact:** Eliminates 500 per-line scans per candidate.

### M9: brace_graph.rs — Walk bytes not chars (HIGH, 30 min)
**File:** `crates/reliary-search/src/brace_graph.rs:115-140`
**Issue:** `for c in line.chars()` allocates UTF-8 decode state per char. Only checks `{` and `}`.
**Fix:** `for &b in line.as_bytes() { match b { b'{' => ..., b'}' => ..., _ => {} } }`.
**Impact:** 2-3× faster on non-ASCII content.

### M10: brace_graph.rs — parent_idx field (HIGH, 1 hr)
**File:** `crates/reliary-search/src/brace_graph.rs:85-99, 159-172`
**Issue:** `find_parent` is O(N) recursive tree search per call. `collect_enclosing_chain` calls it per ancestor. `shared_enclosing` does O(N²) over both chains.
**Fix:** Add `parent_idx: usize` to `BraceNode`. Build during `build_brace_graph`. `find_parent` becomes O(1) lookup.
**Impact:** Eliminates O(N²) tree walks.

### M11: brace_graph.rs — Option<BraceNode> instead of mem::replace (MEDIUM, 30 min)
**File:** `crates/reliary-search/src/brace_graph.rs:131-135`
**Issue:** `mem::replace` with empty placeholder allocates a useless String.
**Fix:** Make `all_nodes: Vec<Option<BraceNode>>`. After moving node out, set `all_nodes[idx] = None`.
**Impact:** Eliminates placeholder allocation.

### M12: brace_graph.rs — collect_method_calls direct children only (MEDIUM, 15 min)
**File:** `crates/reliary-search/src/brace_graph.rs:85-99`
**Issue:** Doc says "direct children only" but implementation recurses through all descendants.
**Fix:** Loop over `self.children` only — don't recurse into grandchildren.
**Impact:** Correctness fix + faster.

---

## Phase 3: Pure Math in Lazy Occurrence + Indexing (~3 hours)

### L1: Single-pass (tag, is_def, brace_delta) per line (CRITICAL, 2 hr)
**File:** `crates/reliary-search/src/lazy_occurrence.rs:251-266`
**Issue:** 3× O(n) walks per line: `count_unmatched(b'{')`, `count_unmatched(b'}')`, then `classify_structural`.
**Fix:** Single function `classify_line_meta(line) -> (u8 /*tag*/, bool /*is_def*/, i32 /*brace_delta*/)` that does one pass tracking string/comment state + brace depth + structural classification.
**Impact:** 3× faster per-line indexing. The dominant cost of `build_all_occurrence`.

### L2: Batch phrase INSERT with VALUES (HIGH, 1 hr)
**File:** `crates/reliary-search/src/lazy_occurrence.rs:464-475`
**Issue:** Per-token `INSERT OR IGNORE INTO phrases` + `SELECT id`.
**Fix:** Collect new phrases. At end of file, batch-insert with multi-row VALUES. Then batch-SELECT ids.
**Impact:** 10× fewer SQL roundtrips.

### L3: has_call_pattern early-exit (MEDIUM, 30 min)
**File:** `crates/reliary-search/src/lazy_occurrence.rs:76-79`
**Issue:** `trimmed.contains('(') && trimmed.starts_with('(') && ... trimmed.chars().any(...)`.
**Fix:** Early-exit on `!bytes.contains(&b'(')`. Replace `chars().any()` with byte walk.
**Impact:** Eliminates 50%+ of per-line checks.

### L4: Cache "already populated" phrase_ids (LOW, 30 min)
**File:** `crates/reliary-search/src/lazy_occurrence.rs:212`
**Issue:** `has_occurrence` runs `SELECT 1 ... LIMIT 1` per phrase per file.
**Fix:** Cache populated phrase_ids in `FxHashSet<i64>` for the session. Skip if already seen.
**Impact:** Eliminates redundant SQL for phrases already built in this session.


## Phase 4: Pure Math in Sift + Output (~3 hours)

### P1: SWAR 8-byte DJB2 hash (HIGH, 1 hr)
**File:** `crates/reliary-sift/src/classify.rs:354-356`
**Issue:** Per-byte `h = h.wrapping_mul(33).wrapping_add(b as u64)`.
**Fix:**
```rust
while i + 8 <= s.len() {
    let chunk = u64::from_le_bytes(s.as_bytes()[i..i+8].try_into().unwrap());
    h = h.wrapping_mul(0x100000001_b3).wrapping_add(chunk);
    i += 8;
}
while i < s.len() {
    h = h.wrapping_mul(33).wrapping_add(s.as_bytes()[i] as u64);
    i += 1;
}
```
**Impact:** 4-8× faster skeleton hashing.

### P2: UUID detection via lookup table or uuid crate (CRITICAL, 1 hr)
**File:** `crates/reliary-sift/src/classify.rs:135-144`
**Issue:** 36-byte manual loop per candidate position.
**Fix:** Add 256-entry `IS_HEX_TABLE: [bool; 256]` for hex char detection. Check 8 bytes at a time using the table. Or: use `uuid::Uuid::parse_str` on 36-byte slices (already SIMD-optimized).
**Impact:** 5-10× faster UUID detection on cargo output.

### P3: Hex hash detection lookup table (HIGH, 30 min)
**File:** `crates/reliary-sift/src/classify.rs:163-169`
**Issue:** `(bytes[he] as char).is_ascii_hexdigit()` — unnecessary `as char` conversion.
**Fix:** Use `IS_HEX_TABLE[bytes[he] as usize]` — 256-entry bool table, branch-free lookup.
**Impact:** Eliminates char conversion per byte.

### P4: Skeleton dedup aggressive_skeleton vs skeleton (MEDIUM, 1 hr)
**File:** `crates/reliary-sift/src/classify.rs:110-204, 219-348`
**Issue:** `aggressive_skeleton` is a copy-paste of `skeleton` with extra rules.
**Fix:** Parameterize: `fn skeleton_inner(line: &str, aggressive: bool) -> String`. Mode only differs in step 6 (alpha word → `{w}` vs verbatim).
**Impact:** 130 LOC deleted, single source of truth.

### P5: strip_all_ansi uses memchr for chunk copy (MEDIUM, 30 min)
**File:** `crates/reliary-sift/src/classify.rs:84-107`
**Issue:** Per-character `out.push(c)` for the 99% plain-text majority.
**Fix:** Use `memchr::memchr(b'\x1b', &text[pos..])` to find ESC positions. Copy chunks between ESCs with `out.push_str(&text[start..esc_pos])`.
**Impact:** Eliminates per-char push for non-ANSI content.

### P6: find_visual_gutters walks bytes not Vec<char> (MEDIUM, 30 min)
**File:** `crates/reliary-sift/src/classify.rs:608-624`
**Issue:** `let chars: Vec<char> = line.chars().collect()` — UTF-8 decode + Vec alloc per line.
**Fix:** Walk bytes directly. Count consecutive runs of `b' '`.
**Impact:** Eliminates Vec<char> allocation per line.

### P7: Version X.Y.Z detection via memchr (LOW, 30 min)
**File:** `crates/reliary-sift/src/classify.rs:172-181`
**Issue:** Three O(n) digit-scan loops for one pattern.
**Fix:** Use `memchr::memchr(b'.', &bytes[ve..])` to find dot candidates, validate digit runs around them.
**Impact:** Early-exit for non-version strings.

### P8: BM25 hoist constants (LOW, 15 min)
**File:** `crates/reliary-search/src/lib.rs:84-93`
**Issue:** `k1 + 1.0` and `k1 * b` computed per call.
**Fix:**
```rust
const K1_PLUS_1: f32 = 2.2;
const K1_TIMES_B: f32 = 0.9;
const K1_TIMES_ONE_MINUS_B: f32 = 0.3;
```
**Impact:** Eliminates 3 multiplies per call.

### P9: Porter stem phf suffix lookup (MEDIUM, 1 hr)
**File:** `crates/reliary-search/src/lib.rs:118-132`
**Issue:** 20-element suffix array iterated linearly with `ends_with` per suffix.
**Fix:** Use `phf` perfect hash for suffix length → suffix list. Or: bucket by suffix length (2/3/4/5 chars), check only matching-length suffixes.
**Impact:** Branch-free suffix detection for the 20 common suffixes.

### P10: ANSI strip regex → byte DFA in output (MEDIUM, 30 min)
**File:** `crates/reliary-output/src/classify.rs:38, 52-54`
**Issue:** Uses regex for ANSI stripping. 3-5× slower than byte DFA.
**Fix:** Delete regex version, use `reliary_sift::classify::strip_all_ansi` (already has byte DFA).
**Impact:** 3-5× faster ANSI stripping in output compression path.

### P11: Decorative separator consolidation (MEDIUM, 15 min)
**Files:** `crates/reliary-sift/src/classify.rs:370-385`, `crates/reliary-output/src/collapse.rs:232-262`
**Issue:** Two implementations of "is this a decorative separator".
**Fix:** Promote `is_decorative_separator` to `pub fn` in sift. Output calls sift's version.
**Impact:** Single source of truth.

### P12: Structural chars const (LOW, 10 min)
**Files:** `crates/reliary-sift/src/lib.rs:279, 316`
**Issue:** Inline `&['{', '}', '(', ')', ';', '=', '<', '>']` redeclared in 2 places.
**Fix:** `pub const STRUCTURAL_CHARS: &[u8] = b"{}();=<>";` in sift. Both consumers reference one definition.
**Impact:** DRY.

### P13: Skeleton consolidation in output (MEDIUM, 30 min)
**File:** `crates/reliary-output/src/classify.rs:57-66`
**Issue:** Output has its own `skeleton` (regex chain) that duplicates sift's byte DFA.
**Fix:** Add `{time}` and `{progress}` cases to sift's skeleton. Delete output's `skeleton`.
**Impact:** Single skeleton impl, byte DFA everywhere.

### P14: Definition detection consolidation (HIGH, 3 hr)
**Files:** `reliary-search/src/lib.rs:270-316`, `reliary-sift/src/lib.rs:264-300`, `reliary-pack/src/lib.rs:1120-1237`
**Issue:** 3 implementations of "is this line a definition?" with different edge-case behavior.
**Fix:** Create `pub fn is_definition_like(line: &str) -> DefinitionKind` in `reliary-search::structural`. `DefinitionKind` enum: `Function`, `Struct`, `Trait`, `NotDefinition`. All 3 crates import one function. Each crate can still apply additional filtering on top.
**Impact:** Single definition detection logic, prevents drift.
**Risk:** Medium — each crate has tests asserting on specific edge cases. Need to ensure the consolidated version passes all 3 test suites.

### P15: find_references resolver chain consolidation (HIGH, 1 hr)
**Files:** `crates/reliary-agent/src/mcp.rs:858-875, 908-955, 957-1010, 1011-1058`
**Issue:** 4 tool arms each implement the same 3-step fallback (pattern_hybrid → auto → fallback).
**Fix:** Add `pub fn find_references_resolve(db, name, anchor_file, anchor_line, threshold) -> Vec<Hit>` in `reliary-search::type_flow`. All 4 arms call one function.
**Impact:** Eliminates duplicate dispatch logic.

### P16: DB open helper consolidation (HIGH, 30 min)
**Files:** `crates/reliary-agent/src/mcp.rs:291-308, 556-571, 580-596`, `main.rs:430, 454, 480`, `ux.rs:162, 400`, `read_summary.rs:48, 92`
**Issue:** 8+ inline DB opens with the same 3-line pattern.
**Fix:** `pub fn open_index_db(path: &str) -> Result<Connection, String>` in `agent::paths`. All callers use one helper.
**Impact:** Single open pattern, single PRAGMA set.

### P17: Index DB path format helper (LOW, 15 min)
**Files:** `crates/reliary-agent/src/mcp.rs:291, 556, 580`, `main.rs:377, 382`, `read_summary.rs:11, 91, 110`
**Issue:** `format!("{}/.reliary/index.sqlite", path.trim_end_matches('/'))` repeated 8+ times.
**Fix:** `pub fn index_db_path(codebase_path: &str) -> String` in `agent::paths`.
**Impact:** DRY.

### P18: Cached DB connection for MCP lifetime (HIGH, 1 hr)
**File:** `crates/reliary-agent/src/mcp.rs`
**Issue:** `Connection::open` per tool call (1-5ms overhead each).
**Fix:** `static CACHED_DB: OnceLock<Mutex<Connection>>`. Open once after auto-trust. All tool dispatch arms use `cached_db()`.
**Impact:** Eliminates per-call open overhead.

### P19: Anchor file resolver helper (MEDIUM, 30 min)
**Files:** `crates/reliary-agent/src/mcp.rs:840-846, 334, 594, 1506, 1603`
**Issue:** `resolve_af` closure + inline `if is_absolute { af } else { format! }` duplicated.
**Fix:** `pub fn resolve_anchor_file(af: &str, cwd: &str) -> String` in `agent::paths`.
**Impact:** DRY.

### P20: Cached index_db_path with LRU (LOW, 30 min)
**File:** `crates/reliary-agent/src/mcp.rs:43-47`
**Issue:** `cached_db_path()` memoizes CWD only. User-provided paths bypass cache.
**Fix:** `pub fn cached_index_db_path(input: &str) -> &'static Path` with input normalization + LRU.
**Impact:** Eliminates per-call format! allocation.

### P21: Shared keyword/common-words (MEDIUM, 30 min)
**Files:** `crates/reliary-search/src/keywords.rs:14-39`, `crates/reliary-pack/src/lib.rs:1240-1274`
**Issue:** Two separate keyword sets with overlap.
**Fix:** Move `is_common_word` into `reliary-search::keywords`. One `OnceLock<HashSet>`.
**Impact:** Single definition.

### P22: Vec::with_capacity for file lines (MEDIUM, 30 min)
**Files:** `crates/reliary-search/src/brace_graph.rs:217`, `crates/reliary-pack/src/lib.rs:961, 2225, 2266`
**Issue:** `content.lines().map(String::from).collect()` without capacity hint.
**Fix:** `let n = content.lines().count(); let mut v = Vec::with_capacity(n); v.extend(content.lines().map(String::from));`.
**Impact:** Eliminates 9 reallocations per file (cap doubling 4→8→...→512).

### P23: FileResult Vec capacity (MEDIUM, 15 min)
**File:** `crates/reliary-search/src/ingest.rs:282-289`
**Issue:** Empty content Vec built without capacity from lines.len().
**Fix:** `Vec::with_capacity(content.lines().count())`.
**Impact:** Eliminates reallocs during indexing.

### P24: Arc<Vec<String>> for shared lines across crates (MEDIUM, 1 hr)
**Files:** All crates that read file lines
**Issue:** Same file's lines stored as `Vec<String>` in 5 separate caches.
**Fix:** After S2 (pack uses file_meta), lines are shared via `Arc<FileMeta>`. This is a follow-on: make `FileMeta.lines: Arc<[Box<str>]>` so multiple readers share the same backing storage without cloning.
**Impact:** Memory dedup across caches.

---

## Phase 5: Verification (~1 hour)

1. `cargo test --lib` — all tests pass (225+)
2. `cargo build --release` — clean compile
3. Re-index tokio corpus
4. Run long bench (seeds 42, 17, conditions A, C)
5. Compare WC, score, wall time to V19 baseline
6. Verify no regression (score within ±2, WC within ±10%)
7. Run determinism tests (sift compression stable across runs)

## Estimated Total Effort

| Phase | Hours | Items |
|-------|-------|-------|
| 1 (Cross-cutting quick wins) | 3 | 9 |
| 2 (Search + structural math) | 4 | 12 |
| 3 (Lazy occurrence + indexing) | 3 | 4 |
| 4 (Sift + output + consolidation) | 7 | 16 |
| 5 (Verification) | 1 | — |
| **Total** | **~18** | **41** |

## Deferred (not worth doing)

- Consolidate ANSI strippers (3→1) — already optimized where it matters
- Shared keyword/common-words lists — marginal
- Skeletal chars const — local duplication, no measurable cost
- BTreeMap for cache eviction — already O(N)