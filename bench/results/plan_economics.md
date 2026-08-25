# Holographic Pack: Plan Economics vs Raw API

## Key finding

Under flat-fee plans, the pack is a **free quality upgrade** — same cost, better answers.
Under raw API, the pack costs **2.5× more** ($0.0269 vs $0.0110 per 63-query session).

## Raw DeepSeek API (pay-per-token)

| Condition | Tokens/session | Real cost | % of A |
|---|---|---|---|
| A (no pack) | 1.7M tokens | $0.0110 | 100% |
| F (slice) | 4.5M tokens | $0.0269 | 244% |

F costs 2.4× more on raw API. Cache hit (98%) helps, but the uncached 2% still pays full price.

## Plans (flat fee, same regardless of tokens)

### Claude Pro ($20/mo)

- Per-message cap: ~200K tokens (well above F's ~73K/query, 63 queries = 63 messages)
- Tool calls don't count as separate messages
- Pack: same message count, better answers (+10% accuracy)
- **Verdict: Pure upside. Same $20/mo, 59% → 69% accuracy.**

### Claude Max ($200/mo)

- Same as Pro, 20× more usage headroom
- **Verdict: Same as Pro. Free upgrade.**

### ChatGPT Pro ($200/mo)

- Unlimited usage (no practical per-message cap)
- **Verdict: Pure upside. Same $200/mo, better accuracy.**

### ChatGPT Plus ($20/mo)

- Per-message cap: ~4-8K tokens (varies)
- F's ~73K tokens/query EXCEEDS this cap → truncated → degraded answers
- **Verdict: Pack is counterproductive. Don't use on Plus tier.**

### Cursor Pro ($20/mo)

- 500 fast requests/month + unlimited slow
- Pack doesn't change request count (1 user message = 1 request)
- 73K token messages may push more to "slow" tier
- **Verdict: Your mileage varies. Helps on quality, may slow down under cap pressure.**

### Devin ($2/hr compute)

- Tool-call reduction matters: F uses 49 calls/session vs A's 85
- Fewer tool calls → faster sessions → less hourly cost
- **Verdict: Helps. Pack pre-loads context, reducing tool-call overhead.**

## The real value: accuracy + speed

| Metric | A (no pack) | F (slice) | Improvement |
|---|---|---|---|
| Score | 111/189 (59%) | 131/189 (69%) | **+10.2%** |
| Wall time | 283s | 257s | **-9%** |
| Tool calls | 85 | 49 | **-42%** |
| Dead-ends | 17 | 10 | **-41%** |

The pack makes the model faster AND more accurate. Under plans where the marginal cost is zero, this is pure upside.

## Recommendation

| Use case | Condition | Rationale |
|---|---|---|
| Claude Pro/Max, ChatGPT Pro, Devin | **F (slice)** | Free upgrade — better answers, same cost |
| ChatGPT Plus | **A (no pack)** | Pack exceeds per-message token cap |
| Raw DeepSeek API, cost-sensitive | **A (no pack)** | Pack costs 2.4× more |
| Cursor Pro, tight token budget | **Client's choice** | Pack helps quality but may impact tier |
