"""Aggregate multi-turn bench results into a summary report."""
import argparse
import json
import sys
from pathlib import Path
from statistics import median, mean, stdev

METRICS = ["task_score", "weighted_cost", "wall_time", "tool_calls",
            "turns", "tokens_in", "tokens_out", "tool_bytes", "dead_end_calls"]


def load_runs(path):
    runs = []
    with open(path) as f:
        for line in f:
            try:
                runs.append(json.loads(line))
            except Exception:
                pass
    return runs


def aggregate(runs):
    by_cond = {"A": [], "B": [], "C": []}
    for r in runs:
        if r["cond"] in by_cond:
            by_cond[r["cond"]].append(r)
    cond_names = {"A": "reliary", "B": "altbackend", "C": "grep"}

    out = {}
    for cond, sub in by_cond.items():
        if not sub:
            continue
        m = {"n": len(sub), "name": cond_names[cond]}
        for metric in METRICS:
            vals = [r[metric] for r in sub if metric in r and r[metric] is not None]
            if not vals:
                continue
            m[f"{metric}_median"] = median(vals)
            m[f"{metric}_mean"] = mean(vals)
            if len(vals) >= 2:
                m[f"{metric}_stdev"] = stdev(vals)
        out[cond] = m
    return out


def head_to_head(runs):
    """Per-task winner counts."""
    by_task = {}
    for r in runs:
        by_task.setdefault(r["task_id"], {})[r["cond"]] = r

    wins = {"A": 0, "B": 0, "C": 0, "tie": 0}
    per_task = []
    for tid, by_cond in by_task.items():
        if len(by_cond) < 3:
            continue
        # Average scores across seeds per cond
        scores = {}
        for c in "ABC":
            scores[c] = mean(r["task_score"] for r in runs
                              if r["task_id"] == tid and r["cond"] == c)
        max_s = max(scores.values())
        winners = [c for c, s in scores.items() if abs(s - max_s) < 0.01]
        if len(winners) > 1:
            wins["tie"] += 1
            win_str = "tie"
        else:
            wins[winners[0]] += 1
            win_str = winners[0]
        per_task.append((tid, scores["A"], scores["B"], scores["C"], win_str))
    return wins, per_task


def print_report(agg, wins, per_task, runs):
    print(f"\n{'='*70}")
    print(f"Multi-Turn Benchmark Report")
    print(f"{'='*70}\n")
    print(f"Total runs: {len(runs)}")
    print(f"Tasks: {len(set(r['task_id'] for r in runs))}")
    print(f"Conditions: {sorted(set(r['cond'] for r in runs))}")

    print(f"\n{'Condition':<12s} {'Score':<8s} {'WC':<8s} {'Wall':<8s} "
           f"{'Calls':<7s} {'Turns':<7s} {'Bytes':<8s}")
    print("-" * 70)
    for cond in ["A", "B", "C"]:
        if cond not in agg:
            continue
        m = agg[cond]
        score = m.get("task_score_median", 0)
        wc = m.get("weighted_cost_median", 0)
        wall = m.get("wall_time_median", 0)
        calls = m.get("tool_calls_median", 0)
        turns = m.get("turns_median", 0)
        bts = m.get("tool_bytes_median", 0)
        print(f"{cond} ({m['name']:<8s}) {score:<8.1f} {wc:<8.0f} {wall:<8.0f} "
               f"{calls:<7.1f} {turns:<7.1f} {bts:<8.0f}")

    print(f"\n{'='*70}")
    print(f"Head-to-head wins (per task, averaged across seeds):")
    print(f"{'='*70}\n")
    print(f"  A (reliary): {wins['A']}")
    print(f"  B (altbackend):     {wins['B']}")
    print(f"  C (grep):    {wins['C']}")
    print(f"  tie:         {wins['tie']}")

    print(f"\nPer-task scores (mean across seeds):")
    print(f"{'task':<30s} {'A_j':<6s} {'B_j':<6s} {'C_j':<6s} winner")
    for tid, sa, sb, sc, win in sorted(per_task):
        print(f"{tid:<30s} {sa:<6.2f} {sb:<6.2f} {sc:<6.2f} {win}")


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--in", dest="input", required=True)
    parser.add_argument("--out", default=None)
    args = parser.parse_args()

    runs = load_runs(args.input)
    agg = aggregate(runs)
    wins, per_task = head_to_head(runs)
    print_report(agg, wins, per_task, runs)

    if args.out:
        out_data = {
            "aggregation": agg,
            "head_to_head_wins": wins,
            "per_task_scores": [
                {"task": t, "A": a, "B": b, "C": c, "winner": w}
                for (t, a, b, c, w) in per_task
            ],
            "n_runs": len(runs),
        }
        with open(args.out, "w") as f:
            json.dump(out_data, f, indent=2)
        print(f"\nSaved: {args.out}")


if __name__ == "__main__":
    main()