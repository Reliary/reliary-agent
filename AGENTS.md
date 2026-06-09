# reliary-agent — Architecture Decisions

## Workspace Structure

Single Rust workspace. 9 crates. One binary. Each crate is a standalone library with
a single responsibility. Crate boundaries match the original tool they were ported from
(attribution in README and commit messages).

## Why One Binary

Six synergies that are impossible with separate binaries:

1. **Self-healing edits**: daemon intercepts fix calls, shadow-applies, runs tests,
   reverts on fail. LLM never sees the failure spiral.
2. **Compression-aware search**: results pre-stripped to query-relevant lines.
3. **Risk-weighted ranking**: search results sorted by relevance × risk.
4. **IR→Memory pipeline**: compressed assistant messages auto-feed co-occurrence.
5. **Predictive pre-load**: co-occurrence predicts next file read. 0ms latency.
6. **Shared session state**: one daemon, one DB, one co-occurrence matrix.

## Grammar-Free Design

Zero AST parsing. Zero tree-sitter. Zero regex for code structure. All analysis uses:
- **Identifier scanning**: `[A-Za-z_][A-Za-z0-9_]{3,40}` split on non-alphanumeric
- **Porter stemming**: simplified suffix stripping for cross-language normalization
- **Byte DFA**: char frequency tables for prose/code classification
- **Indentation scanning**: whitespace comparison for boundary detection

## Output Personas

Three output formats for three audiences:
- `--format default`: human-readable tables with labels and markdown
- `--format compact`: bare score+path lines, no labels (agent-friendly)
- `--format json`: valid JSON array (CI/script-friendly)

## Ports and Attribution

| Crate | Origin | License | Lines |
|---|---|---|---|
| search | stria | MIT | ~600 |
| compress | gate.js (context-engine) | MIT | ~300 |
| sift | sift CLI + maxwell | MIT | ~300 |
| risk | quale | MIT | ~400 |
| memory | cortex-rs | MIT | ~700 |
| fix | cortex-rs fix.rs + relay edit.rs | MIT | ~250 |
| dead | carrion | MIT | ~300 |
