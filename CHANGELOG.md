# Changelog

All notable changes to Reliary Agent will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.8.2] - 2026-09-23

### Added

- **Free benchmark reproduction (cassette)** — `bench/cassettes/canonical-v4/` records the canonical 3-way run (A/B/C, seeds 42/17/123/456). `bench/replay_canonical.sh` replays it against a fresh corpus checkout at commit `5c8b6244` with zero API calls, no key, $0. All three conditions verify byte-identical on a fresh corpus at a different path.

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

- Deterministic claim verification, canonical tape, 4 seeds 42/17/123/456: F1=A 0.947 / B 0.461 / C 0.615; billed=A 21,459 vs B 23,064 vs C 36,967; keyword=A 27.5 / B 24.8 / C 26.5; dead-ends=A 0.0 / B 3.5 / C 0.0. Record: `bench/cassettes/canonical-v4/record.jsonl`. Two measurement corrections landed in this cycle (disclosed separately from product fixes): the claim extractor now rejects sentence-initial prose words as symbols and credits grouped caller citations (`name (987)` under a file header) — both applied symmetrically to every condition and to the ground truth. The tape was re-recorded twice as product fixes changed the index: the string-literal fix removed 41% of non-def occurrence rows, and the deep-nested-definition fix (below) restored keyword-confirmed definitions that had been dropped. **The demonstrated advantage is cost and output size at comparable accuracy; an accuracy superiority over grep is not established.**
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
