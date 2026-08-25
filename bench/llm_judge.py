#!/usr/bin/env python3
"""LLM-as-judge: use deepseek-v4-pro to score answer correctness.

Usage:
    python3 bench/llm_judge.py --input bench/results/long_session_XXXXX.jsonl

Reads the 3-way bench results, sends each (question, ground_truth, model_answer)
triple to deepseek-v4-pro for scoring, and outputs a comparison table.
"""
import argparse, json, os, sys, time
sys.path.insert(0, os.path.dirname(__file__))
from llm_conn import deepseek_chat, DEEPSEEK_MODEL_PRO

JUDGE_MODEL = DEEPSEEK_MODEL_PRO



JUDGE_PROMPT = """You are a code intelligence evaluator. Score the model's answer for correctness against the ground truth.

Question: {question}

Ground truth (what a correct answer should contain):
{ground_truth}

Model's answer:
{model_answer}

Scoring criteria:
- 3/3: Answer correctly identifies the right symbols, files, and relationships. May have minor formatting differences but semantically correct.
- 2/3: Answer is mostly correct but misses 1-2 key symbols or has one wrong file reference.
- 1/3: Answer mentions some relevant symbols but has significant errors or misses most of the key findings.
- 0/3: Answer is wrong, empty, or hallucinated. No useful information.

Respond with ONLY a JSON object:
{{"score": <0-3>, "reason": "<one sentence explanation>"}}"""


# Ground truth descriptions (semantic, not file:line exact)
GROUND_TRUTH = {
    "q1_consume_impls": "The consume method is implemented on these types: Take, BufStream, BufWriter, Empty, Chain, BufReader (and possibly AsyncBufRead trait). Each is in io/util/ directory. The method consumes bytes from a pinned reader.",
    "q2_consume_callers": "consume is called from: BufWriter::poll_write (buf_writer.rs:285), Take::poll_fill_buf (take.rs:126), BufReader::poll_fill_buf (buf_reader.rs:117), BufStream::poll_write (buf_stream.rs:195), and Chain::poll_fill_buf (chain.rs:131,133). Each calls .consume(amt) on the consumer.",
    "q3_block_on_def": "Runtime::block_on is defined in runtime/runtime.rs:340 as pub fn block_on<F: Future>(&self, future: F). It runs a future on the runtime, blocking until completion.",
    "q4_block_on_chain": "The call chain from Runtime::block_on goes: block_on -> block_on_inner -> scheduler dispatch. The known callers of block_on_inner include Handle::block_on, BasicScheduler::block_on, LocalSet::block_on. The scheduler internals are in runtime/scheduler/. block_on delegates to block_on_inner which enters the scheduler.",
    "q5_spawn_callers": "Functions that call spawn include: JoinSet::spawn (join_set.rs:142), JoinSet::spawn_with_id (join_set.rs:722), LocalSet::spawn (local.rs:1028), task::spawn (spawn.rs:174), and Handle::spawn (handle.rs:197).",
    "q6_sleep_def": "Sleep is a struct in tokio/src/time/sleep.rs:225. It implements Future and waits until a deadline. Created via sleep(duration) or sleep_until(deadline).",
    "q7_sleep_methods": "Sleep methods: far_future (sleep.rs:299), deadline (304), is_elapsed (311), reset (344), reset_without_timer (385), poll_elapsed (391), poll (464). The poll method from Future impl is also a Sleep method.",
    "q8_bufwriter_write": "BufWriter::poll_write writes to an inner writer, buffering data. It calls the inner writer's poll_write. Defined in io/util/buf_writer.rs:284. Uses Pin<&mut Self> and calls self.consumer.consume(amt) to flush.",
    "q9_consume_impls_recheck": "Same as q1 — consume is implemented on Take, BufStream, BufWriter, Empty, Chain, BufReader in io/util/. These impls all follow the same pattern: consume(amt) advances the internal position.",
    "q10_dead_code": "Dead/unused code in io/util/: functions that are defined but never called from outside their module. Candidates include methods on Empty (poll_write is trivially empty), Take (some internal helpers), Chain helpers (poll_fill_buf, poll_write), and possibly BufReader helpers. The key is finding functions with zero cross-file references.",
}

# Question text (what was asked)
QUESTIONS = {
    "q1_consume_impls": "Which types in tokio's io/util module implement the consume method? List them with file paths.",
    "q2_consume_callers": "Where is consume called from? Find all call sites of the consume method in tokio.",
    "q3_block_on_def": "Where is Runtime::block_on defined? Show the definition.",
    "q4_block_on_chain": "What is the call chain from Runtime::block_on into the scheduler? Trace the callees.",
    "q5_spawn_callers": "Who calls spawn? Find all callers of the spawn function.",
    "q6_sleep_def": "Where is Sleep defined in tokio? Show the struct definition.",
    "q7_sleep_methods": "What methods does the Sleep type have? List them with line numbers.",
    "q8_bufwriter_write": "How does BufWriter::poll_write work? Show the implementation.",
    "q9_consume_impls_recheck": "Recheck: which types implement consume in io/util?",
    "q10_dead_code": "Find dead code (unused functions) in tokio's io/util module.",
}


# V58d: corpus-matched mode — MUST come after the tokio dicts above.
if os.environ.get("RELIARY_GT") == "1":
    from reliary_judge_gt import QUESTIONS as _RQ, GROUND_TRUTH as _RGT
    QUESTIONS = _RQ
    GROUND_TRUTH = _RGT

def judge_answer(question_id, model_answer):
    """Send (question, ground_truth, model_answer) to the judge model."""
    question = QUESTIONS.get(question_id, question_id)
    gt = GROUND_TRUTH.get(question_id, "unknown question")

    prompt = JUDGE_PROMPT.format(
        question=question,
        ground_truth=gt,
        model_answer=model_answer[:2000] if model_answer else "(empty)",
    )

    messages = [{"role": "user", "content": prompt}]
    try:
        response = deepseek_chat(messages, model=JUDGE_MODEL, max_tokens=200)
        if isinstance(response, dict) and "error" in response:
            return 0, f"api error: {response['error']}"
        # Extract text from response
        if isinstance(response, dict) and "choices" in response:
            text = response["choices"][0]["message"]["content"]
        elif isinstance(response, str):
            text = response
        else:
            text = str(response)
        # Parse JSON from the response text
        import re
        match = re.search(r'\{[^}]+\}', text)
        if match:
            result = json.loads(match.group())
            return result.get("score", 0), result.get("reason", "")
        # Fallback: look for a number
        match = re.search(r'(\d)', text)
        if match:
            return int(match.group(1)), text[:100]
        return 0, "parse failed"
    except Exception as e:
        return 0, f"error: {e}"


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--input", required=True, help="Path to long_session bench JSONL")
    parser.add_argument("--output", default=None, help="Output JSONL path")
    args = parser.parse_args()

    runs = []
    with open(args.input) as f:
        for line in f:
            runs.append(json.loads(line))

    results = []
    for run in runs:
        if "error" in run:
            continue
        cond = run.get("cond", "?")
        seed = run.get("seed", "?")
        queries = run.get("queries", [])
        kw_total = 0
        judge_total = 0
        per_query = []
        for q in queries:
            qid = q.get("query_id", "")
            ans = q.get("answer", "")
            kw = q.get("score", 0)
            kw_total += kw
            # Skip tool-call-only answers (no model text)
            if ans.startswith("{") and '"tool"' in ans[:20]:
                judge_score, reason = 0, "no final answer (tool call only)"
            else:
                judge_score, reason = judge_answer(qid, ans)
                time.sleep(0.5)  # rate limit
            judge_total += judge_score
            per_query.append({"qid": qid, "kw": kw, "judge": judge_score, "reason": reason})
            print(f"  {cond} seed={seed} {qid}: kw={kw} judge={judge_score} ({reason})")

        result = {
            "cond": cond, "seed": seed,
            "kw_total": kw_total, "judge_total": judge_total,
            "per_query": per_query,
        }
        results.append(result)
        print(f"  {cond} seed={seed}: kw={kw_total}/30 judge={judge_total}/30")

    # Summary
    print("\n=== SUMMARY ===")
    print(f"{'Backend':<12} {'Seed':<6} {'Keyword':<10} {'Judge':<10} {'Billed':<10}")
    print("-" * 50)
    for r in results:
        print(f"{r['cond']:<12} {r['seed']:<6} {r['kw_total']:<10} {r['judge_total']:<10}")

    if args.output:
        with open(args.output, "w") as f:
            for r in results:
                f.write(json.dumps(r) + "\n")


if __name__ == "__main__":
    main()