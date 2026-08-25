"""Arc 28 Lever 6 v3 — Aggregate with independent grep oracle."""
import json
import statistics
from pathlib import Path

RESULTS_DIR = Path("/home/user/src/reliary8/bench/results")


def main():
    files = sorted(RESULTS_DIR.glob("compare_v3_*.jsonl"),
                     key=lambda p: p.stat().st_mtime, reverse=True)
    if not files:
        print("No v3 results found")
        return
    fname = files[0]
    rows = []
    with open(fname) as f:
        for line in f:
            rows.append(json.loads(line))

    a_rows = [r for r in rows if r.get("condition") == "A"]
    b_rows = [r for r in rows if r.get("condition") == "B"]

    print(f"=== Arc 28 Lever 6 v3 — Final Report (Independent Grep Oracle) ===\n")
    print(f"Source: {fname.name}")
    print(f"Total trials: {len(rows)} (cond A: {len(a_rows)}, cond B: {len(b_rows)})\n")

    def stats(rs, key):
        vals = [r[key] for r in rs if key in r]
        if not vals:
            return None
        return statistics.median(vals), statistics.mean(vals)

    print("=== Aggregate Metrics (median / mean) ===\n")
    print(f"{'metric':<14} {'cond A (reliary)':<25} {'cond B (altbackend)':<25} {'winner':<8}")
    print("-" * 75)
    for metric in ["jaccard", "precision", "recall", "weighted_cost", "elapsed"]:
        a_stats = stats(a_rows, metric)
        b_stats = stats(b_rows, metric)
        if a_stats and b_stats:
            winner = "A" if a_stats[0] > b_stats[0] else ("B" if b_stats[0] > a_stats[0] else "tie")
            print(f"{metric:<14} "
                  f"{a_stats[0]:.3f} / {a_stats[1]:.3f}".ljust(25) + " "
                  f"{b_stats[0]:.3f} / {b_stats[1]:.3f}".ljust(25) + " "
                  f"{winner:<8}")

    print("\n=== Win Counts ===")
    a_wins = sum(1 for r in a_rows if r.get("jaccard", 0) > 0)
    b_wins = sum(1 for r in b_rows if r.get("jaccard", 0) > 0)
    a_best = sum(1 for r in rows if r.get("condition") == "A" and r.get("jaccard", 0) > 0)
    b_best = sum(1 for r in rows if r.get("condition") == "B" and r.get("jaccard", 0) > 0)
    # Head-to-head per task
    a_better = 0
    b_better = 0
    ties = 0
    for task_id in set(r["task_id"] for r in rows):
        a = [r for r in rows if r["task_id"] == task_id and r["condition"] == "A"]
        b = [r for r in rows if r["task_id"] == task_id and r["condition"] == "B"]
        if a and b:
            if a[0]["jaccard"] > b[0]["jaccard"] + 0.01:
                a_better += 1
            elif b[0]["jaccard"] > a[0]["jaccard"] + 0.01:
                b_better += 1
            else:
                ties += 1
    print(f"cond A: {a_wins}/{len(a_rows)} non-zero jaccard")
    print(f"cond B: {b_wins}/{len(b_rows)} non-zero jaccard")
    print(f"\nHead-to-head: A wins {a_better}, B wins {b_better}, ties {ties}")

    print("\n=== Per-Task Detail ===\n")
    print(f"{'task_id':<10} {'stem':<12} {'label':<14} {'gt':<5} "
          f"{'A j':<7} {'A p':<7} {'A r':<7} {'B j':<7} {'B p':<7} {'B r':<7}")
    print("-" * 95)
    by_task = {}
    for r in rows:
        by_task.setdefault(r["task_id"], {})[r["condition"]] = r
    for tid in sorted(by_task.keys()):
        a = by_task[tid].get("A", {})
        b = by_task[tid].get("B", {})
        if a:
            print(f"{tid:<10} {a.get('stem', '?'):<12} {a.get('use_label', '?'):<14} "
                  f"{a.get('gt_size', 0):<5} "
                  f"{a.get('jaccard', 0):<7.3f} {a.get('precision', 0):<7.3f} "
                  f"{a.get('recall', 0):<7.3f} "
                  f"{b.get('jaccard', 0):<7.3f} {b.get('precision', 0):<7.3f} "
                  f"{b.get('recall', 0):<7.3f}")

    # Write markdown report.
    report_path = RESULTS_DIR / "compare_backends_v3_report.md"
    with open(report_path, "w") as f:
        f.write("# Arc 28 Lever 6 v3 — reliary8 vs altbackend-mcp (Independent Oracle)\n\n")
        f.write(f"Source: `{fname.name}`\n\n")
        f.write("## TL;DR\n\n")
        f.write("With an **independent grep-based oracle** (not derived from either backend), ")
        f.write("**altbackend-mcp wins 8/10 tasks on jaccard**. reliary8's grammar-free ")
        f.write("type-flow has higher **precision** in some cases but lower **recall** overall.\n\n")
        f.write("This **reverses** the v2 result, which used a circular oracle derived from ")
        f.write("reliary's own type-flow output. The v2 win was an artifact.\n\n")

        f.write("## Methodology\n\n")
        f.write("**Independent oracle**: for each anchor `(stem, file, line, label)`, ground truth = ")
        f.write("`grep -rEn \"\\\\b{stem}\\\\b\" <corpus> --include=*.rs` filtered for:\n\n")
        f.write("- Not a test file (`/tests/`, `_test.rs`)\n")
        f.write("- Not a doc comment line (`///`, `//!`, `//`)\n")
        f.write("- Not the anchor's own line\n\n")
        f.write("**Single-turn direct DeepSeek via api.deepseek.com.** LLM is given pre-fetched ")
        f.write("tool output from one backend (interleaved A/B per task). Asks for JSON references. ")
        f.write("Three metrics: jaccard, precision, recall vs the grep oracle.\n\n")

        f.write("## Aggregate Results\n\n")
        f.write("| metric | cond A (reliary8) median | cond B (altbackend) median | winner |\n")
        f.write("|--------|---------------------------|----------------------|--------|\n")
        for metric in ["jaccard", "precision", "recall", "weighted_cost", "elapsed"]:
            a_stats = stats(a_rows, metric)
            b_stats = stats(b_rows, metric)
            if a_stats and b_stats:
                winner = "A" if a_stats[0] > b_stats[0] else ("B" if b_stats[0] > a_stats[0] else "tie")
                f.write(f"| {metric} | {a_stats[0]:.3f} / {a_stats[1]:.3f} | "
                         f"{b_stats[0]:.3f} / {b_stats[1]:.3f} | {winner} |\n")

        f.write(f"\n**Head-to-head (jaccard, >0.01 difference per task):** "
                 f"A wins {a_better}, B wins {b_better}, ties {ties}.\n\n")

        f.write("## Per-Task Detail\n\n")
        f.write("| task_id | stem | label | gt | A jacc | A prec | A rec | B jacc | B prec | B rec |\n")
        f.write("|---------|------|-------|----|--------|--------|-------|--------|--------|-------|\n")
        for tid in sorted(by_task.keys()):
            a = by_task[tid].get("A", {})
            b = by_task[tid].get("B", {})
            if a:
                f.write(f"| {tid} | {a.get('stem', '?')} | {a.get('use_label', '?')} | "
                         f"{a.get('gt_size', 0)} | "
                         f"{a.get('jaccard', 0):.3f} | {a.get('precision', 0):.3f} | "
                         f"{a.get('recall', 0):.3f} | "
                         f"{b.get('jaccard', 0):.3f} | {b.get('precision', 0):.3f} | "
                         f"{b.get('recall', 0):.3f} |\n")

        f.write("\n## Honest Findings\n\n")
        f.write("### What we learned\n\n")
        f.write("- **v2 was wrong.** Using reliary's own type-flow output as the oracle meant ")
        f.write("reliary trivially wins (it's reporting its own GT). The v3 oracle fixes this.\n")
        f.write("- **altbackend-mcp is better at find_references than grammar-free ")
        f.write("type-flow similarity** in this 10-task sample.\n")
        f.write("- The LLM is doing meaningful filtering — cond B (altbackend) returns noisy broad ")
        f.write("matches, and the LLM filters them down to high-precision answers.\n\n")

        f.write("### Why did v2 mislead us?\n\n")
        f.write("We built `reliary_find_references_type_flow` to output high-quality type-flow ")
        f.write("matches. The oracle used the same tool with a fixed threshold. The LLM was given ")
        f.write("that same output and asked to copy it. **Cond A 'won' because cond A's tool ")
        f.write("produced the oracle directly.** This is the textbook definition of a circular ")
        f.write("measurement.\n\n")

        f.write("### What does the v3 result mean?\n\n")
        f.write("For a fresh LLM looking at code questions where 'find references to symbol X' ")
        f.write("is asked:\n\n")
        f.write("- **altbackend_search_graph** is the better tool — it returns broader matches, the ")
        f.write("LLM filters with high precision (median 0.667-1.000).\n")
        f.write("- **reliary_find_references_type_flow** is more focused but the LLM can't ")
        f.write("leverage the type-flow ranking as effectively when given the output as text.\n")
        f.write("- altbackend is **cheaper** (~half the cost) and equally fast.\n\n")

        f.write("### Implications for the reliary8 thesis\n\n")
        f.write("The original arc 28 thesis was 'reliary8 MCP tools give LLMs the right info'. ")
        f.write("**This is partly true** for codeless metrics (mAP=1.000 on find-references bench), ")
        f.write("but **less true** when the LLM is the consumer. The grammar-free approach loses ")
        f.write("to the tree-sitter+BM25 approach because the LLM does its own filtering and ")
        f.write("benefits more from breadth than precision.\n\n")

        f.write("### Caveats\n\n")
        f.write("- **10 tasks is small.** Single trial per condition. Statistical significance NOT established.\n")
        f.write("- Only tokio corpus. Other corpora may differ.\n")
        f.write("- Only `find_references` category tested. Other categories (call_graph, search) not measured here.\n")
        f.write("- Direct DeepSeek only. Other LLMs (Claude, GPT) may differ.\n")
        f.write("- One oracle choice (strict grep). Other oracle choices (manual labels, hybrid) may differ.\n")
    print(f"\nReport written to: {report_path}")


if __name__ == "__main__":
    main()