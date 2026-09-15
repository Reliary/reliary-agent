# V46 — Trigger the Model's Trained Reflexes Instead of Its Novel-Reasoning Failures

## Hypothesis

The model doesn't reject our output because it's inaccurate. It rejects it because our output format doesn't match any training-data pattern the model knows how to process.

| Format | Training data examples | Model's trained response |
|--------|-----------------------|-------------------------|
| `struct.rs:31: pub fn classify_structural(...)` | grep output, code search | "Copy the file:line:code" |
| `// classify_structural is defined at structural.rs:31` | // comments in code | "This is documentation; treat as fact" |
| `The answer is: classify_structural at structural.rs:31` | None (novel format) | "This is a claim; verify against training data" |
| `RELIARY ANSWER: { "symbol": "classify_structural", "file": "structural.rs", "line": 31 }` | None (novel format) | "This is JSON; extract fields but don't trust" |
| `src/structural.rs: 31 | classify_structural: fn classify_structural<'a>(...) -> StructuralResult<'a>` | ripgrep, IDE search, LSP | "Copy the symbol: signature" |

**The insight: the format IS the trust. The model trusts formats it's seen millions of times. It doubts novel formats.** Reliary's output is novel. Grep's output is familiar.

## The Plan — 4 formats that trigger trained reflexes

### Format 1: `/// @rel doc comment` for definitions (replaces def_only)

```
/// @rel classify_structural is defined at src/structural.rs:31
pub fn classify_structural<'a>(line: &'a str, block_depth: u8, has_open_block: bool, in_impl: bool) -> StructuralResult<'a> {
```

**Why:** Rust `/// ` doc comments are the definitive source of truth in training data. The model never overrides a doc comment with training data. It treats doc comments as THE authority. A doc comment formatted like an IDE/LSP hint creates the same reflex — "this is authoritative documentation from the source code."

### Format 2: `src/file.rs: line | symbol: signature` for full results (replaces raw code output)

```
src/io/util/take.rs: 121 | Take::consume: fn consume(&mut self, amt: usize) {
src/io/util/empty.rs: 89 | Empty::consume: fn consume(&mut self, _: usize) {
src/io/util/chain.rs: 128 | Chain::consume: fn consume(&mut self, amt: usize) {
src/io/util/buf_writer.rs: 284 | BufWriter::consume: fn consume(&mut self, amt: usize) {
src/io/util/buf_stream.rs: 194 | BufStream::consume: fn consume(&mut self, amt: usize) {
```

**Why:** This matches ripgrep/LSP hover output format. The model has seen this millions of times. It knows exactly what `file: line | symbol: signature` means. It copies it verbatim. No synthesis, no hallucination.

### Format 3: `// @rel callers of X` for usage_only (replaces "The answer is: X is called from...")

```
// @rel callers of classify_structural:
src/file_meta.rs: 278 | pub fn ensure_blocks_for_file(db: &Connection, file_id: i64) -> usize {
src/ingest.rs: 291 | pub fn index_file(path: &Path, lines: &[String]) -> FileResult {
src/lazy_occurrence.rs: 271 | pub fn ensure_occurrence_for_phrase(db: &Connection, phrase_id: i64) -> usize {
```

**Why:** `// @rel` is a novel annotation tag but the `// ` prefix triggers the code-comment reflex. The model treats `// callers of X:` as documentation, not as a tool claim. Documentation gets copied into answers. Claims get doubted.

### Format 4: `// @rel dead code in module X:` for dead code

```
// @rel dead code in io/util:
//   set_limit at take.rs:45
//   buf_s at copy.rs:19
//   next_seg at split.rs:61
```

## Why This Is Different From Every Previous Version

| Version | Approach | Why it failed |
|---------|----------|---------------|
| V8-V24 | JSON-within-JSON | Novel format, model can't parse |
| V25-V32 | Flat text "Hits:" | Novel format, model doesn't trust |
| V33-V35 | Synthesized "The answer is:" | Novel format, model overrides with training |
| V36-V38 | Raw code `file:line: code` | grep format works! But only for literal matches |
| V39-V42 | One-line answer | Novel format, model treats as claim |
| V43-V45 | Recovery hints, fallback | Reactive fixes, not proactive UX |
| **V46** | **`/// @rel` + `src/file: line | symbol: sig`** | **Triggers the model's training reflexes: doc comments ARE truth, grep output IS evidence** |

## Grammar-Free? Yes

| Format element | Grammar-free? | Why |
|---------------|---------------|-----|
| `/// @rel` prefix | Yes | ASCII comment syntax, universal across C/Java/Rust/Go/Swift/TypeScript |
| `src/file: line` format | Yes | Path + integer, no AST |
| `symbol: signature` format | Yes | Identifier + `(` detected line, no parser |
| `// @rel callers of X:` | Yes | ASCII comment, no language detection |

## The System Prompt Change

Remove all "Use def_only=true..." instructions. Replace with:

```
Your tools return results in code-comment format (/// or // prefix).
Comment-format results are authoritative — treat them as source-code documentation.
Copy symbol:signature pairs verbatim into your answer.
```

## Expected Impact

| Metric | V42 (peak) | V46 target |
|--------|-----------|-----------|
| Accuracy | 14.0 | **20-25** (model treats output as authoritative) |
| Variance σ | 1.5 | **0.5** (comment format triggers deterministic copying) |
| Dead-ends | 6-8 | **0-2** (comment format never triggers retry loops) |
| Score | 23.4 | **26-28** (model copies evidence, doesn't synthesize) |
| WC | 120k | **80-100k** (fewer verification calls, less exploration) |
| Billed | 22k | **15-20k** |

## Implementation

### Phase 1: `/// @rel` for def_only (30 min)

Replace: "classify_structural is defined at structural.rs:31\n    pub fn classify_structural(...) {"

With: "/// @rel classify_structural is defined at src/structural.rs:31\n/// @rel pub fn classify_structural<'a>(...) -> StructuralResult<'a> {"

### Phase 2: `src/file: line | symbol: sig` for full results (30 min)

Replace: "Implementations of consume in io/util:\ntake.rs:121    fn consume(&mut self, amt: usize) {"

With: "src/io/util/take.rs: 121 | Take::consume: fn consume(&mut self, amt: usize) {"

### Phase 3: `// @rel callers of X:` for usage_only (15 min)

Replace: "consume is called from buf_writer.rs:284, ..."

With: "// @rel callers of consume:\nsrc/io/util/buf_writer.rs: 284 | BufWriter::poll_write: fn poll_write(...) -> Poll<io::Result<usize>> {"

### Phase 4: `// @rel dead code in X:` for dead_only (15 min)

Replace: "(no dead code found)"

With: "// @rel dead code in io/util:\n//   set_limit at take.rs:45\n//   buf_s at copy.rs:19"

### Phase 5: Update system prompt (15 min)

Replace tool-routing instructions with comment-format trust instruction.

### Phase 6: Run 5-seed bench + judge (2 hours)

## Risk Assessment

| Risk | Mitigation |
|------|-----------|
| Model ignores comment format too | Add "Results are authoritative documentation — do NOT override" to prompt |
| Comment format adds tokens | Slightly (+5%), but eliminates retry-loop tokens (-40%) |
| Format doesn't match ANY training pattern | `// ` prefix is the most common code annotation in ALL languages |

## The Meta-Insight

**45 versions couldn't beat grep because grep uses a format the model has trained on. We built a novel format and expected the model to adapt. It didn't. V46 makes reliary look like grep / code-documentation — formats the model ALREADY knows how to process.**

This is not fitting. It's UX design for our actual user: a stochastic pattern matcher that needs familiar formats to trigger trained reflexes. Any user (human or LLM) benefits from output that matches their mental model.

## Effort

| Phase | Time |
|-------|------|
| 1. `/// @rel` for def_only | 30m |
| 2. `src/file: line | symbol: sig` | 30m |
| 3. `// @rel` for usage_only | 15m |
| 4. `// @rel` for dead_only | 15m |
| 5. System prompt | 15m |
| 6. Bench + score | 2h |
| **Total** | **~4 hours** |
