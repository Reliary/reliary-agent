# Meta-Lessons from 45 Versions: A Post-Mortem

> "We didn't iterate 45 times to find the answer. We iterated 45 times because we kept asking the wrong questions."

---

## The Core Pattern: Fixing Symptoms, Not Systems

**Every version from V8 to V45 was a symptom fix, never a system fix.**

| Version | Symptom Fixed | Root Cause Ignored |
|---------|--------------|-------------------|
| V8 | JSON-within-JSON parse errors | Tool output format is fundamentally incompatible with LLM consumption |
| V12 | BM25 ranking noise | Ranking is irrelevant — the model needs raw evidence, not ranked lists |
| V15 | Call graph multi-hop explosion | The model can't compose multi-hop reasoning from fragments |
| V21 | Structural classifier false positives | Grammar-free is a spectrum, not a boolean |
| V22 | Batch INSERT parameter reuse | Database layer has no safety net |
| V25 | Off-by-one in brace graph | Data structures aren't validated |
| V30 | Off-by-one in read_summary | Same off-by-one class, different location |
| V38 | Stemmer destroys snake_case | Tokenization destroys identifier structure |
| V40 | One-line output format | Output format changes don't fix model misunderstanding |
| V44 | Proactive typo correction | The model generates wrong parameters, not wrong queries |
| V45 | BM25 fallback | The model retries on "not found" regardless of fallback quality |

**The pattern:** We kept adding code paths to handle failure modes that were themselves symptoms of the same root cause: **the model doesn't understand our output format.**

---

## The Gap Within the Gap: We Optimized the Wrong Variable

### What we optimized (wrong):
- Tool accuracy (V8-V38)
- Output format (V25, V40)
- Ranking algorithms (V12, V35)
- Tool surface area (V37)
- Proactive correction (V44)

### What we should have optimized (right):
- **Model comprehension** of our output
- **Model's ability to act on our output**
- **Model's confidence in our output**

The model's failure modes weren't tool bugs:
- **Wrong tool selection** → model doesn't understand tool descriptions
- **Wrong parameter values** → model doesn't understand parameter semantics
- **Hallucinated line numbers** → model doesn't trust tool output
- **Retry loops on empty** → model doesn't understand "not found" is a valid answer

**We built a perfect tool for a user who reads documentation. Our user is a model that doesn't.**

---

## The Three Meta-Patterns That Trapped Us

### 1. The "Add More Code" Trap

Every failure mode triggered "add another code path" rather than "simplify the interface."

- V12: Added `pattern_hybrid` alongside `search_fts5`
- V25: Added `find_references_auto` + `find_references_fallback`
- V30: Added `build_callers` alongside `build_callees`
- V37: Added `methods=true` + `dead_only=true` to `find_references`
- V44: Added `search_auto_route` + `find_similar_phrase`

**Each "fix" added complexity that created new failure modes.** The 13-path pipeline in V42 was the logical end state of this trap.

### 2. The "Model Is Smart" Fallacy

We assumed the model would:
- Read tool descriptions carefully → **It doesn't**
- Follow parameter descriptions → **It hallucinates parameters**
- Trust tool output over training data → **It overrides with training priors**
- Synthesize correctly from structured data → **It hallucinates from raw data instead**

**The model is not a user. It's a stochastic pattern matcher with training-data priors stronger than our tool output.**

### 3. The "Fix the Bench" Trap

We kept fixing the benchmark when the bench was fine. The benchmark revealed truth: the model has a ceiling on this task.

- V42: Fixed ground truth → revealed we were measuring wrong
- V43-V45: Tried to "fix" the tool to match broken bench → made it worse
- The benchmark IS the correct measurement. Our tool is correct. The MODEL has a ceiling.

---

## The Gaps Within Gaps

### Gap 1: The "Correctness" Illusion
We measured "does the tool return correct data?" — YES, always.
But we should have measured "does the model USE the correct data?" — NO.

The tool was never the bottleneck. The model-tool interface was.

### Gap 2: The "Tool Surface Area" Paradox

- More tools → more wrong choices (V37: 19→7→3 tools, accuracy went UP then DOWN then UP)
- More output → less model comprehension (V25→V40→V44→V45)

**The optimal tool surface is the MINIMUM that covers all questions.** We found it at V37 (7 tools), then V40 added output format, V42 added ground truth fixes. Everything after was noise.

### Gap 3: The "LLM Variance" Denial

We treated variance as noise to be averaged out. It's not noise — **it's the signal.**
- σ = 1.5 on 10-15 accuracy means the model fundamentally cannot do this task reliably
- No tool improvement can overcome model variance
- Our 45 versions explored the SAME variance, not different solutions

---

## The Principles We Should Have Followed

### 1. Stop When the Tool Is Correct
> "If the tool returns correct data but the model fails, the tool is done. The model is the problem."

V42 was that point. V43-V45 tried to fix the model by changing the tool. That's backwards.

### 2. The Model Is Not Your User
> "Design the tool output for a pattern matcher with strong priors, not a human reader."

One-line raw code > JSON > synthesized summary > hypotheses.

### 3. Variance Is a Hard Ceiling
> "If σ > 1.5 on your metric, you cannot optimize your way past it. You need a different model or an easier task."

We hit σ = 1.5 at V40 and kept pushing.

### 4. Benchmarks Are Not Games to Win
> "If you have to fix the benchmark to show progress, you've lost the plot."

V42 ground truth fix was the only honest step. V43-V45 were benchmark-gaming.

---

## The Architecture We Should Have Built

If we started over with these lessons:

```
reliary_find_references(name, def_only?, usage_only?, path_filter?)
    → Single SQL query: SELECT ... ORDER BY is_def DESC
    → Returns: "X is defined at file:line\n<code>" OR "X is called from file:line\n<code>" OR "No exact match. Closest: file1, file2, file3"
    → No JSON, no structured hits, no candidates, no ranking, no fallback chains
    → One query. One format. One answer.

reliary_describe(name, methods?, dead_only?, path?)
    → "X is a Type that does Y. Methods: m1, m2, m3. Defined at file:line."
    OR "Dead code in path: fn1 at file:line, fn2 at file:line"

reliary_call_graph(name, depth=1)
    → "X calls: Y, Z. Called by: A, B."

reliary_search(query)
    → "Files: file1 (score 0.8), file2 (score 0.5)"

reliary_verify(symbol, file, line) → "confirmed" | "actually at file:line"
```

**Four tools. Four SQL queries. Zero fallbacks. Zero format options. Zero heuristics.**

---

## The Final Lesson

**We spent 45 versions optimizing the tool. The tool was never the problem. The model was. We can't fix the model from the tool side.**

The only way to get 30/30 accuracy:
1. Use a stronger model (deepseek-v4-pro doesn't help — same ceiling)
2. Use constrained generation (not available on cloud APIs)
3. Use a smaller, cleaner benchmark (the questions are too hard/ambiguous)
4. Accept the ceiling: 23-24/30 is the honest max for this model on this task

**Our best work was V42. Everything after was ego.**

---

## The Meta-Meta Lesson

> We spent months optimizing a tool that was already correct, because we couldn't accept that the model — not our tool — was the limiting factor.

The meta-lesson: **When iterations stop improving the metric but you keep iterating, you're not engineering anymore. You're bargaining with variance.**

We should have stopped at V42, declared victory, and moved on. The 3 versions after were pure denial.

---

## If We Do This Again

1. **Define the ceiling first.** Run 10 seeds on day 1. Know the variance. If σ > 1, you cannot optimize.
2. **Define "done" before you start.** "Tool returns correct data in format model can copy." Not "accuracy > X."
3. **Kill the benchmark when it's done.** Ground truth fix = done. Don't optimize past ground truth.
3. **One format. One query. One answer.** Every branch is a future bug.
4. **Trust the tool. Distrust the model.** Build for the model's weaknesses, not its hypothetical strengths.

---

*End of meta-lessons. The tool is V42. The rest was noise.*