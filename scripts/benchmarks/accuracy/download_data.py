"""Download and cache BFCL + SQuAD v2 test data for accuracy benchmarks.

Output:
  /tmp/bench_bfcl/bfcl_100.json   (100 function-calling samples)
  /tmp/bench_squad/squad_100.json (100 reading-comprehension samples)

BFCL source: NousResearch/hermes-function-calling-v1 (open, no auth required)
  Schema: {query, tools: [list of function defs], expected_calls: [{name, arguments}]}
SQuAD v2 source: rajpurkar/squad_v2 (open)
  Schema: {context, question, expected_text, is_unanswerable}
"""
import json
import re
from pathlib import Path

from datasets import load_dataset

OUT_BFCL = Path("/tmp/bench_bfcl/bfcl_100.json")
OUT_SQUAD = Path("/tmp/bench_squad/squad_100.json")
N = 100


def parse_hermes_tool_calls(gpt_value: str) -> list:
    """Extract tool_call JSON objects from Hermes <tool_call>...</tool_call> blocks."""
    pattern = r"<tool_call>\s*(\{.*?\})\s*</tool_call>"
    matches = re.findall(pattern, gpt_value, re.DOTALL)
    calls = []
    for m in matches:
        try:
            obj = json.loads(m)
            if isinstance(obj, dict) and "name" in obj:
                calls.append({
                    "name": obj.get("name", ""),
                    "arguments": obj.get("arguments", {}),
                })
        except json.JSONDecodeError:
            continue
    return calls


def load_bfcl():
    """Load 100 BFCL samples from NousResearch/hermes-function-calling-v1.

    Filters to single-tool-call samples for cleanest scoring.
    """
    print("Loading BFCL (NousResearch/hermes-function-calling-v1) ...")
    ds = load_dataset("NousResearch/hermes-function-calling-v1", split="train")
    samples = []
    for row in ds:
        try:
            tools_raw = row.get("tools")
            if isinstance(tools_raw, str):
                tools = json.loads(tools_raw)
            else:
                tools = tools_raw
            convs = row.get("conversations", [])
            if not tools or not convs:
                continue
            query = None
            expected_calls = []
            for c in convs:
                if not isinstance(c, dict):
                    continue
                if c.get("from") == "human":
                    query = c.get("value", "")
                elif c.get("from") == "gpt":
                    expected_calls = parse_hermes_tool_calls(c.get("value", ""))
                    break
            if not query or not expected_calls or not tools:
                continue
            if not isinstance(tools, list) or len(tools) == 0:
                continue
            samples.append({
                "query": query,
                "tools": tools,
                "expected_calls": expected_calls,
            })
            if len(samples) >= N:
                break
        except (json.JSONDecodeError, KeyError, TypeError):
            continue
    return samples


def load_squad():
    """Load 100 SQuAD v2 samples (50 answerable, 50 unanswerable)."""
    print("Loading SQuAD v2 (rajpurkar/squad_v2) ...")
    try:
        ds = load_dataset("rajpurkar/squad_v2", split="validation")
    except Exception as e:
        print(f"  squad_v2 failed ({e}), falling back to squad v1")
        ds = load_dataset("rajpurkar/squad", split="validation")
    samples = []
    answerable_count = 0
    unanswerable_count = 0
    target_answerable = N // 2
    target_unanswerable = N - target_answerable
    for row in ds:
        answers = row.get("answers", {})
        text_list = answers.get("text", [])
        if text_list and answerable_count < target_answerable:
            samples.append({
                "context": row["context"],
                "question": row["question"],
                "expected_text": text_list[0],
                "is_unanswerable": False,
            })
            answerable_count += 1
        elif (not text_list) and unanswerable_count < target_unanswerable:
            samples.append({
                "context": row["context"],
                "question": row["question"],
                "expected_text": None,
                "is_unanswerable": True,
            })
            unanswerable_count += 1
        if len(samples) >= N:
            break
    return samples


def main():
    OUT_BFCL.parent.mkdir(parents=True, exist_ok=True)
    OUT_SQUAD.parent.mkdir(parents=True, exist_ok=True)

    if not OUT_BFCL.exists():
        bfcl = load_bfcl()
        with open(OUT_BFCL, "w") as f:
            json.dump(bfcl, f, indent=2)
        print(f"  Wrote {len(bfcl)} samples to {OUT_BFCL}")
    else:
        print(f"  {OUT_BFCL} already exists, skipping")

    if not OUT_SQUAD.exists():
        squad = load_squad()
        with open(OUT_SQUAD, "w") as f:
            json.dump(squad, f, indent=2)
        print(f"  Wrote {len(squad)} samples to {OUT_SQUAD}")
    else:
        print(f"  {OUT_SQUAD} already exists, skipping")

    print("Done.")


if __name__ == "__main__":
    main()
