"""Diagnose reliary failures from latest benchmark."""
import json
from multi_turn_harness import score_answer, TASKS

print("=== Rubrics ===")
for t in TASKS:
    r = t["rubric"]
    if "accept" in r:
        print(f"  {t['id']}: accept={r['accept']}")
    if "accept_keywords" in r:
        print(f"  {t['id']}: accept_keywords={r['accept_keywords']}")

print("\n=== Failed Reliary Runs ===")
with open("results/multi_turn_20260630T223447Z.jsonl") as f:
    for line in f:
        r = json.loads(line)
        if r["cond"] == "A" and r["task_score"] < 3:
            task = next(t for t in TASKS if t["id"] == r["task_id"])
            rubric = task["rubric"]
            ans = r["final_answer"]
            score = r["task_score"]

            print(f"\n{r['task_id']} seed={r['seed']} score={score}")
            print(f"  Answer: {ans[:300]}")

            if "accept" in rubric:
                found = [e for e in rubric["accept"] if e.lower() in ans.lower()]
                missing = [e for e in rubric["accept"] if e.lower() not in ans.lower()]
                print(f"  accept: {len(found)}/{len(rubric['accept'])} found, missing={missing}")
            if "accept_keywords" in rubric:
                found = [kw for kw in rubric["accept_keywords"] if kw.lower() in ans.lower()]
                missing = [kw for kw in rubric["accept_keywords"] if kw.lower() not in ans.lower()]
                print(f"  keywords: {len(found)}/{len(rubric['accept_keywords'])} found, missing={missing}")

            # Check dead-end calls
            dead_ends = r.get("dead_end_calls", 0)
            tool_calls = r.get("tool_calls", 0)
            print(f"  dead_ends={dead_ends}/{tool_calls} calls")

            # Check tool outputs
            print(f"  wall_time={r['wall_time']:.1f}s turns={r['turns']} wc={r['weighted_cost']}")
