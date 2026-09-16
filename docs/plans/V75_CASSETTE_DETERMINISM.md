# V75 — Deterministic transcripts for a stochastic LLM

## The honest problem

A remote MoE provider's sampler cannot be forced deterministic from the client.
Probed against the live DeepSeek API (2026-09-16), not read from docs:

| Lever | Observed |
|---|---|
| `temperature=0.0` (already set) | high-entropy probe: 2 distinct outputs in 5 runs |
| `seed=<int>` | accepted, **not echoed** in the response, **no observable effect** |
| `response_format: json_object` | accepted, **not enforced** (prose passes through) |
| `response_format: json_schema` | rejected: "This response_format type is unavailable now" |
| `logprobs` / `top_logprobs` | **works**, populated with alternatives |
| `system_fingerprint` | returned, stable across calls (`aeb56401…`) |

Bench variance structure (v70 3-way, condition A, 4 seeds):

| Metric | CV | Meaning |
|---|---|---|
| score | 0.019 | answers are already near-deterministic |
| tool calls / billed / wall | 0.14 | the noise is **explore-vs-answer**, not answer content |
| first divergence | q2 (seed 456), q5 (seeds 42/123) | identical `tokens_in` before it |

Conclusion: stop chasing the sampler. Force the **transcript**.

## Design

**`bench/cassette.py`** — record/replay, bench-only (never in the shipped binary).

- **Key** = SHA-256 over canonical JSON of the *sampling-affecting* request:
  `{model, messages, max_tokens, temperature, thinking, response_format,
  disable_thinking, logprobs, top_logprobs}` **plus** `cassette_version` and
  `index_gen` (the corpus's persisted `meta.index_gen`). Rationale: tool output
  already embeds the index stamp `[idx:xxxxxxxx]`; a reindex must invalidate
  entries. `index_gen` is read read-only from `.reliary/index.sqlite`.
- **Modes** (`RELIARY_CASSETTE_MODE`):
  - `record` — always live, append every response.
  - `replay-strict` — replay only; a miss **raises** with a diagnostic
    (which key component changed). Never silently calls live.
  - `auto` — replay if present, else live + append. Dev default.
- **Storage** — `<dir>/cassette.jsonl`, one JSON object per entry. The body
  only (messages + response). No `Authorization` header is ever part of the
  request body, and a test asserts no key material is present.
- **Wire-in** — inside `llm_conn.deepseek_chat` via `RELIARY_CASSETTE=<dir>`.
  A pluggable live transport (`set_live_transport`) lets tests run with zero
  network. Disabled by default: existing benches behave identically.
- **Flip-risk (Layer 2)** — in cassette mode, requests add
  `logprobs: true, top_logprobs: 3` (report-only; does not affect sampling).
  Per entry, record the worst top1−top2 margin across tokens and the margin at
  the first token. `python3 bench/cassette.py summarize <dir>` prints turns
  below a threshold — so variance is attributed to named near-ties, not
  hand-waved.

## Why this is the strongest honest claim

Tool outputs are already byte-deterministic (V56/V64). Therefore two runs
sharing a message prefix consume **literally identical recorded decisions**
until their paths diverge. "Same input → same decision" becomes provable
rather than statistical, and any divergence is attributable to the exact turn
it occurred. Replay costs zero API calls — verification becomes free.

## Acceptance tests (each with a negative control)

1. record → replay twice ⇒ responses byte-identical (timing fields excluded).
2. strict + unknown request ⇒ raises; live transport call count is 0.
3. mutate one message byte ⇒ strict miss.
4. change `index_gen` ⇒ strict miss (negative control: drop it from the key ⇒ test fails).
5. change `temperature` ⇒ strict miss (negative control: drop it from the key ⇒ test fails).
6. cassette file contains no key material (`sk-`, `Authorization`).
7. prefix pairing: identical conversation prefix ⇒ identical replayed decisions.
8. record captures `system_fingerprint`; replay preserves it.

## Non-goals (explicit)

- Claiming `seed` works, or that the sampler is determinized. It is not.
- Local models (wrong target; "use a real LLM").
- Majority-vote N× sampling (cost), per-query prompt tightening (bench-fitting),
  any history rewriting (breaks KV cache).

## Rollout

1. `bench/cassette.py` + `bench/tests/test_cassette.py` (pytest).
2. Wire `long_session_bench.py`: configure label+index_gen, record cassette
   stats (hits/misses/flip-risk) into each session row.
3. `docs/cassette.md` (or README section) with the honest claims table.
4. `replay-strict` for A/B comparisons; `auto` for development.
5. Commit public-corpus cassettes only if ≤ a few MB (measure first);
   private-corpus tapes stay local (gitignored).
