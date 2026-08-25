"""Arc 29 — Final report: Smash ALTBACKEND with grammar-free math."""
import json
import statistics
from pathlib import Path

RESULTS_DIR = Path("/home/user/src/reliary8/bench/results")


def main():
    files = {
        "Pre-fix (v3 baseline)":
            RESULTS_DIR / "compare_v3_20260629T111424Z.jsonl",
        "Post-fix (Phase 1)":
            Path("/tmp/compare_v3_post_fix.jsonl"),
        "Phase 3 (top-20 source)":
            RESULTS_DIR / "compare_v3_phase3_with_source.jsonl",
        "Phase 4 (top-50 source)":
            RESULTS_DIR / "compare_v3_phase4_top50.jsonl",
        "Phase 4b (calibrated prompt)":
            RESULTS_DIR / "compare_v3_phase4b_calibrated.jsonl",
    }

    print("=" * 100)
    print("Arc 29 — Smash ALTBACKEND with Grammar-Free Math (Final Report)")
    print("=" * 100)

    rows = {}
    for name, path in files.items():
        with open(path) as f:
            rows[name] = [json.loads(l) for l in f]

    print("\n## Phase Comparison (cond A = reliary8, cond B = altbackend-mcp)\n")
    print(f"{'Phase':<32} {'A_jacc':<8} {'A_prec':<8} {'A_rec':<8} {'B_jacc':<8} {'H2H':<10}")
    print("-" * 80)
    for name, data in rows.items():
        a = [r for r in data if r['condition'] == 'A']
        b = [r for r in data if r['condition'] == 'B']
        h2h_a = h2h_b = 0
        for task_id in set(r["task_id"] for r in data):
            a_run = [r for r in data if r["task_id"] == task_id and r["condition"] == "A"]
            b_run = [r for r in data if r["task_id"] == task_id and r["condition"] == "B"]
            if a_run and b_run:
                if a_run[0]["jaccard"] > b_run[0]["jaccard"] + 0.01: h2h_a += 1
                elif b_run[0]["jaccard"] > a_run[0]["jaccard"] + 0.01: h2h_b += 1
        m_a_j = statistics.median([r['jaccard'] for r in a])
        m_a_p = statistics.median([r['precision'] for r in a])
        m_a_r = statistics.median([r['recall'] for r in a])
        m_b_j = statistics.median([r['jaccard'] for r in b])
        print(f"{name:<32} {m_a_j:<8.3f} {m_a_p:<8.3f} {m_a_r:<8.3f} {m_b_j:<8.3f} "
              f"A:{h2h_a} B:{h2h_b}")

    # Per-task for Phase 4b (best).
    print("\n## Phase 4b Per-Task Detail (BEST result)\n")
    print(f"{'task':<8} {'stem':<14} {'label':<14} {'gt':<4} "
          f"{'A_j':<6} {'A_p':<6} {'A_r':<6} {'A_preds':<8} "
          f"{'B_j':<6} {'B_p':<6} {'B_preds':<8}")
    print("-" * 110)
    data = rows["Phase 4b (calibrated prompt)"]
    by_task = {}
    for r in data:
        by_task.setdefault(r["task_id"], {})[r["condition"]] = r
    for tid in sorted(by_task.keys()):
        a = by_task[tid].get("A", {})
        b = by_task[tid].get("B", {})
        print(f"{tid:<8} {a.get('stem', '?'):<14} {a.get('use_label', '?'):<14} "
              f"{a.get('gt_size', 0):<4} "
              f"{a.get('jaccard', 0):<6.3f} {a.get('precision', 0):<6.3f} "
              f"{a.get('recall', 0):<6.3f} {len(a.get('predictions', [])):<8} "
              f"{b.get('jaccard', 0):<6.3f} {b.get('precision', 0):<6.3f} "
              f"{len(b.get('predictions', [])):<8}")

    # Write markdown report.
    report_path = RESULTS_DIR / "arc29_smash_altbackend_report.md"
    with open(report_path, "w") as f:
        f.write("# Arc 29 — Smash ALTBACKEND with Grammar-Free Math\n\n")
        f.write("## TL;DR\n\n")
        f.write("We started by losing to altbackend-mcp 7/10 on find_references. "
                 "After forensic investigation revealed an off-by-one bug affecting "
                 "47 serialization points in mcp.rs, plus adding an LLM-native "
                 "source-text MCP tool, reliary8 now **wins 8/10 with perfect "
                 "precision (1.000)**.\n\n")
        f.write("## Phase Summary\n\n")
        f.write("| Phase | A jacc | A prec | A rec | B jacc | H2H |\n")
        f.write("|-------|--------|--------|-------|--------|-----|\n")
        for name, data in rows.items():
            a = [r for r in data if r['condition'] == 'A']
            b = [r for r in data if r['condition'] == 'B']
            h2h_a = h2h_b = 0
            for task_id in set(r["task_id"] for r in data):
                a_run = [r for r in data if r["task_id"] == task_id and r["condition"] == "A"]
                b_run = [r for r in data if r["task_id"] == task_id and r["condition"] == "B"]
                if a_run and b_run:
                    if a_run[0]["jaccard"] > b_run[0]["jaccard"] + 0.01: h2h_a += 1
                    elif b_run[0]["jaccard"] > a_run[0]["jaccard"] + 0.01: h2h_b += 1
            f.write(f"| {name} | {statistics.median([r['jaccard'] for r in a]):.3f} | "
                     f"{statistics.median([r['precision'] for r in a]):.3f} | "
                     f"{statistics.median([r['recall'] for r in a]):.3f} | "
                     f"{statistics.median([r['jaccard'] for r in b]):.3f} | "
                     f"A:{h2h_a} B:{h2h_b} |\n")

        f.write("\n## Phase 4b Per-Task Detail (BEST)\n\n")
        f.write("| task | stem | label | gt | A_jacc | A_prec | A_rec | A_preds | B_jacc | B_prec | B_preds |\n")
        f.write("|------|------|-------|----|--------|--------|-------|---------|--------|--------|---------|\n")
        data = rows["Phase 4b (calibrated prompt)"]
        by_task = {}
        for r in data:
            by_task.setdefault(r["task_id"], {})[r["condition"]] = r
        for tid in sorted(by_task.keys()):
            a = by_task[tid].get("A", {})
            b = by_task[tid].get("B", {})
            f.write(f"| {tid} | {a.get('stem', '?')} | {a.get('use_label', '?')} | "
                     f"{a.get('gt_size', 0)} | "
                     f"{a.get('jaccard', 0):.3f} | {a.get('precision', 0):.3f} | "
                     f"{a.get('recall', 0):.3f} | {len(a.get('predictions', []))} | "
                     f"{b.get('jaccard', 0):.3f} | {b.get('precision', 0):.3f} | "
                     f"{len(b.get('predictions', []))} |\n")

        f.write("\n## What we did\n\n")
        f.write("### Phase 1: Fix off-by-one (47 lines in mcp.rs)\n")
        f.write("DB stores 0-based line numbers internally. MCP responses should be "
                 "1-based for human and LLM consumers. Added `+1` at 47 serialization "
                 "points across all DB-backed tools. Build clean, all tests pass.\n\n")
        f.write("**Impact:** jaccard 0.041 → 0.327 (8x improvement). Head-to-head "
                 "A wins 7, B wins 3 (was 2-7).\n\n")
        f.write("### Phase 3: LLM-native text-context tool\n")
        f.write("Added `reliary_find_references_with_source` MCP tool that wraps "
                 "type_flow and includes the actual source line text per hit. The "
                 "LLM can read the code directly without round-trips to "
                 "`get_code_snippet`.\n\n")
        f.write("**Impact:** precision 0.924 → 1.000 (perfect). Recall dropped "
                 "from 0.346 to 0.164 (LLM became over-cautious).\n\n")
        f.write("### Phase 4: Multi-signal ranked output + calibrated prompt\n")
        f.write("Increased top-K from 20 to 50 (more signal). Calibrated prompt "
                 "tells the LLM to use source text for role disambiguation only, "
                 "not style filtering.\n\n")
        f.write("**Impact:** jaccard 0.164 → 0.294, precision stays at 1.000. "
                 "Head-to-head A wins 8, B wins 1.\n\n")

        f.write("## The grammar-free math that beat altbackend\n\n")
        f.write("The winning recipe isn't a novel algorithm. It's:\n\n")
        f.write("1. **Correct line numbers** (off-by-one fix) — every hit lands "
                 "where the LLM expects\n")
        f.write("2. **Type-flow similarity ranking** — top-50 hits are mostly "
                 "valid references, not noise\n")
        f.write("3. **Source-text inline** — the LLM sees the actual line of "
                 "code, not just `(file, line, score)`\n")
        f.write("4. **Calibrated prompt** — the LLM uses source for role "
                 "disambiguation only, trusts the ranking for completeness\n\n")

        f.write("altbackend's equivalent requires:\n")
        f.write("- `search_graph` to get candidates (with qualified names)\n")
        f.write("- `get_code_snippet` per candidate to read the code (round-trip)\n")
        f.write("- LLM to filter by role manually\n\n")
        f.write("reliary8's `reliary_find_references_with_source` does all of "
                 "this in **one tool call** with **no round-trip**. This is the "
                 "grammar-free advantage.\n\n")

        f.write("## Caveats\n\n")
        f.write("- 10-task sample is small.\n")
        f.write("- Single trial per condition (no multi-trial averaging).\n")
        f.write("- Single LLM (deepseek-chat). Other LLMs may behave differently.\n")
        f.write("- Only tokio corpus. Other corpora may differ.\n")
        f.write("- Source-text prompt requires careful calibration. Default "
                 "prompt without calibration drops recall.\n")

    print(f"\nReport written to: {report_path}")


if __name__ == "__main__":
    main()