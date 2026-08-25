"""Arc 28 Lever 6 — Aggregate v1 results into a summary."""
import json
import re
import statistics

with open('/home/user/src/reliary8/bench/results/compare_tasks.json') as f:
    tasks = {t['id']: t for t in json.load(f)}

results = []
with open('/home/user/src/reliary8/bench/results/compare_backends_20260629T100232Z.jsonl') as f:
    for line in f:
        d = json.loads(line)
        results.append(d)


def parse_llm_json(content):
    content = (content or '').strip()
    if not content:
        return None
    content = re.sub(r'^```(?:json)?\s*', '', content)
    content = re.sub(r'\s*```\s*$', '', content)
    content = content.strip()
    try:
        return json.loads(content)
    except Exception:
        pass
    for m in re.finditer(r'\{[\s\S]*?\}', content):
        try:
            return json.loads(m.group(0))
        except Exception:
            continue
    return None


a_runs = [r for r in results if r['condition'] == 'A' and r['returncode'] == 0]
b_runs = [r for r in results if r['condition'] == 'B' and r['returncode'] == 0]
a_timeout = sum(1 for r in results if r['condition'] == 'A' and r['returncode'] != 0)
b_timeout = sum(1 for r in results if r['condition'] == 'B' and r['returncode'] != 0)
print(f"cond A: {len(a_runs)} ok, {a_timeout} timeout")
print(f"cond B: {len(b_runs)} ok, {b_timeout} timeout")

print("\nNon-zero jaccards:")
for r in results:
    if r.get('jaccard', 0) > 0:
        print(f"  {r['task_id']} cond={r['condition']} jaccard={r['jaccard']:.3f} preds={len(r['predictions'])}")

print("\nTool call counts (median by condition):")
a_tc = [r.get('tool_calls', 0) for r in a_runs]
b_tc = [r.get('tool_calls', 0) for r in b_runs]
if a_tc:
    print(f"  A: median={statistics.median(a_tc):.0f}, mean={statistics.mean(a_tc):.1f}, max={max(a_tc)}, min={min(a_tc)}")
if b_tc:
    print(f"  B: median={statistics.median(b_tc):.0f}, mean={statistics.mean(b_tc):.1f}, max={max(b_tc)}, min={min(b_tc)}")

print("\nWeighted cost (median by condition):")
a_wc = [r.get('weighted_cost', 0) for r in a_runs]
b_wc = [r.get('weighted_cost', 0) for r in b_runs]
if a_wc:
    print(f"  A: median={statistics.median(a_wc):.0f}, mean={statistics.mean(a_wc):.1f}")
if b_wc:
    print(f"  B: median={statistics.median(b_wc):.0f}, mean={statistics.mean(b_wc):.1f}")

print("\nElapsed time (median seconds):")
a_el = [r.get('elapsed', 0) for r in a_runs]
b_el = [r.get('elapsed', 0) for r in b_runs]
if a_el:
    print(f"  A: median={statistics.median(a_el):.1f}s, mean={statistics.mean(a_el):.1f}s")
if b_el:
    print(f"  B: median={statistics.median(b_el):.1f}s, mean={statistics.mean(b_el):.1f}s")

print("\nPer-task detail:")
for task_id in sorted(set(r['task_id'] for r in results)):
    a = [r for r in results if r['task_id'] == task_id and r['condition'] == 'A']
    b = [r for r in results if r['task_id'] == task_id and r['condition'] == 'B']
    a_str = f"A j={a[0].get('jaccard', 0):.2f} tc={a[0].get('tool_calls', 0)} wc={a[0].get('weighted_cost', 0)}" if a else "A N/A"
    b_str = f"B j={b[0].get('jaccard', 0):.2f} tc={b[0].get('tool_calls', 0)} wc={b[0].get('weighted_cost', 0)}" if b else "B N/A"
    print(f"  {task_id:25s} | {a_str:40s} | {b_str}")