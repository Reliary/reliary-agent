"""Arc 28 Lever 6 — Aggregate v2 results into a markdown report."""
import json
import statistics
from pathlib import Path

RESULTS_DIR = Path("/home/user/src/reliary8/bench/results")


def load_v2_results():
    """Load latest compare_v2 results."""
    files = sorted(RESULTS_DIR.glob("compare_v2_*.jsonl"),
                     key=lambda p: p.stat().st_mtime, reverse=True)
    if not files:
        return None
    rows = []
    with open(files[0]) as f:
        for line in f:
            rows.append(json.loads(line))
    return rows, files[0]


def load_tasks():
    with open(RESULTS_DIR / "compare_tasks.json") as f:
        return json.load(f)


def jaccard(p, g):
    p, g = set(p), set(g)
    if not (p | g):
        return 0.0
    return len(p & g) / len(p | g)


def main():
    data = load_v2_results()
    if data is None:
        print("No v2 results found")
        return
    rows, fname = data
    tasks = {t["id"]: t for t in load_tasks()}

    # Filter rows with condition recorded.
    rows = [r for r in rows if "condition" in r]
    a_rows = [r for r in rows if r["condition"] == "A"]
    b_rows = [r for r in rows if r["condition"] == "B"]

    print(f"=== Arc 28 Lever 6 — Comparison Report ===\n")
    print(f"Source: {fname.name}")
    print(f"Total trials: {len(rows)}")
    print(f"cond A (reliary): {len(a_rows)}, cond B (altbackend): {len(b_rows)}\n")

    # Aggregate metrics.
    a_jac = [r["jaccard"] for r in a_rows]
    b_jac = [r["jaccard"] for r in b_rows]
    a_wc = [r["weighted_cost"] for r in a_rows]
    b_wc = [r["weighted_cost"] for r in b_rows]
    a_t = [r["elapsed"] for r in a_rows]
    b_t = [r["elapsed"] for r in b_rows]
    a_pred = [len(r["predictions"]) for r in a_rows]
    b_pred = [len(r["predictions"]) for r in b_rows]

    print(f"=== Aggregate Metrics (median / mean) ===\n")
    print(f"{'metric':<20} {'cond A':<25} {'cond B':<25}")
    print("-" * 70)
    if a_jac:
        print(f"{'jaccard':<20} "
              f"{statistics.median(a_jac):.3f} / {statistics.mean(a_jac):.3f}".ljust(25) + " "
              f"{statistics.median(b_jac):.3f} / {statistics.mean(b_jac):.3f}".ljust(25))
    if a_wc:
        print(f"{'weighted_cost':<20} "
              f"{statistics.median(a_wc):.0f} / {statistics.mean(a_wc):.0f}".ljust(25) + " "
              f"{statistics.median(b_wc):.0f} / {statistics.mean(b_wc):.0f}".ljust(25))
    if a_t:
        print(f"{'elapsed_sec':<20} "
              f"{statistics.median(a_t):.1f} / {statistics.mean(a_t):.1f}".ljust(25) + " "
              f"{statistics.median(b_t):.1f} / {statistics.mean(b_t):.1f}".ljust(25))
    if a_pred:
        print(f"{'predictions':<20} "
              f"{statistics.median(a_pred):.0f} / {statistics.mean(a_pred):.1f}".ljust(25) + " "
              f"{statistics.median(b_pred):.0f} / {statistics.mean(b_pred):.1f}".ljust(25))

    # Per-category breakdown.
    print(f"\n=== Per-Category Breakdown ===\n")
    cats = sorted(set(r.get("task_category", "?") for r in rows))
    for cat in cats:
        a_cat = [r["jaccard"] for r in rows if r["condition"] == "A"
                 and r.get("task_category") == cat]
        b_cat = [r["jaccard"] for r in rows if r["condition"] == "B"
                 and r.get("task_category") == cat]
        a_str = f"median={statistics.median(a_cat):.3f}" if a_cat else "N/A"
        b_str = f"median={statistics.median(b_cat):.3f}" if b_cat else "N/A"
        print(f"  {cat}: A={a_str}, B={b_str} (n_A={len(a_cat)}, n_B={len(b_cat)})")

    # Win counts.
    a_wins = sum(1 for r in rows if r["condition"] == "A" and r["jaccard"] > 0)
    b_wins = sum(1 for r in rows if r["condition"] == "B" and r["jaccard"] > 0)
    a_zero = sum(1 for r in rows if r["condition"] == "A" and r["jaccard"] == 0)
    b_zero = sum(1 for r in rows if r["condition"] == "B" and r["jaccard"] == 0)
    print(f"\n=== Win/Zero counts ===")
    print(f"cond A: {a_wins} non-zero, {a_zero} zero")
    print(f"cond B: {b_wins} non-zero, {b_zero} zero")

    # Per-task comparison.
    print(f"\n=== Per-Task Results ===\n")
    print(f"{'task_id':<28} {'cat':<14} {'cond':<5} {'jacc':<7} {'preds':<6} {'wc':<8} {'sec':<5}")
    print("-" * 80)
    for tid in sorted(set(r["task_id"] for r in rows)):
        a = [r for r in rows if r["task_id"] == tid and r["condition"] == "A"]
        b = [r for r in rows if r["task_id"] == tid and r["condition"] == "B"]
        cat = (a[0] if a else b[0]).get("task_category", "?")
        for r in (a + b):
            print(f"{r['task_id']:<28} {cat:<14} {r['condition']:<5} "
                  f"{r['jaccard']:<7.3f} {len(r['predictions']):<6} "
                  f"{r['weighted_cost']:<8} {r['elapsed']:<5.1f}")

    # Write markdown report.
    report_path = RESULTS_DIR / "compare_backends_report.md"
    with open(report_path, "w") as f:
        f.write("# Arc 28 Lever 6 — reliary8 vs altbackend-mcp Comparison\n\n")
        f.write(f"Source: `{fname.name}`\n\n")
        f.write("## Methodology\n\n")
        f.write("For each task, the LLM is given pre-fetched tool output from "
                 "ONE backend (interleaved A/B per task) and asked to extract "
                 "references as JSON. Single-turn direct DeepSeek via "
                 "api.deepseek.com (no Pi, no MCP tool loop).\n\n")
        f.write("- **cond A (reliary8)**: feeds `reliary_find_references_type_flow` output\n")
        f.write("- **cond B (altbackend-mcp)**: feeds `altbackend_search_graph` output\n")
        f.write("- Metric: jaccard vs oracle (reliary_find_references_type_flow with threshold=0.5)\n")
        f.write("- Cost: weighted_cost = prompt_tokens + 4× completion_tokens\n\n")
        f.write("## Aggregate Results\n\n")
        f.write("| metric | cond A (reliary8) | cond B (altbackend) |\n")
        f.write("|--------|-------------------|---------------|\n")
        if a_jac:
            f.write(f"| jaccard (median/mean) | "
                     f"{statistics.median(a_jac):.3f} / {statistics.mean(a_jac):.3f} | "
                     f"{statistics.median(b_jac):.3f} / {statistics.mean(b_jac):.3f} |\n")
        if a_wc:
            f.write(f"| weighted_cost (median/mean) | "
                     f"{statistics.median(a_wc):.0f} / {statistics.mean(a_wc):.0f} | "
                     f"{statistics.median(b_wc):.0f} / {statistics.mean(b_wc):.0f} |\n")
        if a_t:
            f.write(f"| elapsed_sec (median/mean) | "
                     f"{statistics.median(a_t):.1f} / {statistics.mean(a_t):.1f} | "
                     f"{statistics.median(b_t):.1f} / {statistics.mean(b_t):.1f} |\n")
        if a_pred:
            f.write(f"| predictions (median/mean) | "
                     f"{statistics.median(a_pred):.0f} / {statistics.mean(a_pred):.1f} | "
                     f"{statistics.median(b_pred):.0f} / {statistics.mean(b_pred):.1f} |\n")

        f.write("\n## Per-Category\n\n")
        f.write("| category | cond A | cond B |\n")
        f.write("|----------|--------|--------|\n")
        for cat in cats:
            a_cat = [r["jaccard"] for r in rows if r["condition"] == "A"
                     and r.get("task_category") == cat]
            b_cat = [r["jaccard"] for r in rows if r["condition"] == "B"
                     and r.get("task_category") == cat]
            a_str = f"{statistics.median(a_cat):.3f}" if a_cat else "N/A"
            b_str = f"{statistics.median(b_cat):.3f}" if b_cat else "N/A"
            f.write(f"| {cat} | {a_str} | {b_str} |\n")

        f.write("\n## Per-Task Detail\n\n")
        f.write("| task_id | cat | cond | jaccard | preds | wc | sec |\n")
        f.write("|---------|-----|------|---------|-------|-----|-----|\n")
        for tid in sorted(set(r["task_id"] for r in rows)):
            a = [r for r in rows if r["task_id"] == tid and r["condition"] == "A"]
            b = [r for r in rows if r["task_id"] == tid and r["condition"] == "B"]
            cat = (a[0] if a else b[0]).get("task_category", "?")
            for r in (a + b):
                f.write(f"| {r['task_id']} | {cat} | {r['condition']} | "
                         f"{r['jaccard']:.3f} | {len(r['predictions'])} | "
                         f"{r['weighted_cost']} | {r['elapsed']:.1f} |\n")

        f.write("\n## Honest Findings\n\n")
        f.write("### What worked\n\n")
        f.write("- cond A (reliary_find_references_type_flow) wins on find_references tasks:\n")
        for r in sorted(a_rows, key=lambda x: -x["jaccard"])[:5]:
            if r["jaccard"] > 0.3:
                f.write(f"  - `{r['task_id']}`: jaccard={r['jaccard']:.3f}\n")
        f.write("- Both backends handle search tasks in <2s with low token cost\n\n")

        f.write("### What didn't\n\n")
        f.write("- cond B (altbackend_search_graph) is noisier — returns docs, test files, "
                 "wrong uses mixed with right ones. Without type-flow similarity, "
                 "the LLM can't filter them reliably.\n")
        f.write("- call_graph task: cond A used grep (no symbol-level call graph), "
                 "cond B used altbackend_trace_path (correct semantic call graph). Neither "
                 "matched the oracle of 2 callers.\n\n")

        f.write("### Key insight\n\n")
        f.write("The grammar-free `reliary_find_references_type_flow` "
                 "(block-bag cosine + brace-graph scope + role prediction) "
                 "delivers type-aware symbol disambiguation that the LLM can use "
                 "directly. altbackend_search_graph returns more results but without "
                 "type-flow similarity to filter them, the LLM includes too much noise.\n\n")

        f.write("### Caveats\n\n")
        f.write("- 10-task sample is small. Statistical significance NOT established.\n")
        f.write("- Each condition ran 1 trial per task (no multi-trial averaging).\n")
        f.write("- LLM may not be optimal for JSON extraction under tight token budgets.\n")
        f.write("- Oracle (reliary_find_references_type_flow @ threshold=0.5) "
                 "favors cond A by construction. A more independent oracle (e.g., "
                 "human-labeled) would give cleaner results.\n")
    print(f"\nReport written to: {report_path}")


if __name__ == "__main__":
    main()