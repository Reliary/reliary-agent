# FIX PLAN V19 — All-Crate Performance Audit

**Date:** 2026-07-13
**Auditor:** 3 parallel explore agents + manual consolidation
**Scope:** 7 previously-unaudited crates (reliary-core, reliary-compress, reliary-risk, reliary-memory, reliary-fix, reliary-edit, reliary-pack)
**Total issues:** 136 (12 CRITICAL, 25 HIGH, 50 MEDIUM, 49 LOW)

---

## Phase A: Cross-Crate Quick Wins (~2 hours)

### A1: Swap `HashMap` → `FxHashMap`/`AHashMap` in 7 crates
**Severity:** HIGH
**Impact:** SipHash is 2-3× slower than FxHash/ahash on short string keys
**Crates:** core, compress, risk, memory, fix, pack (all use `std::collections::HashMap`)
**Fix:** Add `rustc-hash` or `ahash` dep to each crate's `Cargo.toml`, swap `HashMap` → `FxHashMap`/`AHashMap`

### A2: Add `#[inline(always)]` to 10+ hot small functions across crates
**Severity:** MEDIUM
**Impact:** Eliminates function call overhead on hottest paths
**Functions:**
- `reliary-memory`: `dot()`, `bipolar_clamp()`, `bundle()`
- `reliary-core`: `now()`, `default_ttl()`, `default_max_entries()`
- `reliary-risk`: `compute_file_risk()`, `compute_blast_radius()`
- `reliary-search`: (already done in V18)

### A3: Delete dead code across all crates
**Severity:** LOW (compile time + clarity)
**Items:**
- `reliary-core/lib.rs:15-31`: Dead `Session` struct (superseded by `SessionState`)
- `reliary-core/session.rs:54-58`: Dead `edited_files()` method
- `reliary-core/fs_safe.rs:131-146`: Dead `apply_speed_pragma` + `set_page_size_64k`
- `reliary-core/fs_safe.rs:150-158`: Dead `check_write_size`
- `reliary-compress/lib.rs:176-181`: Dead SRCR block (`srcr_for_compression`, `compute_srcr`, `preservation_hit_rate`)
- `reliary-fix/lib.rs:5-11`: Dead `Fix` struct (never constructed)
- `reliary-pack/lib.rs:280-301`: Dead `symbol_hotspot_score`
- `reliary-pack/lib.rs:1799,1816-1819`: Dead `pre_dedup_len`/`post_dedup_len` block
- `reliary-edit/lib.rs:1-11`: Placeholder stub (empty crate)

---

## Phase B: reliary-memory (~4 hours)

### B1: `ensure_token_hv` always clones 10K-byte hypervector
**File:** `lib.rs:132-137`
**Severity:** CRITICAL
**Impact:** Two 10KB clones per `hebbian_update` call, called for every token pair
**Fix:** Return `&Hypervector` (borrow) instead of clone. Operate in-place via `self.token_hvs.get_mut(a)`, `get_mut(b)`

### B2: `recall` re-encodes every memory per query (N×M×D)
**File:** `lib.rs:204-208`
**Severity:** CRITICAL
**Impact:** O(members × tokens × dimensions) per query instead of O(members × dimensions)
**Fix:** Cache `encoded: Hypervector` per `MemoryRecord` at ingest time. `recall` becomes O(M × D) dot products.

### B3: `predict` linear-scans entire cooccur map per token
**File:** `lib.rs:217-228`
**Severity:** CRITICAL
**Impact:** O(|tokens| × |cooccur|) per prediction
**Fix:** Build inverted index `FxHashMap<String, Vec<(String, u64)>>` at retain time. Prediction becomes O(|tokens| × avg_neighbors).

### B4: `retain` clones two Strings per cooccur key
**File:** `lib.rs:147-148`
**Severity:** CRITICAL
**Impact:** N=100 tokens → ~500 clone pairs = 1000 heap allocs per memory ingest
**Fix:** Intern tokens into `Vec<String>` side table, key cooccur on `(u32, u32)` indices.

### B5: `hebbian_update` per-element f64 cast in hot loop
**File:** `lib.rs:174-175`
**Severity:** HIGH
**Impact:** 10K-dim loop, two f64 promotions + two i8 truncations per iteration
**Fix:** Integer math: `hv_a[i] = (hv_a[i] as i32 + (hv_b[i] as i32 >> 1)) as i8;` — pure integer, vectorizes.

### B6: `save_persistent` non-transactional INSERTs, no stmt cache
**File:** `lib.rs:110-118`
**Severity:** HIGH
**Impact:** Each INSERT is its own transaction (fsync per row). Catastrophic for 10K memories.
**Fix:** Wrap in `BEGIN IMMEDIATE`/`COMMIT`. Use `conn.prepare_cached()`.

### B7: `make_hv` no `with_capacity`
**File:** `lib.rs:9-12`
**Severity:** HIGH
**Impact:** 10K-byte Vec grows via realloc
**Fix:** `Vec::with_capacity(dims)`

### B8: `token_hvs` and `cooccur` use SipHash HashMap
**File:** `lib.rs:124-126`
**Severity:** HIGH
**Impact:** SipHash ~3× slower than FxHash for string keys
**Fix:** `FxHashMap<String, Hypervector>` and `FxHashMap<(String, String), u64>`

### B9: `recall` full sort then truncate
**File:** `lib.rs:201-212`
**Severity:** HIGH
**Impact:** O(M log M) sort when only top_n needed
**Fix:** `scored.select_nth_unstable_by(top_n, ...)` then truncate.

### B10: `recall` clones entire `MemoryRecord` per scored entry
**File:** `lib.rs:49-52`
**Severity:** MEDIUM
**Impact:** Clones content String per memory per query
**Fix:** Return `Vec<usize>` (indices), only clone top_n winners.

### B11: `dot()` recomputes norm per call
**File:** `lib.rs:29-34`
**Severity:** HIGH
**Impact:** For bipolar HVs, norm = sqrt(dims) ≈ constant. Recomputed per call.
**Fix:** Precompute norm once at store time.

### B12: `scan_tokens` allocates per token via `.to_lowercase()`
**File:** `lib.rs:244`
**Severity:** MEDIUM
**Impact:** One heap alloc per token
**Fix:** `to_ascii_lowercase()` (no Unicode table, faster)

### B13: `save_persistent` schema duplicates `open_persistent` CREATE TABLE
**File:** `lib.rs:57, 95`
**Severity:** LOW
**Fix:** Extract to `fn ensure_schema(conn: &Connection)`

### B14: `save_persistent` doesn't write `id` column
**File:** `lib.rs:83`
**Severity:** LOW (correctness)
**Impact:** IDs reset on reload
**Fix:** Write `id` in INSERT or use AUTOINCREMENT properly

### B15: Missing `#[inline]` on `bundle`, `bipolar_clamp`, `dot`
**Severity:** MEDIUM
**Fix:** Add `#[inline(always)]`

### B16: `predict` clones `b` (and `a`) for every cooccur iteration
**File:** `lib.rs:220-221`
**Severity:** HIGH
**Impact:** M string clones per query token
**Fix:** Use interning (B4) or borrow from cooccur key

### B17: `open_persistent` runs `PRAGMA journal_mode=WAL` each open
**File:** `lib.rs:58-59`
**Severity:** MEDIUM
**Impact:** WAL checkpoint header per call
**Fix:** Acceptable for safety; low priority

### B18: `id = self.memories.len() + 1` position-based ID
**File:** `lib.rs:155`
**Severity:** LOW (correctness)
**Fix:** Use AUTOINCREMENT or UUID

### B19: `extract_preservation_targets` (compress) — already listed in Phase D

---

## Phase C: reliary-pack (~3 hours)

### C1: Double DB open in `generate_pack_auto`
**File:** `lib.rs:502-509`
**Severity:** CRITICAL
**Impact:** 2× connection setup + schema validation + 5 queries on every auto call
**Fix:** Compute `compute_complexity_score` once, pass open `Connection` through. Skip all gate queries on `Skip` decision.

### C2: `build_cross_refs_from_index` materializes 600K rows twice
**File:** `lib.rs:2091-2120`
**Severity:** CRITICAL
**Impact:** O(N) memory + 2× alloc per row. Two full-table materializations.
**Fix:** Push join into SQL. Stream and build only the inverted index. Or use temp table with `WHERE phrase IN (...)`.

### C3: `build_cross_refs_from_index` panics on SQLite error
**File:** `lib.rs:2094-2120`
**Severity:** CRITICAL
**Impact:** Process crash on malformed/index-shifted DB
**Fix:** Return `Result` (like sibling `build_cross_refs_for_subset`)

### C4: `.cloned().unwrap_or_default()` per symbol in render loop
**File:** `lib.rs:265,266,361,362,863,864`
**Severity:** HIGH
**Impact:** Clones entire function body String per symbol. O(total_source_bytes) of needless cloning.
**Fix:** Pass `&[String]` / `&str` — `cross_refs.get(&sym.name).map(|v| v.as_slice()).unwrap_or(&[])`

### C5: `Vec::new()` without capacity in loops
**File:** `lib.rs:254,349,522,929,1275,1280,1533,1800,1986,2418,2449`
**Severity:** HIGH
**Impact:** Repeated regrowth+realloc in hot loops
**Fix:** `Vec::with_capacity(symbols.len()+1)` for entries; `with_capacity(16)` for surprises

### C6: `i64`-keyed HashMap could be direct-index Vec
**File:** `lib.rs:441, 2103`
**Severity:** HIGH
**Impact:** Hash+collision overhead on every ~600K insertions
**Fix:** `Vec<Option<Vec<(String,bool)>>>` indexed by `file_id as usize`

### C7: All `HashMap<String,_>` uses SipHash (28+ sites)
**File:** `lib.rs:282,283,377,378,426,428,668,692,742,1531,2081-2084,2203-2207,2244-2249,2374,2474`
**Severity:** HIGH
**Impact:** SipHash 2-3× slower than FxHash
**Fix:** `use rustc_hash::FxHashMap` as drop-in

### C8: `lines[start..end].join("\n")` then re-split by lines
**File:** `lib.rs:2233,2283 then 1536`
**Severity:** HIGH
**Impact:** Join then re-split — double allocation per body
**Fix:** Pass `&[String]` (cached lines slice) directly instead of owned joined String

### C9: `format!` + `Connection::open` copy-pasted in 4 functions
**File:** `lib.rs:233-238, 315-320, 813-818, 78-83`
**Severity:** HIGH
**Impact:** Repeated open/validation
**Fix:** Extract `fn open_index(path) -> Result<Connection, String>`

### C10: Dead `symbol_hotspot_score` function
**File:** `lib.rs:280-301`
**Severity:** HIGH (dead code)
**Fix:** Delete

### C11: Dead `pre_dedup_len`/`post_dedup_len` block
**File:** `lib.rs:1799, 1816-1819`
**Severity:** HIGH (dead code)
**Fix:** Remove the block

### C12: `compute_complexity_score` — 5 separate `query_row` calls
**File:** `lib.rs:90,120,135,158,170`
**Severity:** HIGH
**Impact:** 5× statement compilation + 5 round trips per gate decision
**Fix:** Combine into single `SELECT (subquery1),(subquery2),...` or use `prepare_cached`

### C13: `.to_lowercase()` / `.to_ascii_lowercase()` allocates per call in hot loops
**File:** `lib.rs:537,663,710,1198,1260,1748,2022,2417,2443`
**Severity:** HIGH
**Impact:** One heap allocation per call; hundreds per pack
**Fix:** Use `eq_ignore_ascii_case` or pre-lowercase once at caller

### C14: `read_doc_comment` front-of-Vec insert = O(n²)
**File:** `lib.rs:1283,1299,1310,1312,1314`
**Severity:** MEDIUM
**Fix:** `push` then `reverse()`, or use `VecDeque`

### C15: `derive_crate_name` allocates Vec per call
**File:** `lib.rs:2359-2371`
**Severity:** MEDIUM
**Fix:** Iterate `split('/')` manually; cache per `sym.file`

### C16: `derive_module_name` allocates per component
**File:** `lib.rs:2386-2409`
**Severity:** MEDIUM
**Fix:** Cache module name per unique file in `HashMap<String,String>`

### C17: `tokenize` returns `Vec<String>` (alloc per token)
**File:** `lib.rs:558-563`
**Severity:** MEDIUM
**Fix:** Return `Vec<&str>` borrowing from input

### C18: `format!("{} {}", entry.name, entry.body)` per entry for tokenize
**File:** `lib.rs:653,660-665`
**Severity:** MEDIUM
**Fix:** Tokenize name and body separately, merge token streams

### C19: `selected_names.clone()` re-hashes HashSet
**File:** `lib.rs:746-747`
**Severity:** MEDIUM
**Fix:** Seed from iterator, not clone

### C20: `compute_inbound_ref_counts` pulls all phrases then filters in Rust
**File:** `lib.rs:374-418`
**Severity:** MEDIUM
**Fix:** Push `WHERE phrase IN (...)` or `JOIN` into SQL

### C21: `phrase.clone()` per row in cross-ref build
**File:** `lib.rs:2124-2134`
**Severity:** MEDIUM
**Fix:** Insert `&str` references first, or use `FxHashMap<String, SmallVec<[i64; 4]>>`

### C22: `read_to_string` then `lines().map(String::from).collect()` — String per line
**File:** `lib.rs:949-951, 2215, 2256`
**Severity:** MEDIUM
**Impact:** 10K-line file = 10K Strings
**Fix:** Keep file as one `String`, store `Vec<&str>` slices

### C23: `cross_refs.iter().take(5).cloned().collect()` then `.join(", ")`
**File:** `lib.rs:2517`
**Severity:** MEDIUM
**Fix:** `cross_refs.iter().take(5).map(|s| s.as_str()).collect::<Vec<_>>().join(", ")`

### C24: Dedup loop allocates `final_surprises` + `seen_fragments` in addition to originals
**File:** `lib.rs:1838`
**Severity:** MEDIUM
**Fix:** Dedup in place with `retain` + reused set

### C25: `String::from_utf8_lossy(&bytes[start..i]).to_string()` — double alloc
**File:** `lib.rs:1985-2019, 1874-1900, 1902-1928`
**Severity:** MEDIUM
**Fix:** `std::str::from_utf8(&bytes[start..i]).unwrap_or("").to_string()` (single alloc)

### C26: 12-way `OR f.file_path LIKE '%.rs'` — no index, full scan
**File:** `lib.rs:96-107`
**Severity:** MEDIUM
**Fix:** Pull `file_path` once, check extension in Rust

### C27: Repeated 8-way `NOT LIKE '%/bench/%'` in two queries
**File:** `lib.rs:140-147, 389-396`
**Severity:** MEDIUM
**Fix:** Filter in Rust after single fetch, or add `is_source` column

### C28: `extract_surprise_from_body` materializes all lines into Vec
**File:** `lib.rs:1536`
**Severity:** MEDIUM
**Fix:** Iterate `body.lines()` directly

### C29: `detect_modules` clones entire `Symbol` per entry
**File:** `lib.rs:2378`
**Severity:** MEDIUM
**Fix:** Store `&Symbol` refs or indices

### C30: `generate_pack_hotspot` clones top_k Symbols
**File:** `lib.rs:341`
**Severity:** MEDIUM
**Fix:** Acceptable; could use `Rc<Symbol>`

### C31: BM25 uses f64 (could be f32)
**File:** `lib.rs:659,681,687,689,697,702,704-706`
**Severity:** LOW
**Fix:** Switch to f32

### C32: `COMMON` array has duplicates
**File:** `lib.rs:1234-1256`
**Severity:** LOW
**Fix:** Dedupe the literal array

### C33: Full sort for top-K (should be `select_nth_unstable`)
**File:** `lib.rs:329-336, 722`
**Severity:** LOW
**Fix:** `select_nth_unstable_by`

### C34: Linear scan of entries for prefix matching
**File:** `lib.rs:751-792`
**Severity:** LOW
**Fix:** Sorted `Vec<&str>` + `binary_search`

### C35: `query.chars().take(80).collect::<String>()` allocates truncated String
**File:** `lib.rs:799`
**Severity:** LOW
**Fix:** `&query[..80]` at char boundary

### C36: `is_common_word` allocates via `.to_ascii_lowercase()` before lookup
**File:** `lib.rs:1258-1261`
**Severity:** LOW
**Fix:** Compare case-insensitively without allocating

### C37: `surprises.iter().filter(…).count()` — two passes
**File:** `lib.rs:1821-1824`
**Severity:** LOW
**Fix:** Count in dedup loop

### C38: `func_indicators` stack array re-created per call
**File:** `lib.rs:611-621`
**Severity:** LOW
**Fix:** `const` array + `OnceLock`

### C39: `symbol_names.contains(phrase.as_str())` pulls irrelevant phrases
**File:** `lib.rs:411-413`
**Severity:** LOW
**Fix:** Push membership into SQL (C20)

### C40: `extract_phrases` returns `Vec<(String, String)>` — two allocs per phrase
**File:** `lib.rs:39-56`
**Severity:** LOW
**Fix:** Return `Vec<(String, &str)>` borrowing from input

---

## Phase D: reliary-compress + reliary-fix + reliary-core + reliary-risk (~4 hours)

### D1: `CompressionDict::apply` O(n×m) `result.replace()` per entry
**File:** `reliary-compress/lib.rs:74-82`
**Severity:** HIGH
**Impact:** 500 dict entries × 10KB text = 5MB scanning + 500 allocs
**Fix:** Use `aho_corasick::AhoCorasick` for multi-pattern literal replacement in single pass

### D2: `compress_reasoning` — 10 `re.replace_all` each allocating fresh String
**File:** `reliary-compress/lib.rs:95-97`
**Severity:** HIGH
**Impact:** 10 full-text allocations regardless of match
**Fix:** Only reassign if `Cow` is `Owned`. Better: combine patterns into one Regex alternation.

### D3: `extract_preservation_targets` clones accumulator per target
**File:** `reliary-compress/lib.rs:124-141`
**Severity:** MEDIUM
**Fix:** `targets.push(std::mem::take(&mut current))` — moves buffer, no clone

### D4: `preservation_hit_rate` — O(U × L) substring search per target
**File:** `reliary-compress/lib.rs:153`
**Severity:** MEDIUM
**Fix:** Build `AhoCorasick` over unique targets, scan compressed once

### D5: `build_dict` clones each symbol for `seen.entry(s.clone())`
**File:** `reliary-compress/lib.rs:60`
**Severity:** MEDIUM
**Fix:** `AHashMap<&str, u32>` with borrowed keys

### D6: `extract_phrases` allocates Vec of 15 &str
**File:** `reliary-compress/lib.rs:44`
**Severity:** MEDIUM
**Fix:** Iterate directly without collecting

### D7: `compress_reasoning` always clones input even when dict present
**File:** `reliary-compress/lib.rs:87-100`
**Severity:** LOW
**Fix:** `let mut t = if let Some(d) = dict { d.apply(text) } else { text.to_string() };`

### D8: `f64` comparison where integer math suffices
**File:** `reliary-compress/lib.rs:99`
**Severity:** MEDIUM
**Fix:** `if t.len() * 10 < original_len * 6 { Some(t) } else { None }`

### D9: `apply_fixes` reallocates whole content per fix
**File:** `reliary-fix/lib.rs:130-149`
**Severity:** CRITICAL
**Impact:** O(F × S) with F heap allocs of size S
**Fix:** Single-pass: use `aho_corasick` for multi-needle replace, or walk content once

### D10: `apply_fixes` double-scans: `matches().count()` then `replace`
**File:** `reliary-fix/lib.rs:134`
**Severity:** HIGH
**Fix:** Do replace and observe len delta, or use `replacen` with count

### D11: 6 sequential regex passes over input
**File:** `reliary-fix/lib.rs:33-58`
**Severity:** HIGH
**Fix:** `RegexSet` for single-pass multi-pattern matching

### D12: `find_function` up to 4 linear scans of lines
**File:** `reliary-fix/lib.rs:67-86`
**Severity:** HIGH
**Fix:** Single pass with state machine

### D13: `apply_fixes` clones content even when fixes is empty
**File:** `reliary-fix/lib.rs:130`
**Severity:** MEDIUM
**Fix:** Guard: `if fixes.is_empty() { return (content.to_string(), 0); }`

### D14: `find_boundary` recomputes `line.trim()` multiple times per line
**File:** `reliary-fix/lib.rs:107-126`
**Severity:** MEDIUM
**Fix:** Cache `let t = line.trim();` once

### D15: `extract_func_name` linear scan of ~20-element skip array
**File:** `reliary-fix/lib.rs:92-104`
**Severity:** MEDIUM
**Fix:** Sorted array + binary search, or `phf` set

### D16: `Fix` struct dead code (never constructed)
**File:** `reliary-fix/lib.rs:5-11`
**Severity:** LOW
**Fix:** Delete

### D17: `content_aware_match` potentially dead code
**File:** `reliary-fix/lib.rs:153-163`
**Severity:** LOW
**Fix:** Verify with `cargo public-api`, delete if unused

### D18: `read_summary()` called twice in `state_block`
**File:** `reliary-core/state_block.rs:15 and :52`
**Severity:** HIGH
**Impact:** Doubles the hottest function in the crate (allocations, clones, hashing)
**Fix:** Replace `state.read_summary().len()` with `reads.len()`

### D19: `read_summary()` allocates HashMap + Vec + clones + format! per call
**File:** `reliary-core/session.rs:41-52`
**Severity:** HIGH
**Fix:** (1) Dedup key `(&str, &str)` instead of `format!`. (2) Return `Vec<&ReadRecord>` (borrow). (3) Use `AHashMap`. (4) Cache result.

### D20: `ingest.rs` 4 HashMaps with String keys, no pre-sizing, SipHash
**File:** `reliary-core/ingest.rs:10-14`
**Severity:** HIGH
**Fix:** `AHashMap::with_capacity_and_hasher(N, Default::default())`

### D21: `ingest.rs` `serde_json::from_str` then `v.get()` repeatedly allocates Value
**File:** `reliary-core/ingest.rs:18`
**Severity:** MEDIUM
**Fix:** Parse into typed structs per `type` instead of `Value` free-for-all

### D22: `ingest.rs` `format!("{:x}", total_size)` as fake hash
**File:** `reliary-core/ingest.rs:69`
**Severity:** MEDIUM (correctness + alloc)
**Fix:** Compute real hash (xxhash) or rename to `size_hex`

### D23: `state_block.rs` `Vec::new()` without `with_capacity`
**File:** `reliary-core/state_block.rs:9`
**Severity:** MEDIUM
**Fix:** `Vec::with_capacity(6)`

### D24: `state_block.rs` `.chars().take(25).collect()` then `.replace('\n', " ")` — double alloc
**File:** `reliary-core/state_block.rs:17-23, 29`
**Severity:** MEDIUM
**Fix:** Map newlines to spaces during collection

### D25: `state_block.rs` edit_lines — 8+ throwaway allocs per state block
**File:** `reliary-core/state_block.rs:35-38`
**Severity:** MEDIUM
**Fix:** Build each edit line into pre-sized String directly

### D26: `content_cache.rs` uses `DefaultHasher` (SipHash) + `format!` alloc
**File:** `reliary-core/content_cache.rs:21-27`
**Severity:** MEDIUM
**Fix:** Use `ahash::AHasher` or `xxhash_rust::xxh3_64`. Return `u64`, format lazily.

### D27: `content_cache.rs` `row.get::<_, Vec<u8>>(0)` allocates Vec per retrieve
**File:** `reliary-core/content_cache.rs:65`
**Severity:** MEDIUM
**Fix:** Acceptable given rusqlite API. Low priority.

### D28: `content_cache.rs` `hash_content` is `pub` but only used internally
**File:** `reliary-core/content_cache.rs:21-27`
**Severity:** MEDIUM
**Fix:** Make `pub(crate)` or private

### D29: `content_cache.rs` `stats()` runs 2 separate queries
**File:** `reliary-core/content_cache.rs:100-112`
**Severity:** LOW
**Fix:** Combine into one `SELECT COUNT(*), COALESCE(SUM(LENGTH(original)),0)`

### D30: `content_cache.rs` `evict()` runs COUNT after TTL delete
**File:** `reliary-core/content_cache.rs:73-96`
**Severity:** LOW
**Fix:** Skip count/trim if expired >= estimate

### D31: `fs_safe.rs` `atomic_write` opens file twice
**File:** `reliary-core/fs_safe.rs:27-46`
**Severity:** LOW
**Fix:** `File::create` → `write_all` → `sync_all` → `drop` → `rename` in one handle

### D32: `fs_safe.rs` `format!` for tmp path allocates
**File:** `reliary-core/fs_safe.rs:28`
**Severity:** LOW
**Fix:** Acceptable; writes aren't hot

### D33: `fs_safe.rs` `safe_open_db` and `safe_open_db_query` near-duplicates
**File:** `reliary-core/fs_safe.rs:103-127`
**Severity:** LOW
**Fix:** One `open_db(path, mode: DbMode)` with enum

### D34: `fs_safe.rs` `File::open` result ignored via `let _ =`
**File:** `reliary-core/fs_safe.rs:36-37`
**Severity:** LOW (correctness)
**Fix:** Propagate error

### D35: `fs_safe.rs` `apply_speed_pragma` dead but documented as active
**File:** `reliary-core/fs_safe.rs:131-146`
**Severity:** LOW (but CRITICAL-adjacent — DB speed tuning never executes)
**Fix:** Either call it from `safe_open_db` or delete and fix docs

### D36: `fs_safe.rs` `check_write_size` dead code
**File:** `reliary-core/fs_safe.rs:150-158`
**Severity:** LOW
**Fix:** Delete or wire into `atomic_write`

### D37: `ingest.rs` `extract_error_summary` iterates lines twice
**File:** `reliary-core/ingest.rs:133-149`
**Severity:** LOW
**Fix:** Single pass storing fallback line

### D38: Missing `#[inline]` on `now()`, `default_ttl()`, `default_max_entries()`
**File:** `reliary-core/content_cache.rs:13-18, 114-120`
**Severity:** LOW
**Fix:** Add `#[inline]`

### D39: `compute_file_risk` collects lines into Vec then iterates 4+ times
**File:** `reliary-risk/lib.rs:23-24`
**Severity:** HIGH
**Fix:** Single-pass `fold()` collecting all counters at once

### D40: `.to_lowercase()` allocates per line in test_refs filter
**File:** `reliary-risk/lib.rs:38`
**Severity:** HIGH
**Impact:** 500 transient heap allocs on a 500-line file
**Fix:** `to_ascii_lowercase()` or `eq_ignore_ascii_case`

### D41: 4 separate `content.matches("TODO").count()` full scans
**File:** `reliary-risk/lib.rs:43-46`
**Severity:** HIGH
**Impact:** 4× full content scan
**Fix:** Single pass tracking all needles

### D42: `.to_string()` on module path in per-line loop
**File:** `reliary-risk/lib.rs:54`
**Severity:** MEDIUM
**Fix:** Push `&str` slice, own only at final push

### D43: `Vec::new()` on `read_first` then `.truncate(3)`
**File:** `reliary-risk/lib.rs:49`
**Severity:** MEDIUM
**Fix:** `Vec::with_capacity(3)` + break after 3

### D44: `format!()` always allocates `reason` even when unused
**File:** `reliary-risk/lib.rs:75-78`
**Severity:** LOW
**Fix:** Acceptable for API stability

### D45: Closure-based `.filter().count()` re-iterates lines per metric
**File:** `reliary-risk/lib.rs:28-34, 37-40`
**Severity:** MEDIUM
**Fix:** Single fold

### D46: `FileRisk` derives `Clone` but never cloned
**File:** `reliary-risk/lib.rs:4`
**Severity:** LOW
**Fix:** Remove derive

### D47: `read_first.truncate(3)` silences >3 imports
**File:** `reliary-risk/lib.rs:60`
**Severity:** LOW (correctness)
**Fix:** Score all or document limit

### D48: Missing `#[inline]` on `compute_file_risk`, `compute_blast_radius`
**File:** `reliary-risk/lib.rs`
**Severity:** LOW
**Fix:** Add `#[inline]`

### D49: `reliary-edit` is placeholder stub
**File:** `reliary-edit/lib.rs:1-11`
**Severity:** LOW
**Fix:** Port `stria/src/edit.rs` or remove from workspace

---

## Phase E: Deferred / Future

### E1: `RwLock` for read-heavy caches (future-proofing)
**Severity:** DEFER
**Reason:** MCP server is single-threaded; no concurrent readers yet
**When:** When multi-threading is needed

### E2: `ArrayVec` for fixed-size hot collections
**Severity:** DEFER
**Reason:** SmallVec is sufficient
**When:** If SmallVec spill becomes measurable

### E3: SWAR byte classification in scan_identifiers
**Severity:** DEFER (V17 plan item A1)
**Reason:** Complex, 3h effort, marginal on real workloads
**When:** If scan_identifiers becomes profiled bottleneck

### E4: CSR sparse vectors for cosine similarity
**Severity:** DEFER (V17 plan item A3)
**Reason:** Complex, 4h effort
**When:** If cosine similarity becomes profiled bottleneck

### E5: String interning
**Severity:** DEFER (V17 plan item A5)
**Reason:** Complex, 4h effort, cross-crate refactor
**When:** If string comparison becomes profiled bottleneck

---

## Verification Plan

After each phase:
1. `cargo test --lib` — all tests pass
2. `cargo build --release` — clean compile
3. Run long bench (seeds 42, 17, conditions A, C)
4. Compare WC, score, wall time to V18 baseline
5. Verify no regression (score within ±2, WC within ±10%)

## Estimated Total Effort

| Phase | Hours | Items |
|-------|-------|-------|
| A (quick wins) | 2 | 3 |
| B (memory) | 4 | 19 |
| C (pack) | 3 | 40 |
| D (compress+fix+core+risk) | 4 | 49 |
| E (deferred) | — | 5 |
| **Total** | **~13** | **116 actionable** |
