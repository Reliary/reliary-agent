# V85 — Warrant: closure certificates for changes

Status: **built and falsifier-passed** (branch `feat/v85-warrant`)
Owner: single engineer, pure Rust, offline, single binary
Depends on: V83/V84 grammar-free classifier, shared lexer, occurrence index

## 1. Problem inversion

Reliary answers **existential questions over state** ("where is X?").
An agent's product is a **transition**, and the question that ends a session is
universal: **"is this change closed?"** A rename is a symmetry of the source;
a dangling reference is symmetry breaking.

Warrant turns the index into a **closure oracle over a generation pair**:
two structural states, a declared contract, and a replayable certificate whose
verdict is `closed / open / indeterminate`, with source-backed counterexamples
and an explicit unseen region.

Unit of value: a closed change, not a retrieved symbol.

## 2. Prior art and claim discipline

Precedented: GumTree (AST diff), difftastic, RefactoringMiner, change-impact
analysis (Arnold & Bohner), proof-carrying code (Necula), in-toto/SLSA
attestations, soundiness (Livshits et al.), Semgrep/CodeQL, rename support in
rust-analyzer/clangd/tsc.

New: the **combination** — grammar-free state pair + delta accounting +
coverage-bounded three-valued verdict + source-backed counterexamples +
canonical replayable certificate, in-loop at sub-second reindex. Label:
*novel-in-context*, never "first" or "unique".

Must NOT claim: proves correctness; sound; complete; beats LSP/compiler;
works equally on every language; improves edit outcomes; zero false positives.

## 3. Antagonism log (what died, what survived)

| # | Attack | Outcome |
|---|--------|---------|
| A1 | Stemmed phrases make name closure wrong: `handle` and `handles` share stem `handl`, so "no rows for the stem" is a false CLOSED. | **Killed the stem-based closure design.** Closure uses **raw-token scanning**; the stem index is only a coarse candidate filter where needed. Mandatory test with `handle`/`handles`. |
| A2 | Recursive calls (`fn foo` calling `foo()`) must be renamed too. | Kept — raw scan catches the remaining token. Test. |
| A3 | Common names (`new`, `get`) make `rename` flag unrelated sites. | Survived with per-condition `scope`; evidence lists every site so the reviewer decides. No thresholds. |
| A4 | Line mapping base→head is the weakest link (reformatting breaks coordinates). | **Killed line mapping.** Closure = "the old token appears nowhere in head"; no coordinate alignment is needed. |
| A5 | Two full indexes per run is slow (~20 s each on a 600-file repo). | Base index cached by **manifest digest** (content-addressed); head rebuilt per run. v1 correctness first; overlay is a follow-up, not a blocker. |
| A6 | Manifest hashing is an extra full read. | One pass per tree returns digest **and** token hits; files >10 MB or binary are listed unseen without hashing content (size only). |
| A7 | Delta accounting fails every real change (incidental edits). | Asymmetric rule: unclaimed **removals** → OPEN (dangerous); unclaimed **additions** → warning list only. Stated in docs. |
| A8 | Arity from source text is heuristic (defaults, generics, closures). | Arity counted at top-level commas inside the first paren group; if the def line is unparseable the condition is INDETERMINATE, never guessed. |
| A9 | `no_new_dead` needs whole-repo analysis. | Computed from the index with SQL: defs in changed files with zero non-def occurrences in head but non-zero in base. No re-scan, no LLM. |
| A10 | Certificates could be non-deterministic (SQL order, timestamps, absolute paths). | Canonical form: sorted arrays, relative paths, no timestamps, no temp names; byte-identical double-run test. |
| A11 | The falsifier's full 150-run matrix is hours on `cargo check`. | Smoke tier on a synthetic crate (6 mutations) in-session; full tier documented and runnable on demand. |
| A12 | This must stay grammar-free. | No parser, no AST: shared lexer for masking, raw token scanner, occurrence index only. |
| A13 | Adding an 8th MCP tool repeats the W5/W7 menu failure. | **No MCP tool.** CLI + exit codes; hook/CI wiring later. |
| A14 | Working tree is dirty; what is "base"? | Base is always an explicit rev (default `HEAD`) resolved to a detached worktree. Head is the working tree. Dirtiness is the input, not an error. |
| A15 | Changed data files (Cargo.toml) would force INDETERMINATE forever. | Unseen files are always reported; `strict_unseen` (default false) escalates them. Text files are token-scanned even when unindexed, so closure is not blind to them. |
| A16 | `git worktree`/temp cleanup on error paths (past leaks). | Labeled cleanup block, unconditional removal on every exit path. |
| A17 | `$`-prefixed TS identifiers split by the scanner. | Documented limit; `$ZSF` is scanned as `ZSF`. Contract names are normalized the same way. |
| A18 | Case-insensitive languages (SQL/Pascal) would miss variants. | v1 matches case-sensitively; documented. |

## 4. Contract (input)

JSON, `version: 1`. Parsed with serde (already a dependency).

```json
{
  "version": 1,
  "scope": ["src/"],
  "strict_unseen": false,
  "conditions": [
    {"kind": "rename",    "from": "old_name", "to": "new_name"},
    {"kind": "rename",    "from": "a", "to": "b", "scope": ["src/a"]},
    {"kind": "remove",    "name": "dead_fn"},
    {"kind": "add",       "name": "new_fn"},
    {"kind": "signature", "name": "f", "arity": 3},
    {"kind": "no_new_dead"},
    {"kind": "files",     "within": ["src/"]}
  ],
  "allow_unseen": ["vendor/", "*.lock"]
}
```

Path matching: `entry` is a prefix of the repo-relative path; `*suffix`
matches by suffix. No glob dependency.

## 5. Certificate (output)

Canonical JSON. No timestamps. Repo-relative paths. Sorted arrays.
`certificate_sha256` = SHA-256 of the canonical bytes of the object without
that field.

```json
{
  "warrant": 1,
  "verdict": "closed | open | indeterminate",
  "base": {"digest": "sha256:…", "files": 812},
  "head": {"digest": "sha256:…", "files": 815},
  "contract_sha256": "sha256:…",
  "conditions": [
    {"index": 0, "kind": "rename", "from": "a", "to": "b",
     "verdict": "open",
     "evidence": [{"symbol": "a", "file": "src/x.rs", "line": 42,
                   "col": 7, "excerpt": "foo(a, b)",
                   "excerpt_sha256": "sha256:…"}]}
  ],
  "delta": {
    "unclaimed_added":   ["helper"],
    "unclaimed_removed": ["old_name"],
    "files_added":  ["src/new.rs"],
    "files_removed": ["src/gone.rs"],
    "files_changed": ["src/lib.rs"]
  },
  "unseen": [{"path": "assets/logo.png", "reason": "binary"}],
  "waived_unseen": ["vendor/x.lock"],
  "certificate_sha256": "sha256:…"
}
```

Verdict rules:
- `open` if any condition fails, or any unclaimed def removal exists.
- `indeterminate` if any condition cannot be evaluated (unparseable arity,
  missing base index, unreadable needed line). `strict_unseen` escalates any
  non-waived unseen file to indeterminate.
- `closed` otherwise.

Exit codes: `0` closed, `1` open, `3` indeterminate, `2` usage/error.

## 6. Algorithm

### 6.1 Tree analysis (one pass per tree)

`anаlyze_tree(root, needles) -> (Manifest, Hits)`:

- Walk files; skip `.git`, `.reliary`, and `ingest::SKIP_DIRS`.
- File ≤10 MB: read once. SHA-256 over bytes → manifest entry.
  Also mask strings/comments with the shared lexer (`lex::masked_line`,
  one persistent `LexState` per file, hash-comments on) and scan
  identifiers for the needles, recording `(needle, relpath, line, col, raw_line)`.
- File >10 MB or `is_likely_binary`: manifest entry with size only, reason
  `oversize` / `binary`; no hits.
- Manifest digest = SHA-256 over `"{relpath}\0{sha256}\n"` sorted by path.
  Oversize/binary entries contribute `"{relpath}\0size:{n}\n"`.

The same pass serves hashing and closure scanning; no second read.

### 6.2 Indexes

- Base: `git worktree add --detach <tmp>/base <rev>` (or a directory given
  with `--base`), indexed with `struct_diff::index_revision` into a
  content-addressed cache `.reliary/warrant/base-<digest>.sqlite`.
  Cache hit = no indexing.
- Head: working tree indexed into a temp DB via `index_revision`.
- Both are relativized against their roots before comparison
  (`struct_diff` stores absolute canonical paths).

### 6.3 Conditions

- `rename(from,to,scope)`:
  - `from` is a whole identifier (validate: ASCII ident, len ≥2).
  - Head hits for `from` (filtered by scope) → OPEN with evidence.
    No hits → CLOSED.
  - Also require `to` appears somewhere as a token; otherwise evidence is
    annotated `target-missing` (still CLOSED only if `from` is gone and `to`
    exists — otherwise OPEN).
- `remove(name)`: no head hits → CLOSED; else OPEN with evidence.
- `add(name)`: a def row with `stem(name)` exists in head **and** the exact
  token appears in that file → CLOSED; else OPEN.
- `signature(name,arialty)`: locate def rows (head, tag 1/2, `is_source=1`);
  read the def line; count top-level commas inside the first `(...)` group
  after the name; zero params = arity 0. Mismatch → OPEN. Unparseable →
  INDETERMINATE. Multiple def sites: every site must match (evidence lists
  offenders).
- `no_new_dead`: for def phrases in changed files: base had ≥1 non-def
  occurrence and head has 0 → OPEN with the symbol. (SQL, sorted.)
- `files(within)`: changed paths must all match at least one prefix → else
  OPEN with offenders.

### 6.4 Delta accounting

Def rows = `occurrence.is_def=1 AND tag IN (1,2) AND file_map.is_source=1`,
keyed by `(stem, relpath)`.

- Removed = base keys not in head, restricted to changed/removed files.
- Added = head keys not in base, restricted to changed/added files.
- A removed key is **claimed** by `rename` (stem(from) == key stem, path in
  that condition's scope) or `remove` (same). Unclaimed removals → OPEN.
- Added keys with no `add`/`rename` claim → `delta.unclaimed_added`
  (warning, does not open).
- Removed files: every base def is a removal (must be claimed).
- This is the anti-gaming mechanism: a trivial contract cannot hide an
  undeclared deletion.

### 6.5 Evidence is source-backed

Every violation carries the raw line + SHA-256 of that line, read from the
tree where the site was found. The certificate can be checked by a third
party without rerunning the tool (and without trusting the index).
`reliary verify` is unchanged in this version; warrant does not depend on it.

## 7. Build phases

- **P1** `warrant.rs`: manifest, tree analysis, token scanning, contract
  types, canonical certificate. Unit tests (manifest determinism; stem-collision
  negative control; masking: comment/string tokens absent).
- **P2** Condition evaluation + delta accounting + verdict aggregation.
  Unit tests per condition incl. mutation negative controls.
- **P3** CLI `reliary warrant` (`--base`, `--head`, `--contract`, `--cert`,
  `--json`, `--strict-unseen`), git worktree resolution, cleanup.
  Exit-code integration test on a temp git repo.
- **P4** Falsifier harness `bench/warrant_falsifier.py` (smoke tier default).
- **P5** Docs: README section, CHANGELOG; gates: tests, clippy `-D warnings`,
  `cargo deny`.

## 8. Tests and negative controls

Each rule must fail when removed:
- raw-token closure (restore stem matching → `handle/handles` test fails)
- masking (disable lexer masking → comment mention test fails)
- delta accounting (ignore unclaimed removals → test fails)
- arity (accept unparseable → INDETERMINATE test fails)
- unseen waiver (ignore `allow_unseen` → test fails)
- canonical certificate (introduce timestamp → double-run test fails)

End-to-end: temp git repo, commit, complete rename → exit 0; def-only rename
→ exit 1 with the missed `file:line`; add of an undeclared removal → exit 1.

## 9. Falsifier (pre-registered, ~$0)

Corpus: reliary8 (`reliary-search` crate) and a synthetic 3-module Rust crate.
25 symbols with ≥3 call sites; two variants each: (a) def renamed only,
(b) all sites renamed.
Oracle: `cargo check` filtered to the mutated symbol.
Metrics: violation precision, **silent-miss rate**, false positives on
complete renames, unseen AUC.
Kill: any silent miss on a high-confidence site; precision <0.90; >5% silent
misses; any complete-rename false positive; AUC <0.75.
Smoke tier (in-session): 6 mutations on the synthetic crate.

## 10. Build result (what shipped)

- `crates/reliary-search/src/warrant.rs` — contract, manifest, tree scan,
  conditions, delta accounting, certificate, 17 unit tests.
- `crates/reliary-agent/src/main.rs` — `reliary warrant` subcommand, base
  worktree resolution, cleanup on every exit path.
- `crates/reliary-agent/tests/e2e_warrant.rs` — 5 end-to-end tests on real
  git repos (complete rename = 0, def-only rename = 1 with the missed site,
  undeclared removal = 1, bad contract = 2, replay byte-identical).
- `bench/warrant_falsifier.py` — 51-run falsifier, oracle `cargo check`.
  Result: precision 1.0, silent misses 0, complete-rename FPs 0, stem
  control catches a stem-based implementation.
- 622 workspace tests, clippy `-D warnings` clean.

Deviations from the plan, with reasons:
- **No schema v7 / `file_manifest`.** Manifests are computed at scan time and
  carried in the certificate; no migration, no staleness table. The plan's
  `file_manifest` was cut because a sidecar-free scan is simpler and the
  certificate is self-binding.
- **No separate `overlay.rs`.** Head is indexed into a temp DB via
  `index_revision`; the overlay remains a future optimization (A5), not a
  v1 dependency.
- **`new_dead` counts uses across the whole tree**, not just changed files:
  the definition's own file need not change for its last use to disappear.
- **Path selectors support a single `*`** (prefix/suffix), not full globs.

## 11. Limits (documented, not hidden)

- Case-sensitive matching (SQL/Pascal case-insensitivity not modeled).
- `$`-prefixed identifiers normalized to their bare token.
- Binary/oversize files: listed unseen, never scanned.
- Submodules are directories in a worktree; walked only if checked out.
- The certificate states closure **with respect to declared conditions and
  the seen region** — not correctness, not soundness.
- No MCP tool; hook wiring follows only if the falsifier passes.

## 12. Performance work (A5 + scan) — measured

Order: correctness prerequisite first, then measure, then spend.

| Phase | Change | Measured effect |
|---|---|---|
| P0 | `relativize_index` / `index_revision_relative` | Prerequisite. A cached index built at another root would otherwise report every base definition as removed. |
| P1 | `RELIARY_WARRANT_TIMING=1` phase split | Not a feature; it decided the rest. |
| P2 | Allocation removal in `scan_text` | Reused masked buffer, per-worker stem memo, `FxHashMap` counts, and `is_likely_binary_bytes` so each file is read **once** instead of twice. |
| P3 | Parallel scan, width `min(cores, 4)`, `RELIARY_WARRANT_THREADS` | Serial and parallel output proven byte-identical by test. Bounded: peak memory is `workers x file size`. |
| P4 | Content-addressed index cache, `.reliary/warrant/`, 4 entries / 512 MB | 1.43 s -> 1.5 ms on the cached base side. |
| P5 | Head index cached on the head digest | head index 1.6 s -> 0.1 ms on an unchanged tree. |
| P6 | Scan cache keyed on (tree identity, needles, effective excludes) | scan 0.2 s -> 2 ms per tree; the identity pass drops the lexer and runs its reads in parallel. |
| P7 | Exclude set prepared lexically, plus `git ls-files --directory` | identity pass 560 ms -> 34 ms: 37k canonicalize() calls removed, then 37k exclude entries collapsed to 395. |
| P5 | SIMD | No intrinsics: `#![forbid(unsafe_code)]` in all 12 crates. `memchr`/`memmem` used where the std ecosystem provides them; auto-vectorization does the rest. |

Timings on this repository, `HEAD~1..worktree`:

```
cold (--no-cache)   total 3.0-3.5 s   base-index 1.4 s  head-index 1.4-1.6 s  rest < 0.3 s
warm (all cached)   total 0.10 s       identity pass 0.04 s, everything else < 2 ms
```

Warm phase split (577 files): identity pass 34-45 ms, base index 1.5 ms,
head index 0.1 ms, both scans 2 ms each, delta 40-60 ms, evaluate total
0.10 s. The identity pass is now the floor: it reads and hashes every file,
which is what detecting a change costs.

Once both sides are cached, the remaining cost is the tree scan itself (two
passes, ~0.3 s each on this repo), which every run must pay because it is how
the change is detected. The next lever is therefore not another index
optimisation but skipping the second scan: reuse the head scan as the base
scan whenever the base tree is identical to a previously scanned tree.

Cache safety: the key mixes the manifest digest, `SCHEMA_VERSION`,
`INDEXER_VERSION`, a **build-time fingerprint of the indexer sources**
(`build.rs` hashes classifier/lexer/extractor/occurrence/schema/tokenizer/
segment files and bakes the digest into the binary, so a forgotten version
bump cannot serve stale rows), and the classifier mode; entries are written to a temp file,
fsynced and atomically renamed; a wrong schema version or key triggers a
rebuild; `--no-cache` and `RELIARY_WARRANT_CACHE=0` bypass it. Verified
byte-identical certificates across cache miss, cache hit, and no-cache.

## 13. Definitions come from the scan (A5b)

Measured first: 158,326 occurrence rows were being built per head index, and
warrant read 5,231 of them. The scan already reads and lexes every byte to
produce counts and hits, so the index was a second full pipeline feeding a
fourth-order input.

**Change.** Definition sites are now derived in the scan, by the same functions
the indexer calls (`count_braces`, `classify_structural`, `tokenizable_code`,
`scan_identifiers`, `stem_identifier`, `def_name_matches_token`). No index is
built unless `RELIARY_SEGMENTER=1`, whose island classifier the scan does not
reproduce; that mode builds indexes and reads defs from them, and reports
`defs-source: index (segmenter)`.

**Equivalence is proven, not assumed.** `scan_defs_match_index_defs` builds a
real index for a fixture and asserts the scan's def set equals `def_rows` as a
sorted multiset. `scan_defs_real_corpus` does the same on a tree named by
`RELIARY_WARRANT_GATE`. Measured: **5,272 = 5,272** on this repository and
**3,675 = 3,675** on the canonical snapshot, zero divergence.

The gate caught a real divergence on its first run. The scan's counts/hits use
the cross-line lexer, which treats a markdown fence as an unterminated template
literal and masks the rest of the file; the index resolves definitions with the
per-line `tokenizable_code`. A definition inside a fenced block was visible to
the index and invisible to the scan. Definitions are therefore resolved with
`tokenizable_code`, one allocation per *definition line* rather than per line.

**Admission is matched explicitly**, because the failure is silent: a file
admitted by the scan but rejected by the indexer adds definitions that no index
ever had. The scan applies the same chain — `is_known_data_ext`, UTF-8
(`read_to_string` semantics, not lossy), then `is_source_like`. Both gates have
mutation-verified controls: removing either one makes
`scan_defs_admission_matches_indexer` fail with the offending row named.

**Timings** (this repository, 578 files):

```
                        before      after
cold (--no-cache)       3.0-3.5 s   ~0.23 s
first run after edit    1.9-2.9 s   ~0.11 s
repeat run (warm)       105 ms      ~53 ms
cache on disk           28 MB       1.4 MB
```

Four contracts (rename, no_new_dead, signature, files) were compared against
the pre-change binary and are **byte-identical**.

**Cache safety.** `TreeScan.defs` has no serde default, so a cache file written
before the field existed fails to parse and is treated as a miss; a default of
`[]` would make every contract read CLOSED, a failure that looks like success.
`SCAN_FORMAT` is part of the scan key, and `warrant.rs` is now in the build-time
fingerprint source list, since it defines the cached shape.

**Honest limit.** The runtime cross-check is gone: drift between the scan and
the indexer is caught by CI, not by the shipped binary. The segmenter mode still
builds both and can be used as a manual differential (`RELIARY_SEGMENTER=1`)
when a discrepancy is suspected.

