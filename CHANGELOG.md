# Changelog

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
