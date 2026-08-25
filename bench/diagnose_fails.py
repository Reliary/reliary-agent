"""Diagnose specific failures in latest benchmark."""
import json

# Read all reliary failures
with open("results/multi_turn_20260630T225709Z.jsonl") as f:
    lines = [json.loads(l) for l in f]

print("=== block_on_chain failures ===")
for r in lines:
    if r["cond"] == "A" and r["task_id"] == "task_block_on_chain" and r["task_score"] < 3:
        print(f"seed={r['seed']} score={r['task_score']}")
        print(f"Answer: {r['final_answer'][:400]}")
        print(f"dead_ends={r.get('dead_end_calls',0)} calls={r.get('tool_calls',0)}")
        print()

print("=== split_return_type failures ===")
for r in lines:
    if r["cond"] == "A" and r["task_id"] == "task_split_return_type" and r["task_score"] < 3:
        print(f"seed={r['seed']} score={r['task_score']}")
        print(f"Answer: {r['final_answer'][:400]}")
        print()

print("=== bufwriter failures ===")
for r in lines:
    if r["cond"] == "A" and r["task_id"] == "task_bufwriter_write_chain" and r["task_score"] < 3:
        print(f"seed={r['seed']} score={r['task_score']}")
        print(f"Answer: {r['final_answer'][:400]}")
        print(f"dead_ends={r.get('dead_end_calls',0)} calls={r.get('tool_calls',0)}")
        print()
