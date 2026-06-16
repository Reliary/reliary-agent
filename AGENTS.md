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

## Crate Structure

| Crate | Core Capabilities | Lines |
|---|---|---|
| search | BM25 + FTS5, Porter stemming, phrase extraction | ~600 |
| compress | IR reasoning compression, format coercion | ~300 |
| sift | Zone truncation, entropy gate, structural compression | ~300 |
| risk | Pre-edit risk scores, blast radius | ~400 |
| memory | HDC 10K-bit vectors, Hebbian learning | ~700 |
| fix | Pattern extraction, content matching, forgiving signature matching | ~250 |
| dead | Grammar-free dead code via occurrence counting | ~300 |
