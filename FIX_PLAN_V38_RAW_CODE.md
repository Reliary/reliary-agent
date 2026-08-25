# V38: Raw Code Output, Not Synthesized Answers

## Core Insight

The model trusts **raw code**, not **synthesized claims**. Grep wins because it shows actual source code. Our tools fail because we return claims the model overrides with training data.

## Change

Every tool output changes from "The answer is: X" to raw code snippets with file:line prefixes. Same Rust code, same params, same tool surface. Only output format.

## Output Format

### def_only=true (replaces goto_def)
```
runtime.rs:340:    pub fn block_on<F: Future>(&self, future: F) -> F::Output {
runtime.rs:341:        let fut_size = mem::size_of::<F>();
runtime.rs:343:        self.block_on_inner(future)
```

### usage_only=true (replaces call_graph inbound)
```
local.rs:677:    pub fn block_on<F: Future>(&self, future: F) -> F::Output {
handle.rs:341:    pub fn block_on<F: Future>(&self, future: F) -> F::Output {
```

### path_filter="io/util/" (find implementations)
```
take.rs:121:    fn consume(&mut self, amt: usize) {
empty.rs:89:    fn consume(&mut self, _: usize) {
chain.rs:128:    fn consume(&mut self, amt: usize) {
buf_writer.rs:284:    fn consume(&mut self, amt: usize) {
buf_stream.rs:194:    fn consume(&mut self, amt: usize) {
```

### methods=true (list methods on type)
```
sleep.rs:299:    pub fn far_future(location: Option<&'static Location<'static>>) -> Sleep {
sleep.rs:225:    pub fn new_timeout(duration: Duration) -> Sleep {
sleep.rs:243:    pub fn deadline(&self) -> Instant {
sleep.rs:253:    pub fn is_elapsed(&self) -> bool {
sleep.rs:266:    pub fn reset(&mut self, new_dur: Duration) {
sleep.rs:278:    pub fn poll_elapsed(&mut self, cx: &mut Context<'_>) -> Poll<Result<...>> {
```

### dead_only=true (find dead code)
```
take.rs:45:    fn set_limit(&mut self, limit: usize) {
copy.rs:19:    fn buf_s(&self) -> &str {
split.rs:61:    fn next_seg(&mut self) -> Option<&str> {
```

## System Prompt Change

Remove "The answer is:" instruction. Replace with:
```
Your tools return source code snippets with file:line prefixes.
Read the code and answer the question using ONLY what you see.
Do NOT add files, line numbers, or types not shown in the tool output.
```

## Implementation

1. Change output format in find_references (def_only, usage_only, path_filter) to show `file:line: source` instead of "The answer is: ..."
2. Change output format in describe (methods, dead_only) to show raw code instead of synthesized names
3. Update system prompt
4. Build, test, bench

## Expected Impact

| Metric | V37 | V38 target | Why |
|--------|-----|-----------|-----|
| Judge | 12.5 | **18-22** | Model trusts raw code over claims |
| Variance | ±2 | **±1** | Unambiguous evidence forces convergence |
| Dead-ends | 12 | **3-5** | Model gets useful data every call |
| WC | 192k | **120-150k** | Fewer dead-end calls |
