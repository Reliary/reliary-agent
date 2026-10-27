# Code-retrieval literature, October 2026

Fetched from arXiv on 2026-10-26. Numbers are quoted from each abstract, not
re-measured here. This file records **what changes our decisions**, and what
does not.

## Papers that confirm a decision we already made

### SkillSeek (arXiv 2609.38822, AACL-IJCNLP 2026)
Two-stage skill retrieval over a 230K-skill marketplace.

- "plain bm25 alone records a pass rate at or above their refined loop on three
  of four settings"
- "A first-stage recall ceiling explains the pattern"
- Spend per trial falls from USD 51.30 to USD 27.54

This is our thesis (lexical first stage, deterministic, cheap) independently
measured by another group in 2026. It also names the failure mode to guard
against: an LLM-mediated retriever cannot recover what first-stage recall did
not surface. Our own history agrees — V40-V53 spent months rewriting output
format when the ceiling was upstream.

### Cartograph (arXiv 2609.30293)
Federated MCP proxy over 374 tools.

- exposes 3 proxy tools instead of 374 definitions
- top-5 discovery exchange: 475 tokens vs 42,450 full catalog
- R@5 0.816 vs 0.592 Jaccard baseline
- gateway overhead +5 ms mean (0.8%)

Confirms the 7-tool consolidation: fewer, better-described entry points win on
tokens *and* on selection. Also suggests tool descriptions should carry
provenance, which we already emit as `[idx:...]`.

### MemoryTax (arXiv 2610.29823)
- "Agent memory is a costly patch for a weak model"
- "a stronger reader does not recover what a weaker writer left out"
- with a strong model, a plain coding agent "tops the cost-accuracy frontier,
  scoring 99% for less than half the cost of full context"

Confirms the decision not to build cross-session memory. The memory crate stays
unwired.

## Papers that predict a change is worthless — test anyway

### Adaptive q-log IDF (arXiv 2605.18561)
Replaces the outer logarithm of RSJ odds with a q-logarithm.

- CoIR CodeSearchNet Go (182K docs): NDCG@10 0.2575 -> 0.4874, oracle-tuned q
- "A tokenizer ablation shows that identifier-aware tokenization largely
  removes the incremental gain from q-IDF."

We are identifier-aware by construction. The paper's own ablation predicts ~0
for us. This is a cheap experiment and a likely negative result; it is recorded
here so nobody spends a week on it later believing it is untested.

## Papers that predict a change is worth making

### Name-only directory routing (arXiv 2609.35918)
82 audited issues, 11 repositories at pinned pre-fix commits.

| method | gold-file recall @8 |
|---|---:|
| name-only directory routing (LLM) | 0.465 |
| SQLite FTS5 | 0.352 |
| fixed `rg` query | 0.245 |

- paired gain over FTS5: **+0.113** (95% bootstrap CI 0.053-0.168)
- under a 16K-token budget, 0.443 of annotated lines vs 0.246 for FTS5
- honesty in the abstract: "the study cannot attribute the gain to hierarchy or
  show that it improves issue resolution"

Our index already stores `file_map.file_path` and already has a `path_rank`
function — but it only *demotes* tests/docs/bench. Path segments are not a
retrieval signal at all. That is the gap this paper points at.

### RepoNav (arXiv 2609.08355, EMNLP 2026)
Post-retrieval interface that reorganises snippets into a file-centred
navigation scaffold.

- "flat lists of isolated snippets ... provide insufficient structure for agents
  to distinguish the target function from semantically similar alternatives in
  the same file"
- controlled ablations: "gains come from structured evidence organization rather
  than simply exposing additional file structure"

This is the strongest available argument for what our `find_references` already
does (one-line answer + raw evidence) and for going further: when several
candidates live in one file, list the siblings so the model can pick.

### SMIE (arXiv 2006.03103, Programming 2021)
Emacs's Simple-Minded Indentation Engine.

- "SMIE ... gets its power from the weakness of the underlying parsing
  technique. It makes it possible to perform local parsing, which is hence
  unaffected by irrelevant surrounding code. This provides a form of graceful
  degradation in the face of incomplete, erroneous, or just plain problematic
  information."

This is the formal justification for grammar-free analysis, and specifically for
an indentation stack: local, degrading, no global parse. It is the citation the
project has been missing.

### Weave of Formal Thought (arXiv 2606.25987)
GLR + "speculative-lexing construction" to keep concurrent lexer hypotheses for
indentation-sensitive languages.

Even a Tree-sitter-spec-complete engine needs a special lexer construction for
Python-style layout. Evidence that indentation, not keywords, is the hard part.

### Structural Code Search using NL Queries (arXiv 2507.02107)
LLM translates natural language into Semgrep/GQL.
- 55-70% precision and recall over 400 queries / 10 Java projects
- "significantly outperforms baselines based on semantic code search and LLM
  retrievals by up to 57% and 14% on F1"

Structural beats semantic when structure is the question — but it needs a DSL,
which is the cost we avoid.

### MELT (arXiv 2308.14687, ASE 2023)
Mines API-migration rules from pull requests into **Comby**, a language-agnostic
structural search/replace language.
- 461 rules mined; generalisation increases matches 9x

Best precedent for grammar-free structural search as a *product*, not just a
concept.

### Semantic Code Clone Detection: Are We There Yet? (arXiv 2606.25272)
- 11 SOTA detectors across token/tree/graph paradigms, on distribution-shifted
  but semantically equivalent Type-4 clones
- "substantial performance degradation across all evaluated approaches"
- "detectors heavily rely on shortcut learning based on lexical and structural
  cues rather than robust semantic understanding"

Caution for `reliary_similar`: hypervector token-set similarity is exactly
"lexical and structural cues". The tool's output should be presented as
near-clone candidates, never as semantic equivalence.

## Papers that are a threat to the premise

### CodeScout (arXiv 2603.17829)
An RL-trained agent given **only a standard Unix terminal** reaches or beats
2-18x larger models with "specialized scaffolds" on SWE-Bench Verified/Pro/Lite.

If a trained agent with `grep` closes the gap, tool quality is not the binding
constraint in that setting. This is the strongest published argument against the
product thesis and it should be cited in the README rather than omitted.

### Recall Before Rerank (arXiv 2606.27401)
At terabyte scale, deep retrieval models' precision and scalability collapse;
LLM-based code normalisation and query rewriting recover part of it.

Supports a lexical first stage, and warns that "first-stage recall" is the real
budget.

### CORE-Bench (arXiv 2606.11864, EMNLP 2026)
- 180K queries, 106K broader-context relevance labels
- embedding models show "a sharp drop from traditional code search to code
  retrieval in agentic coding settings"

A better benchmark than our 10-question self-test, if we want external validity.

## Decision log

| idea | source | decision | gate |
|---|---|---|---|
| path segments as a retrieval signal | 2609.35918 | **build** | gold-file recall must rise on a mechanical path-query bench |
| q-log IDF | 2605.18561 | **test, expect ~0** | keep only if recall@1 rises outside noise |
| file-centred sibling list in search output | 2609.08355 | build after the path experiment | token cost must not rise more than ~15% |
| indent stack (SMIE justification) | 2006.03103 | remains gated on the rank-diff measurement | must replace, not add to, existing nesting sources |
| memory | 2610.29823 | **do not build** | — |
| semantic/embedding retrieval | 2606.27401, 2606.11864 | **do not build** | non-deterministic, cache-hostile |
| `similar` presented as semantic equivalence | 2606.25272 | **never claim** | — |
