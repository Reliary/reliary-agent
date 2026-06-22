# Changelog

## v0.6.10

### Esoteric bug sweep — 24 fixes + 4 new guardrails

Deep audit found esoteric bugs (race conditions, TOCTOU, panic recovery, cache
keying, resource leaks) that don't show up in normal testing. All fixed, with
new guardrails to prevent the patterns from recurring.

**Critical fixes (7):**
- ux.rs:402 and mcp.rs:53 — SQL PRAGMA corruption (` synchronous=;` with no
  value, no `PRAGMA` prefix) fixed to `PRAGMA synchronous=NORMAL;`

**High-severity fixes (11):**
- **Bug 51** — Daemon connection counter now decrements on thread spawn failure
  (was leaking forever)
- **Bug 52** — RESPONSE_CACHE now uses proper LRU eviction (was removing
  arbitrary hash-order entries, not oldest)
- **Bug 53** — JSONL log uses persistent file handle (was reopening per call,
  60+ open() syscalls per minute)
- **Bug 56** — try_prefetch debounced to 32KB chunks (was 1000+ spawn_blocking
  per second on streaming responses)
- **Bug 57** — Replaced `Mutex::lock().unwrap()` with `unwrap_or_else(|e| e.into_inner())`
  for poison recovery (was panicking whole daemon on any thread panic)
- **Bug 58** — Response cache key now includes `model` (was returning wrong
  model's response on cache hit)
- **Bug 59** — Guard reverts to tool_calls-only check (regression from v0.6.7
  — was checking prose mentions of "edit")
- **Bug 64** — Agent config lookups now cached for 30s (was re-reading 4+
  config files per request)
- **Bug 68** — Antidecision now uses request's workdir inferred from message
  file paths (was using daemon's startup workdir)
- **Bug 69** — HTTP client now has 5-minute timeout (was hanging forever on
  slow upstream)
- **Bug 71** — Error response body capped at 10MB (was unbounded — 100MB HTML
  error page = OOM)

**Medium-severity fixes (7):**
- **Bug 60** — FTS5 search tokens sanitized to strip `"` and FTS5 special chars
  (was corrupting search syntax)
- **Bug 61** — Added `open_existing_db_safe()` with WAL+NORMAL PRAGMAs for
  crash-safe read access (was synchronous=OFF, no crash safety)
- **Bug 62** — ANTI_DB outer map capped at 1000 workdirs with LRU eviction
  (was unbounded across workdirs)
- **Bug 63** — `extract_auth_key` now case-insensitive on "Bearer"/"bearer"/"BEARER"
- **Bug 66** — Upstream URL scheme validated to http/https only (was accepting
  file://, gopher://, etc.)
- **Bug 67** — RATE_BUCKETS capped at 1000 entries (was unbounded under unique
  auth_key attack)

**Low-severity fixes (1):**
- **Bug 70** — Auth keys > 1KB rejected (was using full key as map key, memory
  waste attack)

### 4 new guardrail rules

Added to `scripts/ci_guards.py` (now **10 rules total**):

7. **unbounded-collection** — flags `Mutex<HashMap>` without visible eviction
   (catches bug class behind 62, 67, 40)
8. **blocking-in-async** — flags `std::fs` operations in async fn without
   `spawn_blocking` (catches bug class behind 56, 69)
9. **no-timeout** — flags `reqwest::Client` without `.timeout()` (catches
   bug class behind 69)
10. **panic-lock** — flags `Mutex::lock().unwrap()` (catches bug class behind 57)

### Tests

- 89 unit tests passing
- 10/10 guardrails passing on clean build
- Pre-commit hook: passes

## v0.6.9

### Guardrails (the big new thing)

This release adds **6 pre-commit + CI guardrails** that detect the bug classes
that have repeatedly appeared in audits. Combined with a new `reliary-core::fs_safe`
module, this makes the correct pattern the easy pattern.

**`reliary-core::fs_safe` module** (Phase A):
- `atomic_write(path, content)` — atomic file write (tmp + fsync + rename)
- `safe_read(path)` — read file with 10MB cap
- `safe_read_stdin()` — read stdin with 10MB cap
- `safe_open_db(path)` — open SQLite with correct PRAGMAs

**`scripts/ci_guards.py`** (Phase B + C):
1. **non-atomic-write** — flags `std::fs::write` outside `atomic_write` pattern
2. **uncapped-read** — flags `read_to_string` without size guard
3. **curl-subprocess** — flags `curl`/`wget` subprocesses (we use reqwest)
4. **sql-unknown-table** — flags SQL queries against tables not in schema
5. **uncapped-stdin** — flags stdin reads without size cap
6. **hardcoded-list** — flags `let valid_keys = [...]` (drift risk)

Runs in pre-commit AND in CI. Mark false positives with `// GUARDED: intentional`.

**Single source of truth for feature names** (Phase D):
- `config::FEATURE_DEFAULTS` const — feature name + default value
- `config::VALID_CONFIG_KEYS` const — all valid config keys
- `main.rs` uses these consts instead of duplicating the list

### Bug fixes (Phase E)

21 bugs from round 3 audit, including:
- **Bug 30** — `run_index` now backs up old index before delete (was `remove_file` directly)
- **Bug 33** — `do_update` now extracts to correct path (was copying from non-existent file)
- **Bug 35-36, 39** — `compress`/`risk`/`dead` commands use size-capped helpers
- **Bug 37** — `start` command captures daemon stderr to log file (was null)
- **Bug 40** — `SSE_SESSIONS` map capped at 1000 (was unbounded)
- **Bug 41** — `messages_handler` drops lock before send (was holding lock)
- **Bug 43** — UUID generation now uses getrandom (was monotonic counter)
- **Bug 46** — Pi `settings.json` write uses `atomic_write`
- **Bug 47** — `atomic_write` cleans up tmp on failure (was leaking)
- **Bug 48** — Env var checks still inlined (helper would be larger)
- **Bug 49** — Proxy has per-auth-key rate limit (60 req/min default)
- **Bug 50** — `edit_cache` table capped at 10K rows

### Tests

- 272 tests passing (was 267)
- 4 new tests for `fs_safe` module
- Pre-commit hook: passes

## v0.6.8
