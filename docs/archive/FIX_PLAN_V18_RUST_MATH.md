# FIX PLAN V18 — Rust-Specific + Pure-Math Optimizations (Stacked)

**Status:** M1-M6 from V17 are DONE. This plan stacks the remaining pure-math
tricks (A1-A5, B1-B5) with Rust-specific levers (R1-R10) on top.

**Constraint:** No regression. All changes are drop-in replacements or
additive optimizations. No logic changes, no algorithm changes.

**Total items:** 17 (11 new math + 6 Rust-specific, 2 deferred)
**Estimated effort:** ~22 hours

---

## Phase 1: Zero-Risk Drop-In Replacements (1.25 hours)

### R1: `parking_lot::Mutex` replaces `std::sync::Mutex` (3-10x lock/unlock)

**Current:** 8 `std::sync::Mutex` sites. `std::sync::Mutex` does a syscall on
contention — even on the fast path (uncontended), it's ~20-40ns due to atomic
CAS + potential kernel futex overhead.

`parking_lot::Mutex` uses user-space fast path with adaptive spinning — 2-5ns
uncontended, no syscall until truly contended.

**Sites:**
- `file_meta.rs:22` — `Mutex<HashMap<String, Arc<FileMeta>>>`
- `brace_graph.rs:198` — `Mutex<FxHashMap<String, Arc<BraceNode>>>`
- `func_profile.rs:17` — `Mutex<FxHashMap<String, Vec<FunctionProfile>>>`
- `scope_types.rs:227` — `Mutex<HashMap<String, Vec<(BraceNode, ScopeTypeMap)>>>`
- `type_flow.rs:339` — `Mutex<HashMap<(String, i32, String), String>>`
- `type_flow.rs:577` — `Mutex<HashMap<(String, usize, String), String>>`
- `full_file.rs:24` — `Mutex<Option<FxHashMap<String, FileInfo>>>` (static, switch to OnceLock)

**Change:**
```toml
# Cargo.toml (workspace or per-crate)
parking_lot = "0.12"
```

```rust
// Before
static C: OnceLock<std::sync::Mutex<HashMap<...>>> = OnceLock::new();
c.lock().unwrap()
// After
static C: OnceLock<parking_lot::Mutex<HashMap<...>>> = OnceLock::new();
c.lock() // no Result, never poisons
```

**Impact:** 3-10x on lock/unlock for every cache hit.
**Effort:** 30 min
**Risk:** None — `parking_lot` is the standard ecosystem replacement.
**Verification:** All tests pass. No behavior change.

### R8: `ahash` for string-keyed HashMaps (3-5x on string hashing)

**Current:** `rustc-hash` (FxHash) is optimized for integers but mediocre for
strings. `ahash` uses AES-NI instructions for string hashing — 3-5x faster.

**Sites:** All caches keyed by file path `String`:
- `file_meta.rs:22` — `HashMap<String, Arc<FileMeta>>`
- `brace_graph.rs:198` — `FxHashMap<String, Arc<BraceNode>>`
- `func_profile.rs:17` — `FxHashMap<String, Vec<FunctionProfile>>`
- `scope_types.rs:227` — `HashMap<String, Vec<...>>`
- `full_file.rs:24` — `FxHashMap<String, FileInfo>`

Note: Keep `FxHashMap` for integer-keyed maps (phrase_id, file_id). Only swap
string-keyed maps to `AHashMap`.

**Change:**
```toml
ahash = "0.8"
```

```rust
use ahash::AHashMap;
static C: OnceLock<parking_lot::Mutex<AHashMap<String, Arc<FileMeta>>>> = OnceLock::new();
```

**Impact:** 3-5x on string-keyed HashMap lookups. Hit on every tool call.
**Effort:** 30 min
**Risk:** None.
**Verification:** All tests pass.

### R4: `#[inline(always)]` on 5 hot functions

**Current:** Several hot functions lack inline hints:
- `porter_stem` (called per-token during indexing)
- `bm25_idf`, `bm25_score` (called per-phrase per-search)
- `unpack_count`, `unpack_zone_int` (called per-occurrence)

**Change:** Add `#[inline(always)]` to each.

**Impact:** Eliminates function call overhead on hottest paths. ~2-4ns per call
saved. For 100K tokens x 5 functions = 500K calls saved.
**Effort:** 15 min
**Risk:** None.
**Verification:** All tests pass. Binary size may increase slightly.

### R6: `Box<str>` instead of `String` for stored strings (33% memory reduction)

**Current:** `FileMeta` stores `Vec<String>` for fn_names, impl_targets. Each
`String` is 24 bytes (ptr + len + capacity). `Box<str>` is 16 bytes (ptr + len).

**Sites:**
- `file_meta.rs:15-17` — `pub lines: Vec<String>`, `pub fn_names: Vec<String>`, `pub impl_targets: Vec<String>`

**Change:**
```rust
// Before
pub fn_names: Vec<String>,  // 24 bytes per entry
// After
pub fn_names: Vec<Box<str>>, // 16 bytes per entry — 33% smaller
```

**Impact:** 33% memory reduction for stored string collections. ~3.6MB cache
savings for a 377-file repo.
**Effort:** 30 min
**Risk:** Low — `Box<str>` derefs to `&str`, most callers work unchanged. Need
to update construction sites (`x.to_string()` → `x.into_boxed_str()`).
**Verification:** All tests pass.

---

## Phase 2: Structural Memory Optimizations (3.5 hours)

### R2: Arc-slice for file_meta lines (eliminates 226K+ String allocations)

**Current:** `file_meta::compute_from_content` does:
```rust
let lines: Vec<String> = content.lines().map(|s| s.to_string()).collect();
```
This allocates a new `String` per line (600+ allocations for a 600-line file).

**Proposed:** Store content as `Arc<str>`, lines as `Vec<(u32, u32)>` (byte
ranges). Access via `&content_arc[start..end]`.

```rust
pub struct FileMeta {
    pub content: Arc<str>,              // single allocation for full file
    pub line_ranges: Vec<(u32, u32)>,   // byte offsets into content
    pub fn_names: Vec<Box<str>>,        // only non-empty fn names
    pub impl_targets: Vec<Box<str>>,
    pub arities: Vec<u16>,
    pub brace_graph: Arc<BraceNode>,
}

impl FileMeta {
    pub fn line(&self, idx: usize) -> &str {
        let (start, end) = self.line_ranges[idx];
        &self.content[start as usize..end as usize]
    }
    pub fn lines(&self) -> impl Iterator<Item = &str> {
        self.line_ranges.iter().map(move |(s, e)| &self.content[*s as usize..*e as usize])
    }
}
```

**Impact:** 600 fewer allocations per file_meta cache miss. For 377 files:
226,200 fewer allocations during cold cache warming. Memory per FileMeta drops
from ~15KB to ~5KB.
**Effort:** 2 hours
**Risk:** Medium — struct layout changes. All callers that do `meta.lines[idx]`
must change to `meta.line(idx)`. Need to audit ~20 call sites.
**Verification:** All tests pass. File_meta cache hit rate unchanged.

### R7: `String::with_capacity` for known-size string construction

**Current:** Many `String::new()` + `.push_str()` patterns cause reallocation
as the string grows (2x growth factor = 3-4 reallocs for a 20-char string).

**Sites:** type_flow.rs (30 `.to_string()` + 6 `format!`), callgraph_v2.rs
(40 allocations), mcp.rs (162 allocations).

**Change:** Add capacity hints where size is estimable:
```rust
// Before
let mut result = String::new();
result.push_str("foo");
result.push_str(&bar);
// After
let mut result = String::with_capacity(bar.len() + 8);
result.push_str("foo");
result.push_str(&bar);
```

For `format!()` calls, replace with `write!` into pre-allocated String where
the pattern is in a hot loop.

**Impact:** 2-3x on string construction (eliminates realloc). Marginal per
call but compound across 100s of tool calls.
**Effort:** 1 hour
**Risk:** None.
**Verification:** All tests pass.

### A4: `f32` quantization for BM25 scores (2x memory bandwidth)

**Current:** BM25 scores are `f64` (8 bytes). `f32` (4 bytes) is sufficient for
ranking — the difference between f64 and f32 is ~7 decimal digits, far beyond
what's needed for sorting search results.

**Sites:**
- `search.rs` — `SearchResult.score: f64`
- `symbol.rs` — `IdfTable.weights: FxHashMap<i64, f64>`
- `symbol.rs` — `compute_corpus_mean` returns `FxHashMap<i64, f64>`

**Change:** Replace `f64` with `f32` in score computation and storage.

**Impact:** 2x memory throughput for score arrays. Halves cache line usage for
large result sets.
**Effort:** 30 min
**Risk:** Low — scores are only used for ranking, not display. f32 precision
is sufficient.
**Verification:** All tests pass. Search result ordering unchanged.

---

## Phase 3: Pure-Math Acceleration (5.5 hours)

### A1: SWAR byte classification (4-8x on hot inner loop)

**Current:** `scan_delimiters` and `scan_identifiers` process one byte at a
time. For a 60-byte line, that's 60 iterations.

**Proposed:** Process 8 bytes at once using `u64` arithmetic (SWAR — SIMD
Within A Register). The identifier check `is_ascii_alphanumeric || == b'_'`
becomes a bitwise expression:

```rust
// Check if 8 bytes are all identifier chars in one operation
fn is_ident_chunk(chunk: u64) -> u64 {
    // ASCII alphanumeric: 0-9 (0x30-0x39), A-Z (0x41-0x5A), a-z (0x61-0x7A), _ (0x5F)
    // SWAR: subtract and compare
    let lower = chunk | 0x2020202020202020; // lowercase
    let num = (chunk.wrapping_sub(0x3030303030303030) & 0x8080808080808080);
    let alpha = (lower.wrapping_sub(0x6161616161616161) & 0x8080808080808080);
    // ... full SWAR expression
}
```

For lines < 8 bytes (common for `}`, `};`, `// comment`), skip SWAR and use
the existing byte loop.

**Impact:** 4-8x on the hot inner loop. The inner loop is the hottest code in
the entire codebase (called per-line during indexing, per-candidate during
queries).
**Effort:** 3 hours
**Risk:** Medium — SWAR is tricky to get right. Need extensive testing.
**Verification:** All 115 structural tests pass. Add new SWAR-specific tests.

### A2: Bloom filter for keyword detection (eliminates 200-element linear scan)

**Current:** `is_keyword()` does a linear scan through ~200 keywords for EVERY
token. Called per-token during indexing.

**Proposed:** 256-byte Bloom filter (2048 bits). One hash + one bit test
eliminates 95% of non-keywords in O(1). Remaining 5% fall through to linear
scan.

```rust
static KEYWORD_BLOOM: [u8; 256] = {
    // Pre-computed at compile time from keyword list
    // Each keyword sets 3 bits via 3 hash functions
    ...
};

fn is_keyword_bloom(token: &str) -> bool {
    let h = fxhash(token);
    let bit1 = (h & 0x7FF) as usize;
    let bit2 = ((h >> 11) & 0x7FF) as usize;
    let bit3 = ((h >> 22) & 0x7FF) as usize;
    if !(KEYWORD_BLOOM[bit1 / 8] & (1 << (bit1 % 8))) { return false; }
    if !(KEYWORD_BLOOM[bit2 / 8] & (1 << (bit2 % 8))) { return false; }
    if !(KEYWORD_BLOOM[bit3 / 8] & (1 << (bit3 % 8))) { return false; }
    // Fall through to exact check
    KEYWORDS.contains(token)
}
```

**Impact:** Eliminates 95% of keyword checks in O(1). For 100K tokens, saves
~95K x 200 comparisons = 19M comparisons.
**Effort:** 1 hour
**Risk:** Low — Bloom filter is additive (false positives fall through to
exact check).
**Verification:** All tests pass. Add Bloom filter test.

### A3: CSR (Compressed Sparse Row) vectors for cosine similarity (3-5x)

**Current:** `cosine()` in symbol.rs iterates two `FxHashMap<i64, u32>` doing
hash lookups for every key. Random memory access, cache-unfriendly.

**Proposed:** Store bag-of-words as CSR format:
- `keys: Vec<i64>` — sorted phrase_ids
- `values: Vec<u32>` — parallel counts
- Dot product = linear merge-join of two sorted arrays

```rust
pub struct SparseVec {
    pub keys: Vec<i64>,    // sorted
    pub values: Vec<u32>,  // parallel to keys
}

pub fn cosine_csr(a: &SparseVec, b: &SparseVec) -> f32 {
    let mut dot = 0u64;
    let mut i = 0;
    let mut j = 0;
    while i < a.keys.len() && j < b.keys.len() {
        if a.keys[i] == b.keys[j] {
            dot += (a.values[i] as u64) * (b.values[j] as u64);
            i += 1; j += 1;
        } else if a.keys[i] < b.keys[j] {
            i += 1;
        } else {
            j += 1;
        }
    }
    // normalize by magnitudes...
}
```

**Impact:** 3-5x on similarity computation. Cache-friendly sequential access
instead of random hash lookups. Zero hashing in the hot loop.
**Effort:** 4 hours
**Risk:** Medium — requires converting all bag_cache sites from FxHashMap to
SparseVec. ~15 call sites in symbol.rs.
**Verification:** All tests pass. Cosine results identical (within f32
precision).

### A5: String interning (eliminates string comparison in hot loops)

**Current:** Identifier strings are compared via `==` on `String` / `&str`.
For repeated identifiers (same function name across 50 call sites), this is
redundant byte comparison.

**Proposed:** Intern all identifiers to integer IDs at index time. Compare via
integer equality (1 instruction vs O(n) memcmp).

```rust
static INTERNER: OnceLock<Mutex<StringInterner>> = OnceLock::new();

pub struct StringInterner {
    map: AHashMap<String, u32>,
    strings: Vec<Box<str>>,
}

impl StringInterner {
    pub fn intern(&mut self, s: &str) -> u32 {
        if let Some(&id) = self.map.get(s) { return id; }
        let id = self.strings.len() as u32;
        self.map.insert(s.to_string(), id);
        self.strings.push(s.into_boxed_str());
        id
    }
    pub fn lookup(&self, id: u32) -> &str { &self.strings[id as usize] }
}
```

**Impact:** Eliminates string comparison in hot loops. `phrase_id` is already
a form of interning — this extends it to all identifier operations.
**Effort:** 4 hours
**Risk:** Medium — requires threading interner through index/search paths.
**Verification:** All tests pass.

---

## Phase 4: Set Operations + SQL Optimizations (2.5 hours)

### B1: Bit-set for file ID filtering (10-50x on set operations)

**Current:** `needed_ids` is a `HashSet<i64>` that's intersected with file_map
results. Each intersection is O(N) hash lookups.

**Proposed:** Bit-set (1 bit per file_id). AND/OR/NOT become single
instruction per 64 files.

```rust
pub struct BitSet {
    bits: Vec<u64>,
    max_id: usize,
}

impl BitSet {
    pub fn new(max_id: usize) -> Self {
        Self { bits: vec![0u64; (max_id + 64) / 64], max_id }
    }
    pub fn set(&mut self, id: usize) {
        self.bits[id / 64] |= 1u64 << (id % 64);
    }
    pub fn contains(&self, id: usize) -> bool {
        self.bits[id / 64] & (1u64 << (id % 64)) != 0
    }
    pub fn intersect(&self, other: &BitSet) -> BitSet {
        // SIMD-friendly: each u64 AND is one instruction
        let mut bits = vec![0u64; self.bits.len()];
        for i in 0..bits.len() {
            bits[i] = self.bits[i] & other.bits[i];
        }
        BitSet { bits, max_id: self.max_id.min(other.max_id) }
    }
}
```

**Sites:** `search.rs` needed_ids, `lazy_occurrence.rs` file_id sets.
**Impact:** 10-50x on set operations. AND of two 600-element sets: 10 u64
instructions vs 600 hash lookups.
**Effort:** 1 hour
**Risk:** Low.
**Verification:** All tests pass.

### B2: Prefix-sum block lookup (O(1) instead of binary search)

**Current:** `block_id_at_line` does a SQL query or binary search over block
ranges.

**Proposed:** Precompute a flat `Vec<i64>` indexed by line number. `block_id
= blocks_by_line[line]`. O(1) lookup.

```rust
// During ensure_blocks_for_file:
let max_line = lines.len();
let mut blocks_by_line: Vec<i64> = vec![-1; max_line + 1];
for (block_id, start, end) in block_ranges {
    for line in start..=end {
        blocks_by_line[line as usize] = block_id;
    }
}
// Store in FileMeta or a side cache
```

**Impact:** O(1) block lookup. Used on every occurrence JIT build.
**Effort:** 30 min
**Risk:** Low — memory cost is `max_line * 8 bytes` per file. For a 600-line
file: 4.8KB. Acceptable.
**Verification:** All tests pass.

### B3: Hashbrown (SwissTable) (1.3-2x on all hash lookups)

**Current:** `FxHashMap` uses chaining with linked lists. Cache-unfriendly on
collision.

**Proposed:** `hashbrown::HashMap` uses open addressing (SwissTable). Better
cache locality — the hash table is a flat array, collisions are probed
linearly.

```toml
hashbrown = "0.15"
```

```rust
use hashbrown::HashMap as SwissMap;
```

**Impact:** 1.3-2x on hash lookups. Especially noticeable on large maps
(phrase_cache with 10K+ entries).
**Effort:** 1 hour (swap type at all FxHashMap sites)
**Risk:** Low — `hashbrown::HashMap` has the same API as `std::HashMap`.
**Verification:** All tests pass.

**Note:** This may conflict with `ahash` (R8). If both are applied, use
`ahash::AHashMap` which already uses hashbrown internally. In that case, B3
is subsumed by R8. **Decision: skip B3 if R8 is applied.**

---

## Phase 5: Signature + Branch Optimizations (2 hours)

### R5: `&[u8]` instead of `&str` for hot byte-scanning functions

**Current:** `scan_delimiters`, `scan_identifiers`, `classify_structural` take
`&str`. Rust's `str` has UTF-8 validation overhead on slicing.

**Proposed:** Change to `&[u8]`. Eliminates `.as_bytes()` call and makes
byte-level intent explicit. Enables future SWAR (A1) to work directly on the
input.

```rust
// Before
pub fn scan_delimiters(line: &str) -> LineDelimiters {
    let bytes = line.as_bytes();
// After
pub fn scan_delimiters(line: &[u8]) -> LineDelimiters {
    let bytes = line;
```

**Sites:** `scan_delimiters`, `scan_identifiers`, `classify_structural`,
`strip_line_comment`, `count_unmatched`.
**Impact:** Marginal — `.as_bytes()` is already zero-cost. Main value is
enabling A1 and eliminating UTF-8 boundary panic risk.
**Effort:** 1 hour
**Risk:** Low — callers already call `.as_bytes()` internally. Need to update
~10 call sites to pass `line.as_bytes()` instead of `line`.
**Verification:** All tests pass.

### R10: `#[cold]` on error paths (branch prediction improvement)

**Current:** Error handling branches in hot loops. Compiler can't know these
are rarely taken.

**Proposed:** Extract error paths to `#[cold]` functions:

```rust
#[cold]
#[inline(never)]
fn missing_file_error(fid: i64) -> ! {
    panic!("file_id {} not in file_map", fid);
}

// In hot loop:
let entry = match file_map.get(idx) {
    Some(e) => e,
    None => { continue; } // compiler knows this is cold if we hint it
};
```

Actually, the simplest approach is `#[cold]` on the `continue` path by
wrapping it:

```rust
#[cold]
fn skip_missing() {}
```

Or use `core::intrinsics::cold_path()` (unstable). The stable approach is to
mark the entire error handling function as `#[cold]`.

**Impact:** Improves branch prediction. CPU doesn't waste predictor entries on
cold paths. Marginal but compound.
**Effort:** 1 hour
**Risk:** None.
**Verification:** All tests pass.

### B4: Compile-time pattern matching (eliminate first-call latency)

**Current:** `OnceLock<Regex>` in `reliary-fix/lib.rs` — regex compiled on
first call. Also `is_error_line` patterns.

**Proposed:** Use `const` regex patterns where possible, or `lazy_static!`
with `once_cell::sync::Lazy` (compiled at first access but stored in static).

Actually, `OnceLock` already does this. The real win is replacing `Regex` with
manual byte scanning where the pattern is simple enough:

```rust
// Before
static RE: OnceLock<Regex> = OnceLock::new();
let re = RE.get_or_init(|| Regex::new(r"^Error:").unwrap());
if re.is_match(line) { ... }

// After (no regex needed for prefix check)
if line.starts_with("Error:") { ... }
```

Audit all `OnceLock<Regex>` sites and replace simple patterns with byte checks.
**Impact:** Eliminates regex compilation overhead. Marginal after first call
(regex is cached), but reduces binary size and cold-start latency.
**Effort:** 30 min
**Risk:** None.
**Verification:** All tests pass.

---

## Deferred (no immediate benefit)

### R3: `RwLock` for read-heavy caches

**Current:** All caches use `Mutex` — exclusive lock for both reads and writes.
MCP server is single-threaded (stdio), so no concurrent access.

**Proposed:** `parking_lot::RwLock` for read-heavy caches. Allows multiple
simultaneous readers.

**Decision:** Defer until MCP server goes multi-threaded. No benefit on
current single-threaded path.
**Effort:** 1 hour
**Risk:** None.

### R9: `ArrayVec` for fixed-size collections

**Current:** `SmallVec<[T; N]>` still allocates on heap when exceeding N.

**Proposed:** `ArrayVec<T, N>` never allocates — panics if exceeded.

**Decision:** Defer. `SmallVec` is sufficient. `ArrayVec` for `String` requires
careful Drop handling. The risk of panic on overflow outweighs the marginal
benefit.
**Effort:** 30 min
**Risk:** Low (but panic risk on overflow).

### B5: Iterative brace-graph traversal with prefetch

**Current:** Recursive tree walk in brace_graph and callgraph_v2. Each
recursion is a stack frame + potential cache miss on child node.

**Proposed:** Convert to iterative with explicit stack. Use
`prefetch_read_data` on child pointers before processing.

**Decision:** Defer. The recursive pattern is clear and correct. Converting to
iterative adds complexity for marginal cache benefit. Most brace-graphs are
shallow (<10 levels).
**Effort:** 2 hours
**Risk:** Medium — recursion → iteration conversion is error-prone.

---

## Execution Order

| Phase | Items | Effort | Risk |
|-------|-------|--------|------|
| **1** | R1, R8, R4, R6 | 1.25h | None |
| **2** | R2, R7, A4 | 3.5h | Low-Medium |
| **3** | A1, A2, A3, A5 | 12h | Medium |
| **4** | B1, B2, B3(skip if R8) | 2.5h | Low |
| **5** | R5, R10, B4 | 2h | None |
| **Deferred** | R3, R9, B5 | — | — |

**Total (excluding deferred): ~21.25 hours**

## Verification Plan

After each phase:
1. `cargo test --lib` — all tests pass
2. `cargo build --release` — clean compile
3. Run long bench (seeds 42, 17) — no score regression
4. Compare WC + wall time vs V17 baseline

**V17 baseline:**
- A score: 22.0 ± 1.4
- A WC: 135k
- A wall: 52s
- A tool bytes: 11.5k

**Target after V18:**
- A score: 22+ (no regression)
- A WC: <120k (-10%+)
- A wall: <45s (-15%+)
- A tool bytes: <10k (-15%+)

## No-Regression Guarantee

All changes fall into two categories:
1. **Drop-in replacements** (R1, R8, R4, R6, B3, B4) — same API, different
   implementation. Zero behavior change.
2. **Additive optimizations** (A1-A5, B1-B2, R2, R5, R7, R10) — new code
   paths that produce identical results faster. Old code can be kept as
   fallback if needed.

No logic changes, no algorithm changes, no data structure semantics changes.