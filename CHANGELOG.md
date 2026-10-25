# Changelog

All notable changes to Reliary will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.9.0] - 2026-10-24

Everything below 0.8.16 was measured after 0.8.16 shipped. The headline is a
correction, not a feature: **the tool's advantage is precision and output size,
not recall.** Full methodology and reproduction in `bench/RECALL_BENCH_P0.md`.

### Measured: recall is at parity with grep, precision is not

Head-to-head, tools driven over MCP with no model, scored against an
independent lexical oracle (comments and string literals blanked, whole-word
match). Symbols chosen by occurrence frequency, never by any tool's result.

| | reliai | codebase-memory-mcp | grep |
|---|---:|---:|---:|
| Python usage recall | **0.929** | 0.846 | 0.905 |
| Python usage precision | **0.957** | 0.668 | 0.824 |
| Rust usage recall | 0.905 | 0.867 | **0.920** |
| Rust usage precision | **0.988** | 0.694 | 0.780 |
| Definition hit (Rust) | **1.000** | 0.750 | — |
| Output bytes per answer | **1.4–1.8K** | 11.6–12.7K | 4.9–5.4K |

An index does not beat a text scan at finding occurrences. It returns far less
noise and far fewer bytes for the same recall.

### Fixed: a stray backtick was hiding whole files

The worst defect found was not a ranking problem. `tokenizable_code` reset its
lexer on every call, and the multi-line literal tracker had no bound, so:

- **one unpaired backtick in a Python comment opened a template literal that
  never closed**, masking every later definition in the file.
  `httpx/_urlparse.py` had been silently half-indexed;
- **every docstring line was tokenized as code**, so prose became call sites.
  `request` in `psf/requests` had 241 docstring hits, which was the entire
  Python precision gap (0.48 → 0.96).

Multi-line backtick literals are now abandoned after 50 lines (triple-quoted
strings are not bounded — Python docstrings are legitimately longer), and the
tokenizer carries its state across a file.

### Fixed: eight definition shapes were invisible

| shape | example | was |
|---|---|---|
| visibility qualifier with a paren | `pub(crate) struct FastRand {` | read as a signature (304 tokio sites) |
| empty body | `unsafe impl<T> Send for X<T> {}` | fell through every branch (478 sites) |
| name from a line-wide scan | `pub(super) fn throttle<T>(` | named `pub` |
| arrow in the signature | `fn f() -> impl Iterator<Item = u8>` | `->` broke angle depth |
| impl self-type | `impl From<E> for std::io::Error` | named `std` |
| lowercase type alias | `pub type uid_t = u32;` | required a capitalised name |
| bodyless trait method | `unsafe fn f(h: H) -> Self;` | required a `self` receiver |
| `unsafe impl` | `unsafe impl<T> Send for X<T>` | named `impl` |

Definition recall against the blind ctags oracle:

```
tokio     0.918 -> 0.9976   function 1.000  method 1.000  typedef 1.000
                            interface 1.000  struct 0.999  impl 0.989
requests  0.79  -> 0.9988
httpx       --  -> 1.0000
```

Every change has a mutation-verified negative control.

### Fixed: `list_methods` returned nothing for any language without braces

The type-body walk was brace-based, so a Python `class` never entered its own
body. `Session`, `Response`, `PreparedRequest` and `HTTPAdapter` all answered
"No methods found" while their `def`s were indexed and findable. Layout
languages now walk indentation. The competitor exposes no per-type method list
at all.

### Fixed: `build-all` reported rows inserted, not table totals

On an already-built index it printed `occurrence=0` while the table held 26,921
rows, which reads as data loss. It now prints the total plus the new-row count.

### Known limitation: homonyms cannot be separated

The index keys references by name, not by binding. 683 of tokio's 1,124
multi-definition names are declared twice in the same file, where `path_filter`
cannot help. For those, the tool returns all sites of the name — correct, but
not scoped to one definition. `path_filter` does separate the cross-file cases.
Documented in `bench/RECALL_BENCH_P0.md` rather than left implicit.

### Not proven

That any of this converts into better *fixing*. Four attempts tied at 100%.
The corpus analysis says why: the Rust compiler repairs missed call sites, and
the Python fix commits available are localizable in a couple of greps. The
claim is not made.

### Regression check

Comprehension benchmark re-run on this binary (condition A, 4 seeds):
F1 0.948–0.950, precision 0.985–0.990, recall 0.913, coverage 1.00, zero
refusals, zero dead-ends. No regression from the indexing changes.

## [0.8.16] - 2026-10-24

Fixes the CI failure 0.8.15 shipped with. The shared test module is compiled
separately into each test binary, and each binary calls a different subset of
it, so the unused half was dead code under the warnings-as-errors gate.
Allowing dead code in that one file is the fix; splitting it would recreate
the duplication it exists to remove.

## [0.8.15] - 2026-10-24

The oracle test file still carried its own copy of the ctags parser, so the
0.8.14 claim that both oracle tests share one was false, and CI failed with a
dead-code error naming the unused helper. It now delegates to the shared
module the same way the segmenter test does.

## [0.8.14] - 2026-10-24

- Both ctags oracle tests now share one parser (`crates/reliary-search/tests/common/mod.rs`) instead
  of duplicating it. The second copy had the same first-letter kind bug as the
  first, so CI failed twice for one root cause.
- The oracle forces the language by extension. Auto-detection returns nothing
  for the C fixture in Ubuntu's universal-ctags build, so `macros.c` had been
  contributing zero oracle lines.
- The segmenter test's hand-adjudicated table panicked on an unknown fixture
  rather than returning an empty vector: an empty list made every recall
  assertion vacuous, so adding a fixture silently disabled the gate.
- The ctags cross-check is a subset test. Adjudicated extras (body-less
  prototypes, a function-pointer variable, an operator overload) are printed
  rather than treated as errors; a ctags line missing from the table fails.

## [0.8.13] - 2026-10-24

Fixes a defect CI caught in 0.8.12.

- The ctags oracle harness mapped ctags kind names by their first letter.
  With short kinds, `field` becomes `f`, which is also `function`'s initial, so
  every struct field entered the oracle as a definition we had "missed". The
  harness now requests long kind names and matches on the whole word, drops
  `field` and `constant` (a field is a slot, not a definition) and keeps
  `member`, which is how ctags reports Python methods. A unit test pins the
  mapping.
- The oracle had been red since V84 and nobody saw it: ctags is not installed
  on the development host, and the test skips when the binary is absent. CI
  installs it, which is where it surfaced.

## [0.8.12] - 2026-10-23

Everything below was unreleased since 0.8.11: the cross-line lexer, the island
classifier, the V86/V87 definition-quality fixes, inline test detection, the
trait-question fixes, and a retraction of every comparison against
codebase-memory-mcp published before this release.



### Tests and trait questions

Three bugs that made shipped output wrong, all found by running the commands
rather than reading them.

- **`test-plan` could not see tests inside the changed file.** `reliary
  test-plan --files crates/reliary-search/src/structural.rs` answered "no tests
  found" for a file containing 79 `#[test]` functions; 652 inline tests across
  the workspace were invisible. Tests are now detected by shape — an attribute
  run (`#[test]`, `#[tokio::test]`, C#/Java `[Fact]`/`[Test]`, `@Test`), the
  `test_` name convention where there are no attributes, and the declaration
  slice so a one-line body still classifies — and reported per function with its
  line, which is what you actually run.
- **`find_references("Default", def_only)` answered with `fn default()`.** Two
  ordering bugs: the V87 comment guard treated every `#` line as a comment,
  which made the `#[derive(...)]` branch unreachable, and a trait name took the
  definition path before the implementor path. `Default` now returns the five
  structs that implement it.
- **The same fix had to be applied twice.** The first patch reached
  `reliary_find_references` but not `reliary_find_references_with_source`, which
  is the handler the benchmark and harness call, so measured behaviour did not
  change. The trait answer is now one shared function, and an end-to-end test
  compares both handlers' answers directly — a source-grep test passed with the
  bug reinstated, because the helper call count stayed high through the usage
  path.
- **A bare `find_references(Trait)` now includes implementors.** The tool
  description promises a call without mode flags returns all relevant knowledge;
  for a trait it did not, because `#[derive]` has no occurrence row for the
  struct.

`test-plan` also no longer reports "no tests" for a file it could not read: the
unreadable path is named on stderr, so "not checked" cannot look like "nothing
here".

### Definition quality: prose, control flow, comments

Three classes of phantom definition were still being served after the V86
index fixes. All were found by querying the index, not by reading code.

- **Documentation is no longer a definition source.** Fenced snippets and
  prose both match the shape rules, and no content test separates a plan
  document from source: 627 of 4,750 definition rows came from markdown, so
  `def_only("line")` answered with a README bullet. `.md`, `.mdx`, `.rst`,
  `.adoc` and `.txt` stay searchable but supply no definitions. An extension
  may exclude a decision, never admit one, so this is a deny-list.
- **`except`/`elif`/`finally` are control flow.** The Python colon classifier
  deliberately accepted `except Exception:` as a type definition (<0.1% of
  lines), which was fine as a fraction and bad as a symbol: `Exception` is a
  common query and it returned 153 `except` clauses. They join the leading-word
  guard the rest of control flow uses.
- **A line that begins inside a multi-line string is prose.** The classifier is
  handed raw text, so a Python docstring body was classified as code and
  `def_only("String")` answered with a doc line. `lex::starts_in_code` asks the
  shared lexer whether the line starts inside a literal; ingest, JIT, reindex
  and the warrant scan all consult it.
- **Comment lines are not declarations.** `find_trait_impls_scoped` matched a
  doc comment (`///   `pub name: String`   (struct-field fallback)`) through its
  C#/Kotlin `:` pattern and reported `String` as implemented by a comment.

Definition rows on this repository: 4,750 → 3,668, with zero markdown rows and
zero `except` rows. Search still finds documentation content.

Every gate here is mutation-verified. The trait-scan test also had to escape a
`tempfile::tempdir()` dot-directory (the walker skips dot paths), which had made
it pass vacuously.


### Correctness fixes from the project review

- **MCP server could be aborted by one query.** `reliary_query_ast` sliced a
  user pattern at byte 20 without checking a character boundary; a 3-byte
  character there panicked, and under `panic = "abort"` that killed the whole
  server — every tool gone for the session. The slice is now boundary-safe and
  a test drives the parser with multibyte input.
- **Deleted files left the index.** Nothing removed `file_map` or `file_stats`
  rows, so a deleted file stayed a search candidate and kept resolving
  definitions to a path that no longer existed (23,510 occurrence rows on this
  repository pointed at files that were gone). The watcher's delete path now
  removes every row, and `trust` prunes files that no longer exist — skipping
  paths whose parent directory is missing, so an unmounted volume cannot wipe
  an index.
- **Reindex could admit prose as source.** `reindex_file` never wrote
  `is_source`, so a file created after `trust` inherited the schema default of
  1 and a markdown file's fenced code became findable definitions. Reindex now
  computes the flag from content and refuses data/config formats outright.
- **An index built by older code was served silently.** `SCHEMA_VERSION` covers
  table shape, not contents, so an upgraded binary kept answering from the old
  extractor (which stored type names and locals as definitions).
  `trust` now records the indexer fingerprint in `meta`; `doctor` reports a
  mismatch and tells the user to rebuild.
- **The phrase index was process-global.** A single `OnceLock` held the first
  project's index path, so a server answering for a second project reranked
  against the first project's phrases. It is now keyed by DB path.
- **Six classifier false positives**, all found by reading the index the tools
  actually serve:
  - `let ones: Vec<i32> = {` was a type definition of `i32` (the value opens a
    brace, so the block path named the annotation).
  - `) -> String {` defined `String` — a signature continuation named the
    return type.
  - `text = ... else str(resp)` defined `str` — an assignment whose value
    contains a call.
  - `pub symbols_added: Vec<(String, String, i32)>,` defined `Vec` — the `(`
    inside an unclosed generic group was read as a parameter list.
  - `pub fn segment_lines<S: AsRef<str>>(...)` defined `Vec` — `>>` closes two
    generic groups and was treated as a shift operator, leaving the depth
    stuck.
  - `seen: &mut HashSet<...>| {` defined `i32` — a brace after a closure's
    closing `|` opens a closure body, not a type.

  Each produced phantom symbols that `find_references(..., def_only)` answered
  with, and type names are the worst case: they look plausible. After the
  fixes the index holds zero `i32`, `vec` or `segment` definitions.
- **Warrant: binary edits were invisible.** The scan stored `size:N` for
  binary files, so a same-size edit changed nothing and the tree digest
  disagreed with the identity pass. The manifest now always hashes content,
  with `scanned` carrying the separate meaning.
- **Empty qualified names** produced answers like `" is defined at a.rs:1"`;
  there is now a file-stem fallback.
- **A Rust field annotation read as an interface list.** `def_only` for `i32`
  answered "implemented by 6 types" citing `pub struct Thing { pub field: i32 }`:
  the C#/Kotlin `:` separator and a Rust field annotation are the same
  character. A `{` or `"` before the colon now disqualifies the line, so a
  primitive type reports no definition instead of six structs.

### Performance

- `search` reads document frequency from the in-memory phrase index instead of
  a full `phrases` scan per term, and computes the two BM25 aggregates in one
  query rather than two.
- Warrant only runs the brace scanner on lines whose definitions it needs.


### Warrant performance: cached base index, parallel scan, one read per file

- **Indexer fingerprint (mechanical cache safety).** A `build.rs` in
  `reliary-search` hashes the sources that determine stored rows (classifier,
  lexer, extractor, occurrence builder, schema, tokenizer, segment gating) and
  bakes the digest into the binary; it is mixed into every cache key. A
  forgotten `INDEXER_VERSION` bump can no longer serve rows produced by
  different code. Verified by editing a source file and observing the
  fingerprint change.
- **Head index cache.** The head side is now content-addressed too: an
  unchanged working tree reuses its index (1.6 s -> 0.1 ms).
- **Warrant derives definitions from the tree scan.** It used to build a
  SQLite index per tree and read 5,231 definition rows out of 158,326. The scan
  already reads and classifies every byte, so definitions are now produced in
  that pass, and no index is built unless `RELIARY_SEGMENTER=1` (whose island
  classifier the scan does not reproduce). Cost: cold 3.0 s -> 0.23 s, first
  run after an edit 2.9 s -> 0.11 s, cache on disk 28 MB -> 1.4 MB. Four
  contract shapes (rename, no_new_dead, signature, files) produce
  byte-identical certificates to the previous binary.
  Equivalence with the index is asserted against a real index, not assumed:
  5,272 = 5,272 on this repository and 3,675 = 3,675 on the canonical snapshot,
  with mutation-verified controls for the deny-list and UTF-8 admission gates.
  Cached scans without a `defs` field fail to parse and are re-scanned rather
  than reading as "no definitions", which would make every contract look
  closed.

- **Scan cache.** Scan results are cached per `(tree identity, needle set,
  effective excludes)`, so a repeat contract on the same tree skips the scan
  (0.2 s -> 2 ms per side). The identity pass itself no longer runs the lexer
  and reads files in parallel.
- **Exclusion handling was the hidden cost.** The gitignored-file list was
  canonicalized entry by entry — 37,000 syscalls per pass on this repository,
  more than the scan it protected. Excludes are now matched lexically as
  root-relative paths, and `git ls-files --directory` collapses ignored
  directories to one entry each (37,131 entries -> 395). The identity pass went
  from 560 ms to 34 ms. Excludes that the walker can never reach (a dot-dir,
  a `SKIP_DIRS` name) no longer participate, because they cannot change the
  scan and would otherwise invalidate the cache whenever an ignored build
  artefact appeared.
- **Base index cache.** `reliary warrant` keys a cached base index on content
  (`sha256(manifest digest, schema version, indexer version, classifier mode)`)
  and stores it under `.reliary/warrant/` (4 entries, 512 MB cap, LRU by
  mtime). Cache hit takes the base side from 1.43 s to 47 ms on this
  repository. `--no-cache` / `RELIARY_WARRANT_CACHE=0` bypass it.
  `INDEXER_VERSION` is bumped whenever anything that shapes stored rows
  changes, so a stale entry can never be served by newer code.
- **Index paths are now relativized** for warrant's indexes
  (`relativize_index`, `index_revision_relative`). Without this a cached index
  built at one root would report every base definition as removed when reused
  at another. Covered by a move-the-tree test with a negative control.
- **One read per file.** The tree scan used to open every file twice (binary
  sniff, then read); `is_likely_binary_bytes` reuses the buffer it already has.
- **Parallel scan**, width `min(cores, 4)`, `RELIARY_WARRANT_THREADS` to
  override. Streaming: peak memory is `workers x file size`, not repo size.
  Serial and parallel output are proven byte-identical by test.
- **Allocation removal** in the token scan: reused masked-line buffer,
  per-worker stem memo, `FxHashMap` occurrence counts.
- **Byte-identical guarantee held:** certificates compared across cache miss,
  cache hit, and `--no-cache` are identical; `RELIARY_WARRANT_TIMING=1` reports
  the phase split.
- No SIMD intrinsics: all crates are `#![forbid(unsafe_code)]`. `memchr` and
  LLVM auto-vectorization are used where the standard ecosystem provides them.

### Warrant v1 — closure certificates for changes

New `reliary warrant` answers the question that ends an agent session: *is
this change finished?* Given a base revision and the working tree, plus a
small JSON contract, it emits a canonical certificate with a three-valued
verdict (`closed` / `open` / `indeterminate`), source-backed counterexamples
(`symbol at file:line` plus the raw line and its hash), and an explicit
unseen region.

- **Raw-token closure, not stem closure.** A rename is closed when the old
  token appears nowhere in head. Index phrases are porter stems, so a
  stem-based check would report closed while `handled` remains after
  renaming `handling` — the falsifier pins this with a verified stem-collision
  pair.
- **Comments and strings never count.** The shared grammar-free lexer masks
  them before scanning, so a renamed symbol mentioned in a comment does not
  block closure.
- **Delta accounting is asymmetric.** Unclaimed definition removals open the
  verdict; unclaimed additions are warnings. A trivial contract cannot hide
  an undeclared deletion.
- **Canonical, replayable evidence.** Relative paths, sorted arrays, no
  timestamps; the certificate is byte-identical across runs (tested).
- **Falsifier passes.** 51 mutations (complete/incomplete renames, 25 symbols
  plus a stem-collision case) on a generated 3-module crate, oracle
  `cargo check`: violation precision 1.0, silent misses 0, false positives on
  complete renames 0. The harness itself is mutation-verified: switching the
  implementation to stem-based closure makes it fail.
- Exit codes: 0 closed, 1 open, 3 indeterminate, 2 usage.

Contract: `{"version":1,"conditions":[{"kind":"rename","from":"a","to":"b"}]}`
with optional `remove`, `add`, `signature`, `no_new_dead`, `files`, `scope`,
`allow_unseen`, `strict_unseen`. Plan: `docs/plans/V85_WARRANT.md`.

### Measurement: the benchmark was breaking the comparison

Every published comparison against codebase-memory-mcp before 2026-10-23 is
retracted.

- **The harness called its tracer with the wrong parameter.** `trace_path`
  requires `function_name`; we sent `name`. The CLI prints its complaint to
  stderr, exits 0 and writes nothing to stdout, so the wrapper raised and handed
  the model `(altbackend error: Expecting value: line 1 column 1)`. That
  silently disabled caller/callee tracing on four of the ten questions (q10
  4/4, q5 3/4, q2 2/4, q4 1/4) and affected every tape recorded since
  2026-06-30 — 35%, 42% and 25% of its answers in canonical-v1, v4 and v8.
  The published "F1 0.298" and "clearly ahead of codebase-memory-mcp" are void.
- Every wrapper now builds its payload through a guard that raises when a
  server-required key is missing, and a test compares our required-key map
  against the installed server's own `tools/list`.
- **Recall could double-count.** The caller-list exception matched on location
  alone, so one cited line could satisfy several ground-truth facts in the same
  file. Each claim now satisfies at most one fact, and `recall <= 1.0` is
  asserted. Disclosure: the published F1 was not affected by this; the check
  over all 120 recorded answers maxes at 1.0 either way.
- **Refusals are now counted.** "I could not determine X" is a different
  failure from "X is wrong", and a backend that declines is cheap. Reported
  next to the score, never folded into it.

Re-measured on a snapshot of this repository, 10 questions, 4 seeds,
claim-weighted:

| | F1 | billed | tool bytes | dead-ends | refusals |
|---|---|---|---|---|---|
| reliary | 0.948 | 22,925 | 10.5K | 0.0 | 0/40 |
| codebase-memory-mcp | 0.495 | 21,901 | 11.1K | 2.0 | 4/40 |
| grep | 0.596 | 35,456 | 23.7K | 0.0 | 1/40 |

A foreign corpus (tokio) with questions generated from the index, and the same
tree with every identifier and filename pseudonymised:

| | tokio F1 | obfuscated F1 |
|---|---|---|
| reliary | 0.783 | 0.711 |
| grep | 0.642 | 0.526 |

The lead is larger when the names carry no meaning, so it is not name
recognition. Four questions and two seeds: a direction, not an interval.

### Dead code, and two output excesses

- **`#[test]` functions were counted as dead.** The exclusion was documented but
  never implemented, so a module-scoped query returned 191 items whose first 12
  were all tests, and the real answer never reached the caller. Two shapes are
  now excluded: a test attribute on the preceding attribute run, and the `test_`
  naming convention.
- **Dead results were sorted worst-first** by descending path length, so the
  caller's limit truncated away the exported functions the question asks about.
  Ranking is now exported-first, then shortest path, then line — every key a
  function of content.
- **Every exported dead item is listed** and private helpers are summarised by
  count. A flat present-cap of 12 was tried and reverted: it pushed the exported
  functions below the cut and recall on the dead-code question went to 0.
- **`describe(context="callers")` re-stated the definition and the pack body.**
  The caller already has both from the call it made first, and paid for the
  duplicate on every later turn: 1,312 → 306 bytes.
- **`find_dead_code` printed every item the limit allowed**, and the model
  re-emitted all of them at four times the input weight.

### Reliability

- The pre-commit hook aborted with a `grep` usage dump when a file named
  `--help` appeared in the working tree. It now passes `--`.
- Several end-to-end assertions were vacuous: two grepped source text for a
  constant name and passed with the fix removed, and two fixtures never
  exercised the code path they claimed to. All four now fail when the guard is
  removed, verified by making the edit.

## [0.8.11] - 2026-10-16

### Hardening (security review F2-F8)

- **MCP stdin 10MB line cap (F2).** `serve_stdio` no longer uses unbounded
  `lines()` — a single 1GB line OOMed the server. Oversize input is rejected
  with `-32602` and framing resyncs (verified: next message answered).
- **Self-update staging (F3).** `tempfile::TempDir` (unpredictable name, no
  pre-create `remove_dir_all` symlink race) + `tar --no-absolute-filenames
  --no-same-owner` (defense in depth behind the digest gate).
- **`risk`/`fix`/`dead` scope to the session root (F4).** Hidden dispatch
  tools pinned `safe_path(…, ".")` (server CWD); now `active_root()` like
  every other tool. Verified: server in `/tmp` serves `path="$T"` hints.
- **`read_tee` gated + wired (F5).** Basename-only, hex-only, 10MB-capped;
  `cache-retrieve` falls back to the tee file on DB miss (was dead code).
- **`fix --verify` filename escaped (F6).** Substituted `{file}` is
  single-quote wrapped (malicious repo filenames can't inject).
- **`gate.js` binary trust (F7).** Ownership + X_OK + regular-file checks,
  parity with the sh hook (was `existsSync` only).
- **`logs` 10MB cap + `status` read-only open (F8).**

### UX/DX (P1-P4)

- **Single color gate.** `NO_COLOR` + `CLICOLOR`/`CLICOLOR_FORCE` + `TERM=dumb`
  + stderr-TTY, honored by every command. Piped output is never colored.
  Progress and startup lines go to stderr; the fake spinner is TTY-gated.
- **Fail-closed prompts + `--yes`.** `init`/`clean`/index-offer on closed
  stdin exit 2 with remediation instead of auto-accepting. Scripted piped
  answers still work; only immediate EOF refuses. `search` with no index
  exits 2 (was a silent token fallback + exit 0); `pack`/`compress` errors
  exit non-zero.
- **`update` exit codes.** 0 up-to-date/updated, 1 error, 2 available (with
  `--check`). Previously always 0.
- **`trust` vs `index` clarified (kept separate).** `trust` = first-time
  setup; `index` = rebuild. All hints agree. `status` gains `--path`;
  `doctor --fix` rebuilds via `trust` and writes a missing `.gitignore`;
  `man` writes `reliary.1`; `logs --tail` conflicts with `--level`.
- **MCP honesty.** Escaping `path_filter` returns an explicit scoped-empty
  message (never a silent whole-codebase dump). `list_methods`/`verify`
  schemas document `path`/`tol`; unknown-tool errors list valid names;
  empty-call-graph hint points to `def_only` (not deprecated `goto_def`).
- **`doctor` no false-green.** Binary checks PATH resolvability; unknown
  `RELIARY_MODE` fails; `claude` hooks-skipped is not a failure; `ignore`
  checks content + deeper walk and is fixed by `--fix`.
- **`init --dry-run` honest.** Counts previewed integrations instead of
  always printing "No agents were configured".

### Security

- **`.reliary/` is now git-ignored automatically.** `trust` (and the MCP auto-trust path) write a self-ignoring `.reliary/.gitignore` containing `*`, so `git add .` can no longer stage `index.sqlite` (source-derived, multi-MB) or `cache.sqlite` (which stores the original text of every command passed through `reliary wrap` — routinely API keys and env dumps). Existing installs get the file on the next `trust` without touching any tracked file, and a user-authored `.reliary/.gitignore` is never overwritten. `doctor` reports if the guard is missing.
- **`--internal-unlink` was an arbitrary-file-deletion primitive.** The private Windows self-update janitor mode was reachable from any command line (`reliary status --internal-unlink /etc/hosts`) and deleted whatever path it was given. It now refuses any file that is not a `*.old` self-update leftover.

### Fixed

- **`reliary trust` aborted on a file containing a bare `impl<`.** `extract_impl_type` walked generics looking for the closing `>` without checking that one existed, then sliced past the end of the string and panicked — aborting the whole trust run and every later query on that repo. It now bails on unbalanced generics.
- **Sift compression corrupted diffs and JSON.** The "drop any line ≤ 2 chars" rule removed structurally significant short lines — a lone `}`, a diff `+}`, `);`, `});` — so compressed output no longer applied or parsed. Short lines carrying punctuation are now kept.
- **`reliary wrap bash -c '<source reader>'` dropped a trailing line.** The passthrough detection failed to recognise the `bash -c` wrapper the shipped hook produces, so `wrap bash -c 'cat file.rs'` compressed a source read and lost a closing brace. The inner command is now detected and passed through.
- **`reliary verify`, `pack` and `bench gen` returned wrong/empty results on a freshly trusted project.** All three read the occurrence table, which `trust` leaves empty (lazy index). Each now builds it once when empty.
- **`reliary-memory::recall` / `encode_query` returned a uniform 1.0 similarity.** Query encoding bundled token vectors via `bundle_into` from an all-`-1` accumulator, which keeps the accumulator's bit on ties, so every query matched every memory equally. Now uses the same deterministic majority vote as `encode_tokens`; `recall`/`predict` discriminate.
- **`doctor` always reported the index as "0d old".** It queried a non-existent `file_map.mtime` column and `unwrap_or(0.0)`-ed the error, so the stale-index warning was dead code. Now uses the index file's own mtime.
- **macOS and `XDG_CONFIG_HOME` OpenCode installs were reported unwired.** `opencode_config_dir` used `~/Library/Application Support/opencode` on macOS, but OpenCode resolves global config as `XDG_CONFIG_HOME || ~/.config/opencode` on every platform — so a correctly wired install read as "not found". Both `init` and `doctor` now use the same resolution.
- **The Claude Code hook could break interactive pagers and dropped sibling tool-input fields.** It rewrote `less`/`more`/`bat` (interactive TTY pagers) into a pipeline that cannot serve a TTY, and its `updatedInput` replaced the whole Bash input, discarding `timeout`/`description`/`run_in_background`. Pagers are no longer rewritten and the original input object is preserved.
- **The shipped `opencode-reliary-sift.js` hook was dead code** that referenced a removed API, was never installed by `init`, and contradicted the docs. Removed; the working OpenCode plugin (`opencode-plugin/`) is what `init` installs.
- **Content-cache keys were 64-bit SipHash with `INSERT OR REPLACE`.** A collision would silently overwrite another entry's stored content. Now SHA-256.
- **`safe_read` blocked forever on a FIFO or character device.** A named pipe or `/dev/zero` reports size 0, passed the size cap, and then blocked in `read_to_string`. Only regular files are read now.
- **CLI commands run from a subdirectory created a nested index instead of using the project's.** `search`, `risk`, `classify`, `vacuum`, `build-occurrences`, `build-all`, `reindex-file`, `who-calls`, `map`, `test-plan`, `impact`, `verify` and `bench` each built `<path>/.reliary` directly with no walk-up (and `reindex-file`/`who-calls` accepted a 0-byte stub). They now share one resolver that walks up to the nearest *usable* index, skipping stubs. A relative start (the default `--path .`) is absolutized first — `Path::pop()` on `.` returned false, so the walk-up never climbed.
- **Several commands printed an error but exited 0**, so CI could not distinguish failure: `risk`, `vacuum`, `build-all`, `build-occurrences`, `reindex-file` (including a non-empty file that indexed 0 tokens), `who-calls`, `logs`, `session-state`, `cache-store`, `cache-stats`. All now exit non-zero.
- **`vacuum`/`build-*` opened the DB without WAL or `busy_timeout`**, so they could fail immediately with `SQLITE_BUSY` under the MCP watcher.
- **A nested `features` object in the config silently discarded the whole file.** `read_config_file` deserialized into `HashMap<String, String>`, so `{"mode":"fast","features":{"compress":false}}` failed to parse and `unwrap_or_default()` dropped everything, including `mode`. Nested objects are now flattened to dotted keys so both shapes work; `validate_config` accepts the flat `features.*` keys `set_config` writes; and `reliary config` reads the project config by default.
- **The MCP server answered notifications.** JSON-RPC messages with a `method` and no `id` (`initialize`/`ping`/`tools/call` sent as notifications) are no longer replied to, and a malformed JSON line is now answered with `-32700` instead of being silently dropped.
- **`sift --llm` was a no-op** — `compress_unified` hardcoded LLM mode; `RELIARY_SIFT_LLM=0` now selects the human-readable form.
- **`reliary-dead` never returned `High` confidence** (it required an ALL-CAPS name, but names are lowercased at extraction). Exported-but-unused symbols are now `High`; the CLI `dead` and MCP `find_dead_code` agree better.
- **A multibyte symbol name crashed (aborted) the whole MCP server.** `closest_symbols` sliced the name at a byte index without a char-boundary check, so `reliary_find_references(name="café")` hit `end byte index 4 is not a char boundary` and — with `panic = "abort"` — killed every client session. Also fixed two more reachable panics: `similar`'s anchor slice (a stale occurrence line past EOF) and the pack `manual_capture_sentinel` fixed-offset slice.
- **`reliary fix --dry-run` wrote to files.** The deterministic recipes ignored `dry_run` and wrote unconditionally, so a "safe preview" silently rewrote source. Dry-run is now threaded through every recipe, which previews without writing. `fix` flag parsing also no longer leaks `--flag` values into the task text and honors `--dry-run`/`--json`/`--verify` anywhere on the line.
- **`trust`/`index` reported success (exit 0) when the index build failed.** `run_index` returned `()`, so a database-open or ingest failure still printed "✓ Project trusted". Both now exit 1.
- **`reliary_find_dead_code` returned "0 dead items" on a freshly trusted project.** Deadness is read from the occurrence table, which `trust` does not populate (lazy index) — indistinguishable from a clean repo. The tool now builds the table once when empty.
- **`phrase_id_for` returned `None` for a valid stemmed phrase before its occurrence rows existed**, making symbols look absent on a fresh lazy index before JIT could run.
- **`apply_edit` with an empty/whitespace-only `old` prepended `new` to the file and reported success** (`content.find("") == Some(0)`) — a malformed LLM edit silently corrupted the file. Now refused.
- **`fix` E0252 grouped-import repair emitted `use use foo::{b}`** and dropped siblings whose name merely ended with the duplicate. Now removes only the exact duplicated segment.
- **MCP parameters declared but ignored:** `reliary_search.limit` (hardcoded 10), `reliary_call_graph.depth`, `reliary_find_dead_code.format` (and its default limit was 100 vs the advertised 30), `reliary_describe.file`. `reliary_brace_debug` also read an arbitrary path with no traversal check — now validated.
- **`parse_pack_entries` used `strip_prefix("///")` on `## ` headers**, so entry names were `"## skeleton"` and cross-reference expansion could never match a name.
- **`search -f json` printed "No results found." instead of `[]`** for an empty result set.
- **`impact`/`find_definition` reported "no definition found" for C#/Java-style type declarations.** A type whose declaration line ends in neither `{`, `(`, nor `:` — `public class Foo : Bar` (base list), `public interface IFoo`, `public sealed class Foo` with the brace on the next line, `class Foo<T> where T : X` (constraints) — was never tagged as a definition, so it was invisible to `impact`, `find_definition`, `call_graph`, and qualified names even though the file was indexed. These are the dominant .NET/Java forms. The classifier now recognises a PascalCase identifier preceded by a type keyword (`class`/`interface`/`struct`/`record`/`enum`/`type`/…) as a type definition when no delimiter terminator is present, and `find_definition` accepts a body-less type anchor. Grammar-free and word-boundary-checked, so `Datatype`/`prototype`/`subclass` and method-body locals are not misread.
- **`verify` was hardcoded to `.rs`.** Its claim regexes only matched `file.rs:line`, so on a `.cs`/`.java`/`.ts`/`.go`/`.py` corpus `verify` silently extracted zero claims and returned no verdicts. All six claim forms now recognise the same source-extension set as the Python verifier (`rs py go ts tsx js jsx c cc cpp h hpp java rb php cs kt swift scala …`), and non-source citations (`README.md:12`) are still rejected.
- **`test-plan` missed the `<Subject>Tests.<ext>` convention.** `is_test_path` recognised `_test`,`tests/`,`.test.` and `tests.rs`, but not `FooTests.cs` / `FooTest.java` / `FooTests.kt` / `FooSpec.scala` — the standard C#/Java/Kotlin/Scala test naming — so a changed class with a matching `…Tests.cs` returned "no tests found". The PascalCase `Test(s)`/`Spec(s)` suffix is now recognised, with word-boundary guards so `latest`, `contest`, `Manifest`, `Prospect` are not misread as tests.
- **Declaration-only source files were rejected at ingest.** `is_source_like` counted `public class …`/`interface …`/`record …` lines as non-code (the code-keyword list only had bare `class `/`enum `), so a file of interfaces, DTOs, or enums scored a 0.0 code ratio and was skipped as "not source" — every symbol in it silently unsearchable. Visibility modifiers and the C#/Java declaration keywords now count as code signals.
- **`list_methods`, `describe(methods)` and `find_references(methods)` returned nothing on non-Rust types.** They were hard-filtered to `%.rs` files and only understood Rust `impl` blocks. Added a language-general type-body member scanner (Rust/Go `name: Type` and C#/Java `Type name;` fields, method definitions via the shared classifier) and dropped the `.rs` filters.
- **`call_graph` returned no callees on C#/Java/TS/Python.** Callee extraction deliberately dropped every `receiver.Method()` call as a "std method"; in those languages almost every call is dotted, so callees were always empty. Method declarations preceded by a return type (`public async Task<X> Name(`) are now recognised as definitions.
- **`find_references(usage_only)` for an interface/trait name only matched Rust `impl T for X`/`#[derive]`.** Added C# `: IBar` and Java `implements IBar` detection (grammar-free, word-boundary and class-keyword guarded).
- **Anchored queries could return empty: `block_id_at` and `best_context_key` double-converted the line.** Both subtracted 1 from an `anchor_line` that callers already pass 0-indexed, so the anchor block/context resolved one line early — often to nothing. Both now use the value as-is.
- **The per-thread block-comment flag leaked between files.** A file ending in an unterminated `/*` carried the open-comment state into the next file on the same Rayon worker, zeroing its brace counts and making indexing scheduling-dependent. Reset at every per-file scan.
- **The `file_meta`/brace-graph caches were never invalidated on edit.** A long-lived MCP server served pre-edit lines/fn_names/brace tree forever, so dead-code, callgraph and qualified names reflected the old file. `reindex` now invalidates the edited file.
- **`verify` could panic the server.** A client-supplied huge/negative `tol` or an absurd line overflowed `line - 1 - tol` (remote DoS). Clamped. The file-line bound was `content_len/8 + 1000` (~8× too generous, so `foo.rs:5000` verified in a small file); now a sound `line <= content_len`. The basename lookup now escapes LIKE metacharacters and breaks ties deterministically.
- **The v4→v5 migration created `file_phrases` empty and swallowed errors.** It is now backfilled from the existing `phrase_occ` blobs, so a reindex of an upgraded index strips old phrase entries instead of accumulating duplicates.
- **`reindex` could leave a transaction open on a block-build error** (poisoning later JIT builds on the shared connection) — added an RAII rollback guard.
- **Silent errors in `search`/`map`.** Prepare/query failures returned an empty result indistinguishably from "no matches"; now logged. A failed `bump_index_gen` after a successful index left the freshness stamp stale; now surfaced.
- **Determinism: `reliary_similar` used `ORDER BY RANDOM()`** — now `ORDER BY file_path, line`.
- **The watcher resolved the wrong project root in a monorepo** (preferred `.git` over an existing trusted `.reliary/`), so package edits were silently never reindexed.
- **`build_callers` used a Rust-only test-path check**, so C#/Java `FooTests.cs` callers were reported as production callers.
- **`find_references` with `path_filter` printed blank code evidence** (looked up the file by basename while the cache is keyed by full path).
- **The legacy `reliary_callgraph` handler anchored one line early** (pre-subtracted `anchor_line` while `build_call_graph` expects it 1-indexed), unlike its `_v2` sibling.
- **The index resolver accepted schema versions the opener rejects** (v1–v3), yielding "no index" with an index present.
- **`pack`, `diff` and `architecture` were still JS/TS/Rust-shaped and degraded on .NET.** `pack`'s `is_source_file` had no `cs` extension, so a .NET repo packed "(0 symbols)" and a mixed repo silently dropped its whole C# backend; it also re-derived definitions with its own Rust/JS/TS heuristic instead of the shared classifier. `derive_crate_name` used a generic parent directory (`src`) as the label, producing headers like `Foo/src`. A real `diff` on a C# commit reported 0 symbol changes and could hang on repos with generated code; it now skips build-output directories (`obj/` and other ecosystem caches) and reports the real change. `architecture` queried the removed `method_occurrence` table and failed outright; it now derives the count from `occurrence`, and detects C#/Kotlin/Swift/etc.
- **`namespace App {` was tagged as a type definition named `App`**, so every C#/C++ namespace became a bogus symbol. A namespace is a scope container, not a symbol. Method declarations with a return type and camelCase name (`public int handleRequest(`) are now recognised for Java/Go/Kotlin, with call guards intact.

## [0.8.10] - 2026-10-15

### Fixed

- **MCP tools reported "no index" whenever the server was not started in the indexed project.** The CLI resolved the index from the `path` argument, but the MCP server resolved it from its own process working directory (cached once at startup) and ignored `path` entirely. With a global MCP config — or any client that does not set the spawn directory — every symbol tool failed even though the project was indexed. Both now share one resolver: `path` argument → `RELIARY_ROOT` → the client's advertised workspace root (`roots/list`) → the server's working directory, then walk up to the nearest `.reliary/` index.
- **A stray empty `index.sqlite` could shadow a real project.** `Connection::open` is read-write-create, so opening a directory that merely had a `.reliary/` folder silently created a 0-table database; the next walk-up then found that stub instead of the project. Candidates are now validated (`user_version` in range) with a read-only probe before use, and a query never creates a file.
- **Auto-trust indexed the wrong root in a monorepo.** It keyed off the nearest `.git`, so a package nested inside a larger repo was indexed at the repo root instead of the package the user built. It now prefers an existing valid `.reliary/` walking up, matching the tools.
- **The server ignored MCP `roots`.** Clients that advertise the `roots` capability (OpenCode does) now receive a `roots/list` request, and the returned workspace root becomes the project for path-less calls. `notifications/roots/list_changed` re-resolves it, and `ping` is answered.
- **rustls 0.23.45 → 0.23.46** (RUSTSEC-2026-0346 through 0349).

### Added

- MCP root-resolution coverage in the e2e suite: project selection via `path`, via `roots/list`, via walk-up from a subdirectory, and a negative control asserting an unindexed server neither reports nor creates an index.
- **Docker integration tests.** Layer 1 (`Dockerfile.e2e`) drives the installed binary offline (`--network none`) through install → trust → query → MCP handshake → init/uninstall, and gates every push. Layer 2 (`scripts/e2e_opencode.sh`) runs a real OpenCode agent headlessly with a keyless free model and asserts it actually called a `reliary_*` tool, with a negative control; it runs nightly. `make e2e` and `make e2e-opencode` run them locally.

## [0.8.9] - 2026-09-30

### Fixed

- **OpenCode setup failed for every real install.** `init` located its plugin with `binary.ancestors().nth(3)`, which only resolves inside the source tree (`<repo>/target/release/reliary`). From `cargo install`, a tarball, or brew (`~/.cargo/bin/reliary`, `/usr/local/bin/reliary`) it looked for `~/opencode-plugin` or `/usr/opencode-plugin` and failed with a message telling the user to build a source tree they do not have. The built plugin is now embedded in the binary and written to `~/.local/share/reliary/opencode-plugin.js` at init — no source tree, no npm, works offline.
- **The OpenCode MCP entry was written in the wrong schema.** OpenCode requires a local server to be `{ "type": "local", "command": ["<exe>", "mcp"] }`; `init` wrote the Claude/Cline shape (`{ "command": "<string>", "args": [...] }`), which fails schema validation and never starts the server. OpenCode now gets its own writer.
- **A `.jsonc` config could not be edited at all.** OpenCode's documented default is `opencode.jsonc`, which contains comments. `init`, `doctor`, and `uninstall` parsed configs with `serde_json`, which rejects comments, so `.jsonc`-only setups were silently skipped and uninstall left the MCP entry behind. Config handling now uses a JSONC parser (lossless CST edits that preserve comments and formatting).
- **`uninstall` left the OpenCode plugin entry and file behind.** It removed the MCP server but not the plugin path, so OpenCode kept trying to load a file that no longer existed. Removal is now symmetric; unrelated plugin entries are preserved.
- **`doctor` reported OpenCode green on a malformed entry**, because it only checked that the `mcp.reliary` key existed. It now validates the schema and reports "present but malformed".
- **Cline integration looked in a stale extension directory.** The extension id changed from `rooveterinery.cline` to `saoudrizwan.claude-dev`; both are now checked.

### Added

- Config discovery follows OpenCode's own precedence: `opencode.jsonc`, `opencode.json`, then `config.json`.
- Integration tests for the whole OpenCode path (schema-valid MCP entry, plugin file materialised and registered, `.jsonc` preserved, uninstall symmetry, malformed-entry detection), each with a negative control, plus a clean-room Docker test run on amd64 and arm64.

## [0.8.8] - 2026-09-30

### Fixed

- **`--help` named a binary you do not have.** The examples said `reliary-agent index .`, but the shipped binary is `reliary` (the crate is `reliary-agent`; `cargo install reliary-agent` is what puts `reliary` on your PATH). Every example now uses `reliary`, and a test asserts each example line starts with the real binary name.
- **The advertised `rel` shortcut did not exist.** `--help` printed "Shorter: 'rel' also works for all commands", but nothing ever created a `rel` command, so `rel search` failed with `command not found`. `reliary init` now offers to install a `rel` shim into `~/.local/bin` (a symlink on Unix, a `.cmd` wrapper on Windows) and `uninstall` removes it. The help text now says how to get it. A test runs the installed shim and checks it dispatches.

## [0.8.7] - 2026-09-28

### Fixed

- **The Windows self-update no longer leaves `reliary.exe.old` behind.** The update path renames the running executable aside because Windows will not overwrite or delete one in use, but nothing ever removed the renamed file, so it accumulated next to the binary on every update. A detached janitor mode (`--internal-unlink <path>`, hidden from the CLI surface) now waits for the lock to clear and removes it. Implemented by polling the file rather than waiting on a process handle, because the crate forbids `unsafe` and a `WaitForSingleObject` wrapper would not compile.
- **`reliary --internal-unlink` with no path exits immediately** instead of retrying for 30 seconds against the current directory, which can never be removed.

## [0.8.6] - 2026-09-28

### Added

- **Windows binaries.** The release matrix now builds `x86_64-pc-windows-msvc` and `aarch64-pc-windows-msvc` as `.zip` archives, matching the other Reliary repos. `reliary update` understands the new asset names and installs on Windows: it extracts with `Expand-Archive` and replaces a running executable by renaming it aside first (Windows forbids overwriting or deleting a running `.exe`).
- **`workflow_dispatch` on the release workflow**, so the whole platform matrix can be built and inspected without cutting a tag.

### Changed

- **CI now cross-compiles for aarch64-linux and uploads a Linux binary artifact**, so a target-specific break is caught on push rather than on release day. The binary is downloadable from any green run.
- **`release_asset_platform` / `release_asset_name` are now functions with tests** that pin the six release triples, so `reliary update` and `release.yml` cannot drift apart silently (a drift is a 404 for every user on that platform).

## [0.8.5] - 2026-09-28

### Fixed

- **`cargo install reliary-agent` now works.** The crate embedded two hook scripts from the repository root, outside the crate directory, so they were absent from the published tarball and the build failed with `couldn't read src/../../../hooks/claude-pretooluse.sh`. The hooks now live at `crates/reliary-agent/hooks/` and ship in the package. 0.8.0–0.8.4 were affected on every platform.
- **Publishing no longer skips verification.** `publish.yml` passed `--no-verify` for both the dry run and the real upload, which disabled the one check that catches an unpackageable crate. Both now verify, and CI runs `cargo package` for every crate so this cannot return silently.

## [0.8.4] - 2026-09-28

### Fixed

- **`reliary doctor` no longer reports an unusable index as ok.** A `.reliary/index.sqlite` that exists but cannot be opened as a database (empty, truncated, corrupt) was shown with a green tick; it is now a failure that names the file and suggests a rebuild. Shipped separately from 0.8.3 because the 0.8.3 crates were already published when this was found during end-to-end verification of a repaired machine.

## [0.8.3] - 2026-09-28

### Fixed

- **Claude Code and OpenCode hook install was half-wired, and one hook was broken.** The `PreToolUse` code-gate blocked the agent's own grep/glob/read tools using a session key that changed on every invocation, so the once-per-session check never hit and *every* call was blocked, not the first; it also advertised tool names absent from `tools/list`, which the model cannot call. The gate (Claude, OpenCode, and the same gate inside Pi `gate.js`) has been removed — the session reminder and the tool descriptions carry that guidance instead.
- **`reliary init` wrote three Claude hook files but registered only the sift entry** in `~/.claude/settings.json`, so the session reminder never fired while `reliary doctor` still reported "3/3 hooks installed". Install now registers both shipped hooks and doctor reports 2/2.
- **`reliary uninstall` removed hook files but left the settings.json registration**, so the tool call kept firing a command that no longer existed. Removal is now symmetric and preserves unrelated hooks.
- **`reliary doctor` now detects a registered reliary hook whose file is missing** and names it, instead of only counting files on disk.
- **Pi `gate.js` binary discovery now checks `~/.local/bin`**, the documented install location. Agent-spawned processes often run with a reduced PATH, so the previous fallback list could resolve a stale binary or none.
- **Hook scripts are POSIX `sh`.** Agents invoke them with `sh`; a bash-only construct would fail silently.
- **Internal version tags removed from MCP tool descriptions** (`V37:`, `V53:`) — descriptions are shown to the model and should not carry changelog tags.
- **CONTRIBUTING license corrected** to MIT (was stated as MIT OR Apache-2.0), branch name corrected to `master-rebuild`, and the `DEEP_SEEK_API_KEY` typo fixed.

### Changed

- **Documentation rewritten around the product name, Reliary** — the working-directory name `reliary8` had leaked into the README, AGENTS.md, CONTRIBUTING, the OpenCode plugin README, and every crate README. The README is now a product document; benchmark methodology, the edit-outcome result, the familiarity experiment, and the cassette replay moved to `docs/BENCHMARKS.md`.
- **Agent-config instructions corrected** to match `reliary init`: Claude Code uses `~/.claude.json`; OpenCode uses `opencode.json` (the `"mcp"` key, not `"mcpServers"`); Pi is installed as an extension, not an MCP JSON entry.
- **Removed the stale "API proxy" description** from eight crate READMEs (the proxy was removed in 0.8.0).

### Added

- Integration-conformance tests covering the whole install loop: registered hooks resolve to files on disk, doctor goes green after init, `init --dry-run` mutates nothing, re-init does not duplicate entries, uninstall removes exactly what install added, doctor flags a dangling registration, and every tool name in the session reminder exists in `tools/list`.
- CI: OpenCode `dist/` freshness check, a real `reindex-file` integration test, a POSIX-safety lint for every hook, a Pi `gate.js` selftest, and a presence check for the conformance tests.

## [0.8.2] - 2026-09-23

### Added

- **Free benchmark reproduction (cassette)** — `bench/cassettes/canonical-v4/` recorded the canonical 3-way run at the time of that release (the current tape is `canonical-v8`) (A/B/C, seeds 42/17/123/456). `bench/replay_canonical.sh` replays it against a fresh corpus checkout at commit `5c8b6244` with zero API calls, no key, $0. All three conditions verify byte-identical on a fresh corpus at a different path.

### Fixed

- **Identifiers inside string literals were indexed as call sites** — the tokenizer stripped `//`/`#` comments but not string contents, so a name inside a literal became a non-def occurrence row. Found via the llama.cpp bench: `raise NotImplementedError("write_vocab() must be implemented...")` made reliary report `write_vocab` as *called from* that line. This inflated caller lists and under-reported dead code in every language (41% of non-def rows on the canonical corpus were comment/string noise). New grammar-free `strip_strings_and_comments` (blanks `"..."`, `r#"..."#`, char literals, comments; preserves positions and Rust lifetimes) wired into the three identifier-tokenization sites. The brace graph already tracked strings and is unchanged.
- **Keyword-confirmed definitions nested 3+ levels deep were dropped** — the blanket `block_depth > 2` filter discarded any real definition nested three or more levels deep, which is every function inside a `macro_rules!` body and every method inside `impl` inside two or more `mod` blocks. On tokio this silently dropped 265 `fn <name>` declaration lines (`is_def=0`), so `find_references`/`def_only`, `list_methods`, `call_graph`, and qualified names all missed them. A declaration-keyword-confirmed definition is now accepted at any depth; nested control flow is still rejected. Found by the familiarity experiment (obfuscation could not be applied consistently when the index had no definition to rename).
- **Pack/`describe` struct fields omitted types and lines** — the L2 field list printed names only (`fields(tag, is_def, defined_name)`), so "list the fields of X with types" needed a second lookup and the field-located facts were uncitable. Fields now render `name: Type (line N)`. Enum variants likewise carry their line.
- **Pack L2 definition line was 0-indexed** — `read_signature_line` indexes `lines` directly, so a struct at source line 16 printed as `structural.rs:15`. Now displayed 1-indexed like every other tool surface (the V26/V51 off-by-one class).
- **Method line numbers were reported one line late** — `BraceNode.start_line` is 1-indexed (`line_no = line_idx + 1`) while the occurrence table is 0-indexed, and three MCP handlers added another `+1` to the already-1-indexed value (the `V38` raw handler printed it correctly, hiding the inconsistency). `list_methods` reported every method's line as N+1. All four sites now print `MethodOn.line` verbatim. Same V26/V51 off-by-one class.
- **`is_source_like` methods output did not distinguish public from private** — the question "what public methods does X have" was unanswerable from the output, which listed `new` and `collect_method_calls` beside real `pub fn`s with no marker. `MethodOn` now carries grammar-free `is_pub` (leading-token test) and `is_field`, and every handler renders `pub fn name (file:line)` / `name: Type (file:line)`.
- **`auto_questions.py` extracted nothing** — `brace_blocks` returns `(start, end)` but both callers unpacked `end, _`, so the block was always empty and auto-generated q3/q4 ground truth was always empty. Its `impl_methods` also returned 1-indexed lines that `load_auto_gt` then incremented again, and the Rust `bench gen` did the same to `MethodOn.line`.
- **Auto-generated q3 ground truth included private methods** — the question asks for public methods; the GT now filters on `is_pub` (Rust and Python generators both).
- **Cassette accounting was silently absent from every result row** — `os` was imported as `_os` inside `run_long_session` but referenced in `main()`, raising `NameError` that a bare `except Exception: pass` swallowed. Now module-level `os` and the failure is logged.
- **JIT occurrence `is_def` disagreed with ingest** — three lazy-build paths in `lazy_occurrence.rs` re-derived the definition flag from the tag range (`1..=6`, `col == 0 && line_tag >= 5`) instead of carrying the classifier's `is_def` bool. Tag 6 is `local_binding`, which is deliberately not a definition, so a line like `let format = args.get(...)` was a definition when JIT wrote the row and a usage when ingest wrote it — same source, two answers depending on query history. All three paths now carry `result.is_def` verbatim (the `extract_file_phrases` contract). Found by the cassette: the phantom row made tool output depend on which rows had been lazily built.
- **`describe` caller order was non-deterministic** — caller sets were built with `std::collections::HashSet`, whose random per-process hash seed reorders callers on every run. Now `FxHashSet`.
- **BM25 term scoring had no row order** — multi-term accumulation summed in arbitrary row order, shifting float last-digits run to run. Query now has `ORDER BY p.id`.
- **`long_session_bench` summary crashed on any errored run** — `Score per seed` indexed a position into a filtered list; now pairs each seed with its own row.
- **`lazy_jit_test` hardcoded `/tmp`** — ignored `TMPDIR`, failing on hosts where `/tmp` is a full tmpfs. Now `env::temp_dir()`.
- **Bench `search` wrapper never parsed its result** — the `[idx:…]` freshness stamp broke `json.loads`, so condition A's search returned an error string in every benchmark that used it.
- **altbackend results were order- and rank-dependent** — `search_graph` returns the same hit *set* in a different *order* from two indexes of identical bytes, and `rank` is a corpus-statistics float. Both are now dropped/sorted before the result enters the conversation, so the tape is portable and the comparison is set-based.

### Benchmarks

- Deterministic claim verification, canonical tape, 4 seeds 42/17/123/456: F1=A 0.947 / B 0.461 / C 0.615; billed=A 21,459 vs B 23,064 vs C 36,967; keyword=A 27.5 / B 24.8 / C 26.5; dead-ends=A 0.0 / B 3.5 / C 0.0. Record: `bench/cassettes/canonical-v4/record.jsonl` (superseded by `canonical-v8`; this entry is a historical release note, not current numbers). Two measurement corrections landed in this cycle (disclosed separately from product fixes): the claim extractor now rejects sentence-initial prose words as symbols and credits grouped caller citations (`name (987)` under a file header) — both applied symmetrically to every condition and to the ground truth. The tape was re-recorded twice as product fixes changed the index: the string-literal fix removed 41% of non-def occurrence rows, and the deep-nested-definition fix (below) restored keyword-confirmed definitions that had been dropped. **The demonstrated advantage is cost and output size at comparable accuracy; an accuracy superiority over grep is not established.**
- **Familiarity experiment: no flip.** Renaming every identifier in a memorized public repo (tokio) to test whether the accuracy edge was a training-prior artifact did not flip the A-vs-C comparison (judge delta +0.25 original / +0.19 obfuscated, 1.1–1.4σ, n=32). The familiarity mechanism found no support; the cost advantage reproduced on both arms. `bench/FAMILIARITY_EXPERIMENT.md`.
- Bash compression: 31.6% mean / 3.9% median on the 20 RTK fixtures (the earlier 46.3% figure on 6 hand-picked fixtures is retracted).
- **Edit-outcome mutation bench: TIE twice — no demonstrated advantage, line scrapped.** Run 1 (tests visible) tied at 100% f2p because a runnable failing test is a localization oracle. Run 2 withheld the oracle (all tests stripped from the agent workspace via `#[cfg(any())]`, `tests/` deleted; bare prose symptoms; hidden tests injected at scoring on a pristine tree with the agent patch re-applied) — **still 100% f2p for all three conditions, zero wrong-file edits**, kill criterion failed again. Single-line defects in a ~500-file repo are greppable from symptom prose. Claim only the retrieval advantage. `bench/MUTATION_BENCH_V2_PREREG.md`, `bench/MUTATION_BENCH_V2_RESULTS.md`.

## [0.8.0] - 2025-XX-XX

### Changed - BREAKING

- **Removed: HTTP proxy (`reliary serve`)** — The reverse-proxy compression layer (965 LOC, 6 deps: `axum`, `tokio`, `tower-http`, `futures-util`, `tokio-stream`, `tracing`) was untested in benchmarks and never matched proxy-compressed output to LLM input. The `serve` and `daemon` subcommands are removed. The MCP-over-stdio path is the sole transport.

- **Removed: SSE over HTTP transport** — `mcp_sse.rs` removed alongside the proxy.

- **Removed: 39 research-grade modules** — Pure-research math tools that proved less effective than type-flow in symbol disambiguation benchmarks (`mAP 1.0`). These were exposed only behind `RELIARY_FULL_MENU=1`. Removed crates from `reliary-search` Cargo.toml: `nalgebra`, `flate2`, `rand`. Removed CLI subcommands: `BuildLsa`, `MineOps`.

- **Removed: `tracing` dependency** — Replaced with `eprintln!` macros. Tracing infrastructure (~6 deps transitive) was overkill for stderr-based CLI output.

- **MCP tool surface changes**: 62 tools reduced to 19 primary. Specialized variants (`find_references_robust`, `find_references_holographic`, etc.) removed. The 19 primary tools cover every documented use case.

### Performance

- Binary size: ~11 MB → ~8.2 MB (release profile, stripped).
- Cold-start query: 0.001-1.5s (lazy mode keeps derived tables sparse).
- Trust time on small repos: ~100ms.
- Trust time on Linux kernel (72K files): ~60s.

### Improved

- **Symbol intelligence accuracy**: mAP 1.0 on tokio (homonym benchmark), 0.93 cross-corpus on hyper.
- **Cross-language support**: Validated on Rust, Python, JavaScript, Go, Java, Prolog, Nix, Erlang, Haskell.
- **Grammar-free guarantee**: zero keyword matching, zero tree-sitter, zero per-language code in the hot path.

### Fixed

- **Proxy/daemon removal was incomplete**: CHANGELOG claimed `serve` and SSE were removed in 0.8.0 but the code paths (`routes.rs`, `inject_opencode_proxy_routes`, `restore_opencode_proxy_routes`, `write_proxy_routes`, `install_pi_proxy_routes`, `inject_sse_mcp_server`, `install_daemon`, `uninstall_daemon`, `daemon_alive`, `daemon_pid`, `proxy_stats`, `proxy_routes_count`, `has_upstream`, `Commands::Start/Stop/ProxyStats`, 9090/PID files/proxy-routes.json references) were still in the binary and the `reliary init` flow still rewrote OpenCode provider `baseURL`s to `http://127.0.0.1:9090/v1`. All proxy/daemon code, CLI subcommands, init prompts, doctor/status/logs/`has_upstream` plumbing, systemd/launchd registry installers, and tests have now been removed. Cargo.toml description no longer mentions "API proxy". Verified: 51/51 agent tests pass, zero `9090`/`proxy-routes`/`reliary-agent serve` hits in `crates/`.

### Maintained

- All 19 primary MCP tools: `search`, `find_references`, `find_references_with_source`, `find_references_type_flow`, `find_references_boltzmann`, `goto_def`, `callgraph`, `callgraph_v2`, `methods_on`, `scope`, `dead_symbols`, `brace_graph`, `call_graph`, `brace_debug`, `architecture`, `trace_path`, `query_ast`, `compress`, `risk`.

### Added

- **29 primary CLI subcommands** (down from 31 — `serve` and `daemon` removed).
- **`reliary build-occurrences`** — populate the lazy `occurrence` table for offline analysis.
- **`reliary build-all`** — populate all lazy tables eagerly.
- **`RELIARY_FULL_MENU=1` env var** — exposes the full MCP tool set when set.

### Removed

- `gate.js` (safety control layer) was already removed in the prior arc. The v0.8 arc focused on language universality and tool surface reduction.
- `proxy.rs`, `mcp_sse.rs` (transport layer)
- 39 research modules (math variants that underperformed type-flow)
- 9 Cargo dependencies (`axum`, `tokio`, `tower-http`, `futures-util`, `tokio-stream`, `tracing`, `tracing-subscriber`, `nalgebra`, `flate2`, `rand`)
- `Serve` and `Daemon` subcommands

### Migration from v0.6.x

If you previously used `reliary serve` to front your agent with IR compression:

**Before (v0.6.x):**
```bash
reliary serve --port 9090
# Configure agent proxy: http://127.0.0.1:9090
```

**After (v0.8):**
```bash
reliary mcp
# Configure agent MCP server (stdio transport)
```

The `mcp` path is more direct — no HTTP transport, no proxy cache, no second-process to monitor. The MCP server implements the same sanitization and rate-limiting logic as the proxy did, but over stdio.

If you relied on the HTTP API endpoints (`/search`, `/risk`, etc.) for non-MCP integrations, those are removed. Wrap MCP-over-stdio in your own server (~50 lines of Python/Node) to expose them.

## [0.6.13] - 2024-XX-XX

Initial public release. Added in CHANGELOG after first stable API.

---

For the complete history of the v0.6.x series, see the git history prior to the v0.8.0 rewrite.
