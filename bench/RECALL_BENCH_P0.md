# P0 — head-to-head recall, no LLM

The product claim is not "this tool fixes bugs". It is: **the indexer gives an
LM accurate, cheap recall of code structure, so the LM can find what to change
without burning context.** P0 measures that first half, with no model in the
loop.

- No sampling variance, no memorisation confound, no API cost.
- Ground truth is an independent lexical oracle (comments and string literals
  blanked, whole-word match). It shares no code with reliary's lexer, so no tool
  grades itself.
- Tools are driven over MCP directly, exactly as an agent would drive them.

## Corpora

| corpus | language | files | symbols | ctags defs |
|---|---|---:|---:|---:|
| `psf/requests` @ 611c616 | Python | 37 | 23 | 1,000 |
| `tokio` @ snapshot | Rust | 802 | 24 | 13,130 |

Symbols are sampled **outcome-blind**: sorted by occurrence count in three
frequency bands (4–15, 16–60, 61+), never by any tool's result. Two exclusion
rules are applied before any tool runs, both from ctags and paths alone:

- symbols whose only definitions live in test paths (2,219 on tokio): asking
  "who calls X" for a helper that exists only inside a test file is a category
  error, because `usage_only` excludes tests by design;
- symbols whose only definitions are kinds reliary does not model
  (enumerators, modules, struct fields).

## Results

`R_use` = recall of call sites, declaration lines excluded on every side.
`P_use` = precision against the same set.
`R_noSt` = the same, restricted to symbols whose phrase key is not shared with
another identifier (a stem-keyed index cannot separate `Request`/`request`).
`R_ntest` = non-test only, which is `usage_only`'s default scope.
`def` = is the correct definition line returned by a definition query.
`bytes` = mean tool output per call. `p50` = warm latency.

### psf/requests (Python)

| tool | R_use | P_use | R_noSt | P_noSt | R_ntest | P_ntest | def | bytes | p50 |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| **reliary** | **0.929** | **0.957** | 1.000 | **1.000** | **0.984** | **0.908** | 0.957 | **1,813** | **3.3 ms** |
| codebase-memory-mcp | 0.846 | 0.668 | 0.990 | 0.757 | 0.842 | 0.505 | 0.957 | 11,624 | 17.4 ms |
| grep | 0.905 | 0.824 | 1.000 | 0.884 | 0.898 | 0.660 | — | 5,411 | 7.1 ms |

### tokio (Rust)

| tool | R_use | P_use | R_noSt | P_noSt | R_ntest | P_ntest | def | bytes | p50 |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| **reliary** | 0.905 | **0.988** | 0.939 | **0.987** | **0.932** | **0.973** | **1.000** | **1,431** | **8.5 ms** |
| codebase-memory-mcp | 0.867 | 0.694 | 0.946 | 0.724 | 0.828 | 0.583 | 0.750 | 12,746 | 37.7 ms |
| grep | **0.920** | 0.780 | **0.999** | 0.756 | 0.896 | 0.688 | — | 4,954 | 27.2 ms |

Definition queries only: reliary 110–115 bytes vs codebase-memory-mcp 5,608
bytes for the same question (50× smaller). Definition hit rate is 1.000 on
tokio against 0.750, and identical on Python (0.957 both).

First call of a session costs 74 ms (reliary), 74 ms (cbm), 27 ms (grep). After
that everything is milliseconds. A model round-trip is seconds, so **none of
these tools is a latency bottleneck**; the differences below are about bytes and
noise, not speed.

Determinism: two full runs on tokio produced byte-identical scores; only
latency varied (warm p50 2.4 → 6.3 ms under a co-tenant load).

## Verdict — this partially refutes the hypothesis

**Recall is now ahead on Python and still just behind on tokio.** reliary
reaches 0.929 on requests (grep 0.905) and 0.905 on tokio (grep 0.920). The tokio
gap is 1.5 points; the Python lead is 2.4. On the no-stem-collision subset
reliary is 0.939 on tokio against grep's 0.999, so text search still finds more
of the truth there. The honest reading: an index roughly matches a text scan at
*finding* occurrences, and wins decisively on what it returns with them.

What the data supports is narrower and still useful:

1. **Precision.** reliary returns the right sites and almost nothing else
   (0.957 / 0.988) where codebase-memory-mcp returns a lot of noise (0.668 /
   0.694) and grep returns comments and prose (0.824 / 0.780). For a model, a
   wrong line is a wasted read or a wrong edit. On the no-collision subset
   reliary is at 1.000 / 0.987 precision.
2. **Answer size.** 1.4–1.8 KB against 5.0–12.7 KB, at equal recall. That is
   3–9× less context for the same answer, which is the token-cost claim from the
   comprehension benchmark reappearing with a mechanism.
3. **Definition queries.** One line and 111 bytes, versus 5.6 KB of surrounding
   code from the competitor, at a higher hit rate on the larger corpus.

So the honest phrasing is: **reliary does not retrieve more, it retrieves less
noise and fewer bytes for the same recall.** That is a real advantage for a
model with a context budget; it is not the advantage the claim originally
implied.

## P1 — Python definition recall

Separately from usage recall, the blind-oracle definition comparison found
Python definition coverage at 0.79. Four real bugs were behind it, all fixed:

| bug | effect | fix |
|---|---|---|
| `scan_identifiers_iter` had a 3-char floor | single-character names (`def u(s)`) invisible | floor is 1 |
| `is_valid_identifier` required 2 chars | same, in the classifier | floor is 1 |
| `is_pascal_case` looked at byte 0 | `class _CookieCompatResponse:` invisible | skip leading underscores |
| `impl`-style qualifier `(` was read as a signature | `pub(crate) struct X {` rejected | skip a leading qualifier group |

And in the tokenizer, which affected *usage* precision rather than definitions:

| bug | effect | fix |
|---|---|---|
| per-line masking reset the lexer each call | every docstring interior was tokenized as code; `request` had 241 prose hits in requests | cross-line literal state, one per file |
| `impl ... for <path>` took the first identifier | `std::io::Error` recorded as `std`; `&'_ MockFile` as `_` | last path segment of the self type |

Definition recall after the fixes, against the blind ctags oracle:

| corpus | before | after |
|---|---:|---:|
| psf/requests | 0.79 | **0.999** |
| encode/httpx | — | **1.000** |
| tokio | 0.918 | **0.992** |

On tokio the per-kind result is function 1.000, method 1.000, typedef 1.000,
struct 0.999, implementation 0.989, interface 1.000. Usage precision rose from
0.792 to 0.957 on Python (0.988 on Rust) once docstrings stopped being
tokenized.

A second round closed the alias/trait-method/generic-arrow/where-clause set:

| shape | example | was |
|---|---|---|
| lowercase type alias | `pub type uid_t = u32;` | not a definition |
| trait method without `self` | `unsafe fn from_raw_handle(h: RawHandle) -> Self;` | not a definition |
| generic list containing an arrow | `struct CallOnDrop<O, F: FnOnce() -> O> {` | named `O` |
| where-clause before the body | `impl<St> Trait for St where St: Stream {}` | not a definition |
| `unsafe impl` | `unsafe impl<T> Send for X<T> {}` | named `impl` |

## Fitting check

- Corpora and symbols chosen before any tool ran, from ctags and source only.
- The two exclusion rules remove categories the tool *documents* it does not
  model. They are disclosed above and would apply identically to a competitor
  with the same scope statement.
- Three scoring bugs were found and fixed during the work, each disclosed rather
  than tuned around: grep was being pre-filtered by truth (inflating its
  precision to 1.0), the oracle had no Python triple-quote handling (corrupting
  the truth set), and test-path filtering ran on basenames so it never fired on
  tokio.
- No question or truth value was changed after seeing a tool's answer.

## Product bugs found while measuring

Each of these made a real question unanswerable, and each is fixed:

1. **`stem_identifier` Porter-stemmed single-word PascalCase types.**
   `Session` → `ses` collided with the lowercase `session`, so 181 occurrence
   rows merged into one phrase and "who calls Session" answered with lowercase
   sites. Any `-sion`/`-tion`/`-ment` type hit this (`Transaction`,
   `Connection`). CamelCase compounds were already protected; single-word types
   were not.
2. **`phrase_id_for` preferred the stem over the exact phrase**, so a caller
   that had already stemmed got stemmed twice: MCP passes `session`, and the
   resolver turned it into `ses`. Exact phrase with occurrences now wins.
3. **`usage_only` had no way to ask for more than 12 call sites.** The answer
   said "(+48 more)" and there was no parameter to retrieve them. Added
   `limit`.
4. **`LIMIT 50` ran before the test-path filter**, so a symbol used mostly in
   tests returned almost nothing: `Response` in requests has 227 occurrences and
   `usage_only` answered with 4. The filter now runs in SQL so the limit applies
   to surviving rows.
5. **No way to include tests.** `usage_only` excludes tests, which is right for
   "who calls this in production" and wrong for "rename this and miss nothing".
   Added `include_tests`; with it, `Request` goes from 4 to 224 sites.
6. **The same-file slot cap ignored `limit`**, so a request for 500 sites still
   reserved only 6 for same-file callers.

## What this does not measure

- Whether better precision converts into better fixes. That is P2, and it needs
  tasks where finding the code is the hard part.
- Cross-file rename completeness in a language with no compiler check.
- The 0.79 Python definition recall separately measured against the blind ctags
  oracle; this bench samples by usage frequency, not by every definition.

## Reproduce

```sh
# corpus + index
git clone --depth 1 https://github.com/psf/requests /tmp/p0/requests
reliary trust /tmp/p0/requests && reliary build-all /tmp/p0/requests
codebase-memory-mcp cli index_repository '{"repo_path":"/tmp/p0/requests"}'

# ground truth (ctags, Docker; never a runtime dependency)
docker run --rm -v /tmp/p0/requests:/c:ro -v /tmp/p0/tags:/out <ctags-image> ...

# measure
python3 bench/recall_bench.py --corpus /tmp/p0/requests --lang python \
  --tags /tmp/p0/tags/requests.tags --project <cbm-project> --symbols 24
```

## A method-listing gap that the recall bench could not see

P0 scores "who calls X" and "where is X defined". It does not score
"what methods does X have", and that question was broken for every language
without braces: `list_methods` walked the type's brace-delimited body, so a
Python `class` never entered its own body and every Python type answered
"No methods found". `Session`, `Response`, `PreparedRequest` and `HTTPAdapter`
in requests all returned nothing while their `def`s were indexed.

Fixed with an indentation-based body walk for layout languages. Measured:

```
Before:  Methods on Session: No methods found on Session.
After:   Methods on Session: fn __init__ (sessions.py:442), fn prepare_request
         (sessions.py:511), fn request (sessions.py:557), ... (20 methods)
```

The competitor exposes no per-type method list at all, so this is a capability
gap rather than a recall delta.

## Operational note: `build-all` reports totals, not inserts

`reliary build-all` printed the number of rows it *inserted*, which is 0 on an
already-built index. On a fresh trust it printed `occurrence=26921`, and on the
next run `occurrence=0` while the table still held 26921 rows. That reads as
data loss and cost time chasing a phantom. It now prints the table total plus
the count of new rows: `occurrence=26921 (+0 new)`.

Related: `reliary index` on an existing project clears the lazy tables, so the
next `build-all` must rebuild them. That is correct but easy to miss; the
message now makes the state visible.

## Regression check on the shipped binary

The indexing changes above touch every tokenization site, so the comprehension
benchmark was re-run on the shipped binary (condition A only, 4 seeds, same
corpus and questions):

| seed | score /30 | billed | tool calls | dead-ends |
|---|---:|---:|---:|---:|
| 42 | 28 | 23,323 | 15 | 0 |
| 17 | 28 | 22,588 | 14 | 0 |
| 123 | 28 | 21,374 | 14 | 0 |
| 456 | 28 | 24,855 | 15 | 0 |

Claim-verified F1 **0.948–0.950** (precision 0.985–0.990, recall 0.913,
coverage 1.00, zero refusals). The committed canonical tape records 0.937 for
the earlier binary; the delta is within seed variance and in the expected
direction, since the index now finds definitions it previously dropped.

## Known limitation: homonyms cannot be separated

The index keys references by name, not by binding. A call site lives *outside*
the definition it calls, so knowing a definition's line extent does not tell you
which definition a distant call site belongs to. I prototyped anchor scoping
(`anchor_file`/`anchor_line` filtering to the definition's extent) and it
returns zero sites, which is the expected result and the reason it was not
shipped:

```
class Alpha:            # c.py:1
    def get(self): ...  # c.py:2
class Beta:
    def get(self): ...  # c.py:7
def use_alpha(): ... Alpha().get()   # c.py:12  (outside both extents)
def use_beta():  ... Beta().get()    # c.py:16  (outside both extents)
```

Scale of the limitation:

| corpus | names with one definition | names with 2+ |
|---|---:|---:|
| requests | 695 / 778 (89%) | 83 |
| tokio | 2,132 / 3,256 (65%) | 1,124 |

Of the multi-definition names, 683 in tokio are declared twice in the *same*
file, where `path_filter` cannot help either. For those queries the tool
answers "here are all sites of this name", which is correct but not scoped.
`path_filter` does separate the cross-file cases, and the answer says so.

This is a real ceiling on the "index beats grep" story for homonymous names:
grep has the same ambiguity, so the comparison is a tie there rather than a
loss, but neither tool can answer "this `get` specifically".
