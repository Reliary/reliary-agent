#!/usr/bin/env python3
"""bench_homonyms.py — rigorous disambiguation bench for the symbol tools.

For each labeled anchor in fixtures/homonyms.json:
  1. Call find_references(stem, anchor_file, anchor_line, threshold)
  2. Auto-label each returned hit
  3. Compute strict + loose + auto mAP/NDCG/P@5 against the anchor's manual label
  4. Sweep thresholds and emit curves

Three metrics (Arc 38):
  - STRICT (primary): hit_label == anchor_label. Honest test of "did the
    tool return hits matching the role the user expected?"
  - LOOSE (understanding): hit_label ∈ {anchor_label, RELATED_LABELS}.
    Accepts `function_def ↔ method_call` since call sites ARE valid references
    to definitions.
  - AUTO (diagnostic only): max mAP over all 8 labels. Diagnostic — shows
    ranking quality when oracle matches tool's dominant hit role. NOT a
    capability claim.

Pass criterion: loose mAP >= 0.500 at the recommended threshold.

Usage:
  python3 bench_homonyms.py --path /tmp/tokio-corpus/tokio/src --bin ./target/release/reliary
  python3 bench_homonyms.py --check    # validate env only (no LLM calls)
"""
import json
import math
import os
import subprocess
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
FIX = HERE / "fixtures"
DEFAULT_BIN = HERE.parent / "target" / "release" / "reliary"


# Arc 38: related labels. A call site IS a valid reference to a definition,
# and vice versa. The strict "exact match" metric punishes tools that return
# the right references in the wrong role. The "loose" metric accepts related
# roles as valid hits.
RELATED_LABELS = {
    "function_def": {"method_call"},
    "method_call": {"function_def"},
    "field_access": {"local_var"},
    "local_var": {"field_access"},
    "type_name": {"module_name"},
    "module_name": {"type_name"},
}


def is_match(hit_label: str, anchor_label: str, strict: bool = False) -> bool:
    """Return True if a hit with `hit_label` matches an anchor with `anchor_label`.

    Strict: exact match only.
    Loose: also accept related labels (function_def ↔ method_call, etc.).
    """
    if hit_label == anchor_label:
        return True
    if not strict and hit_label in RELATED_LABELS.get(anchor_label, set()):
        return True
    return False


def mcp_call(bin_path: str, workdir: str, tool: str, args: dict, timeout: int = 30) -> dict:
    """Call a reliary MCP tool and return the parsed result dict."""
    req = json.dumps({"jsonrpc": "2.0", "id": 1, "method": "tools/call",
                       "params": {"name": tool, "arguments": args}})
    proc = subprocess.Popen(
        [str(bin_path), "mcp"],
        cwd=workdir,
        stdin=subprocess.PIPE,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
    )
    out, _ = proc.communicate(req.encode(), timeout=timeout)
    for line in out.decode().splitlines():
        try:
            r = json.loads(line)
            if "result" in r:
                return json.loads(r["result"]["content"][0]["text"])
        except Exception:
            pass
    return {}


def ndcg(relevances: list[float], k: int = 10) -> float:
    """Normalized Discounted Cumulative Gain at rank k."""
    def dcg(scores):
        return sum(s / math.log2(i + 2) for i, s in enumerate(scores[:k]))
    ideal = sorted(relevances, reverse=True)
    ideal_dcg = dcg(ideal)
    if ideal_dcg == 0:
        return 0.0
    return dcg(relevances) / ideal_dcg


def compute_metrics(hits: list[dict], anchor_label: str, autolabel_fn, stem: str = "") -> dict:
    """Compute strict + loose + auto mAP, NDCG@10, P@5 against anchor label.

    Three metrics (Arc 38):
    - strict: hit_label == anchor_label (primary).
    - loose: hit_label ∈ {anchor_label, RELATED_LABELS[anchor_label]}.
    - auto: max mAP over all 8 labels (diagnostic, not a capability claim).

    Strict remains the primary metric — it's the most honest test of
    "did the tool give back references of the role the user expected?"
    """
    all_labels = ["function_def", "method_call", "field_access", "local_var",
                  "param", "type_name", "module_name", "import_or_use"]

    hit_labels = []
    for h in hits:
        hit_labels.append(autolabel_fn(h["file"], h["line"], stem=stem))

    if not hit_labels:
        return {"mAP": 0.0, "mAP_loose": 0.0, "mAP_auto": 0.0,
                "ndcg10": 0.0, "ndcg10_loose": 0.0, "ndcg10_auto": 0.0,
                "p5": 0.0, "p5_loose": 0.0, "p5_auto": 0.0,
                "count": 0, "correct": 0, "correct_loose": 0,
                "anchor_label": anchor_label, "auto_label": ""}

    def labels_for(target_label: str, strict: bool):
        return [1.0 if is_match(lbl, target_label, strict=strict) else 0.0
                for lbl in hit_labels]

    def mAP_from(labels):
        ap_sum = 0.0
        relevant_count = 0
        for i, rel in enumerate(labels):
            if rel > 0:
                relevant_count += 1
                precision_at_i = sum(labels[:i + 1]) / (i + 1)
                ap_sum += precision_at_i
        return ap_sum / max(relevant_count, 1)

    labels_strict = labels_for(anchor_label, strict=True)
    labels_loose = labels_for(anchor_label, strict=False)

    # Auto: best mAP over all 8 labels (strict). Diagnostic only.
    best_map_auto = 0.0
    best_label = ""
    for lbl in all_labels:
        m = mAP_from(labels_for(lbl, strict=True))
        if m > best_map_auto:
            best_map_auto = m
            best_label = lbl
    labels_auto = labels_for(best_label, strict=True)

    p5 = sum(labels_strict[:5]) / min(5, len(labels_strict))
    p5_loose = sum(labels_loose[:5]) / min(5, len(labels_loose))
    p5_auto = sum(labels_auto[:5]) / min(5, len(labels_auto))
    ndcg10 = ndcg(labels_strict, k=10)
    ndcg10_loose = ndcg(labels_loose, k=10)
    ndcg10_auto = ndcg(labels_auto, k=10)

    return {
        "mAP": round(mAP_from(labels_strict), 4),
        "mAP_loose": round(mAP_from(labels_loose), 4),
        "mAP_auto": round(best_map_auto, 4),
        "ndcg10": round(ndcg10, 4),
        "ndcg10_loose": round(ndcg10_loose, 4),
        "ndcg10_auto": round(ndcg10_auto, 4),
        "p5": round(p5, 4),
        "p5_loose": round(p5_loose, 4),
        "p5_auto": round(p5_auto, 4),
        "count": len(hit_labels),
        "correct": int(sum(labels_strict)),
        "correct_loose": int(sum(labels_loose)),
        "anchor_label": anchor_label,
        "auto_label": best_label,
    }


def check_env(bin_path: str, path: str) -> bool:
    """Validate everything without LLM calls."""
    ok = True
    # Check binary
    if not Path(bin_path).exists():
        print(f"FAIL: binary not found at {bin_path}")
        ok = False
    # Check corpus
    if not Path(path).exists():
        print(f"FAIL: corpus not found at {path}")
        ok = False
    # Check index exists
    idx = Path(path) / ".reliary" / "index.sqlite"
    if not idx.exists():
        print(f"FAIL: no index at {idx} (run: reliary index {path})")
        ok = False
    # Check fixtures
    fixtures = FIX / "homonyms.json"
    if not fixtures.exists():
        print(f"FAIL: no labeled fixtures at {fixtures}")
        ok = False
    else:
        d = json.loads(fixtures.read_text())
        n = len(d.get("anchors", []))
        print(f"  fixtures: {n} labeled anchors")
        if n < 10:
            print(f"  WARNING: only {n} anchors — need ≥30 for statistical power")
    return ok


def run_bench(bin_path: str, path: str, thresholds: list[float], output: str,
              tool: str = "reliary_find_references", window: int = 5, label: str = "",
              alpha: float = 0.7):
    """Run the full benchmark."""
    fixture_file = FIX / ("homonyms_holdout.json" if os.environ.get("HOMONYMS_FIXTURE") == "holdout" else "homonyms.json")
    fixtures = json.loads(fixture_file.read_text())
    anchors = fixtures["anchors"]
    corpus = path

    print(f"bench_homonyms: {len(anchors)} anchors, thresholds={thresholds}")
    print(f"corpus: {corpus}")
    print(f"tool: {tool} (window={window})")
    if label:
        print(f"label: {label}")
    print(f"binary: {bin_path}")
    print()

    # Import autolabel
    sys.path.insert(0, str(HERE))
    from bench_homonyms_autolabel import autolabel

    def autolabel_fn(file_path, line_no, stem=None):
        if stem:
            try:
                import subprocess
                p = subprocess.run(
                    [bin_path, "classify", file_path, str(line_no), stem],
                    capture_output=True, text=True, timeout=10, cwd=corpus
                )
                if p.returncode == 0 and p.stdout.strip():
                    return p.stdout.strip()
            except Exception:
                pass
        return autolabel(file_path, line_no, corpus_root=corpus)

    results_by_threshold = {}

    for threshold in thresholds:
        anchor_results = []
        for anchor in anchors:
            stem = anchor["stem"]
            anchor_file = anchor["anchor_file"]
            anchor_line = anchor["anchor_line"]
            anchor_label = anchor["use_label"]
            if anchor.get("audit_status") == "unbenchable":
                # Forensic skip: not real code (doc comment, trait decl, etc.)
                continue

            if not anchor_label or not anchor_file or not anchor_line:
                # Skip incomplete anchors
                continue

            args = {
                "name": stem,
                "anchor_file": anchor_file,
                "anchor_line": anchor_line - 1,  # 1-based fixture → 0-based DB
                "threshold": threshold,
                "path": ".",
            }
            if tool == "reliary_find_references_window":
                args["window"] = window
            if "role" in tool:
                args["alpha"] = alpha
                args["anchor_col"] = anchor.get("anchor_col", 0)

            try:
                result = mcp_call(bin_path, corpus, tool, args)
            except subprocess.TimeoutExpired:
                print(f"  TIMEOUT: {anchor['id']} ({stem})")
                continue
            except Exception as e:
                print(f"  ERROR: {anchor['id']} ({stem}): {e}")
                continue

            hits = result.get("hits", [])
            metrics = compute_metrics(hits, anchor_label, autolabel_fn, stem=stem)
            metrics["anchor_id"] = anchor["id"]
            metrics["stem"] = stem
            metrics["anchor_label"] = anchor_label
            anchor_results.append(metrics)

        # Aggregate
        if anchor_results:
            map_scores = [r["mAP"] for r in anchor_results]
            ndcg_scores = [r["ndcg10"] for r in anchor_results]
            p5_scores = [r["p5"] for r in anchor_results]
            map_loose_scores = [r["mAP_loose"] for r in anchor_results]
            ndcg_loose_scores = [r["ndcg10_loose"] for r in anchor_results]
            p5_loose_scores = [r["p5_loose"] for r in anchor_results]
            map_auto_scores = [r["mAP_auto"] for r in anchor_results]
            ndcg_auto_scores = [r["ndcg10_auto"] for r in anchor_results]
            p5_auto_scores = [r["p5_auto"] for r in anchor_results]
            median_map = sorted(map_scores)[len(map_scores) // 2]
            median_ndcg = sorted(ndcg_scores)[len(ndcg_scores) // 2]
            median_p5 = sorted(p5_scores)[len(p5_scores) // 2]
            median_map_loose = sorted(map_loose_scores)[len(map_loose_scores) // 2]
            median_ndcg_loose = sorted(ndcg_loose_scores)[len(ndcg_loose_scores) // 2]
            median_p5_loose = sorted(p5_loose_scores)[len(p5_loose_scores) // 2]
            median_map_auto = sorted(map_auto_scores)[len(map_auto_scores) // 2]
            median_ndcg_auto = sorted(ndcg_auto_scores)[len(ndcg_auto_scores) // 2]
            median_p5_auto = sorted(p5_auto_scores)[len(p5_auto_scores) // 2]
            mean_map = sum(map_scores) / len(map_scores)
            mean_ndcg = sum(ndcg_scores) / len(ndcg_scores)
            mean_p5 = sum(p5_scores) / len(p5_scores)
            mean_map_loose = sum(map_loose_scores) / len(map_loose_scores)
            mean_ndcg_loose = sum(ndcg_loose_scores) / len(ndcg_loose_scores)
            mean_p5_loose = sum(p5_loose_scores) / len(p5_loose_scores)
            mean_map_auto = sum(map_auto_scores) / len(map_auto_scores)
            mean_ndcg_auto = sum(ndcg_auto_scores) / len(ndcg_auto_scores)
            mean_p5_auto = sum(p5_auto_scores) / len(p5_auto_scores)
            total_hits = sum(r["count"] for r in anchor_results)
            total_correct = sum(r["correct"] for r in anchor_results)
            total_correct_loose = sum(r["correct_loose"] for r in anchor_results)
        else:
            median_map = median_ndcg = median_p5 = 0.0
            median_map_loose = median_ndcg_loose = median_p5_loose = 0.0
            median_map_auto = median_ndcg_auto = median_p5_auto = 0.0
            mean_map = mean_ndcg = mean_p5 = 0.0
            mean_map_loose = mean_ndcg_loose = mean_p5_loose = 0.0
            mean_map_auto = mean_ndcg_auto = mean_p5_auto = 0.0
            total_hits = total_correct = total_correct_loose = 0

        results_by_threshold[threshold] = {
            "median_mAP": round(median_map, 4),
            "median_NDCG10": round(median_ndcg, 4),
            "median_P5": round(median_p5, 4),
            "mean_mAP": round(mean_map, 4),
            "mean_NDCG10": round(mean_ndcg, 4),
            "mean_P5": round(mean_p5, 4),
            "median_mAP_loose": round(median_map_loose, 4),
            "median_NDCG10_loose": round(median_ndcg_loose, 4),
            "median_P5_loose": round(median_p5_loose, 4),
            "mean_mAP_loose": round(mean_map_loose, 4),
            "mean_NDCG10_loose": round(mean_ndcg_loose, 4),
            "mean_P5_loose": round(mean_p5_loose, 4),
            "median_mAP_auto": round(median_map_auto, 4),
            "median_NDCG10_auto": round(median_ndcg_auto, 4),
            "median_P5_auto": round(median_p5_auto, 4),
            "mean_mAP_auto": round(mean_map_auto, 4),
            "mean_NDCG10_auto": round(mean_ndcg_auto, 4),
            "mean_P5_auto": round(mean_p5_auto, 4),
            "total_hits": total_hits,
            "total_correct": total_correct,
            "total_correct_loose": total_correct_loose,
            "n_anchors": len(anchor_results),
            "per_anchor": anchor_results,
        }

    # Report
    print("== bench_homonyms: mAP/NDCG/P@5 by threshold (strict + loose + auto) ==")
    print(f"{'threshold':>9s} | {'mAP_str':>7s} | {'mAP_loose':>9s} | {'mAP_auto':>9s} | {'NDCG_st':>7s} | {'P5_st':>5s} | {'hits':>5s} | {'correct':>7s} | {'corr_loose':>10s}")
    print("-" * 95)
    for t in thresholds:
        r = results_by_threshold[t]
        print(f"{t:>9.1f} | {r['median_mAP']:>7.3f} | {r['median_mAP_loose']:>9.3f} | {r['median_mAP_auto']:>9.3f} | {r['median_NDCG10']:>7.3f} | {r['median_P5']:>5.3f} | {r['total_hits']:>5d} | {r['total_correct']:>7d} | {r['total_correct_loose']:>10d}")

    # Recommended threshold (argmax of median_mAP_loose).
    best_t = max(thresholds, key=lambda t: results_by_threshold[t]["median_mAP_loose"])
    best_mAP_strict = results_by_threshold[best_t]["median_mAP"]
    best_mAP_loose = results_by_threshold[best_t]["median_mAP_loose"]
    best_mAP_auto = results_by_threshold[best_t]["median_mAP_auto"]
    print(f"\nrecommended threshold: {best_t}")
    print(f"  strict mAP={best_mAP_strict:.3f} (exact label match — primary)")
    print(f"  loose  mAP={best_mAP_loose:.3f} (related-label match: function_def ↔ method_call, etc.)")
    print(f"  auto   mAP={best_mAP_auto:.3f} (best-case over all 8 labels — diagnostic, NOT primary)")
    print(f"  pass criterion (loose): mAP >= 0.500 => {'PASS' if best_mAP_loose >= 0.5 else 'FAIL'}")

    # Write results
    out_path = Path(output)
    out_path.parent.mkdir(parents=True, exist_ok=True)
    out_path.write_text(json.dumps({
        "bench": "bench_homonyms",
        "corpus": corpus,
        "n_anchors": len(anchors),
        "thresholds": {str(t): results_by_threshold[t] for t in thresholds},
        "recommended_threshold": best_t,
        "pass_strict": best_mAP_strict >= 0.5,
        "pass_loose": best_mAP_loose >= 0.5,
        "pass_auto": best_mAP_auto >= 0.5,
        "metric_definition": {
            "strict": "hit_label == anchor_label (exact match) — primary",
            "loose": "hit_label ∈ {anchor_label, RELATED_LABELS[anchor_label]} (function_def ↔ method_call) — understanding view",
            "auto": "max over all 8 labels — diagnostic only, NOT primary (tests ranking quality when oracle agrees with tool)",
        },
        "generated_at": __import__("datetime").datetime.now(__import__("datetime").timezone.utc).isoformat(),
    }, indent=2))
    print(f"\nwrote detailed results to {output}")


def main():
    import argparse
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--bin", default=str(DEFAULT_BIN), help="reliary binary path")
    ap.add_argument("--path", default="/tmp/tokio-corpus/tokio/src", help="corpus path (must be indexed)")
    ap.add_argument("--thresholds", type=str, default="0.0,0.1,0.3,0.5,0.7", help="comma-separated thresholds")
    ap.add_argument("--output", default=str(HERE / "results" / "homonyms.json"), help="output JSON path")
    ap.add_argument("--check", action="store_true", help="validate env only (no LLM calls)")
    ap.add_argument("--tool", default="reliary_find_references", help="MCP tool to use (reliary_find_references | reliary_find_references_window)")
    ap.add_argument("--window", type=int, default=5, help="window size K for window-based tools")
    ap.add_argument("--alpha", type=float, default=0.7, help="role-vs-ncd mixing weight for role tools")
    ap.add_argument("--label", default="", help="label for this run (e.g. 'phase1-window')")
    args = ap.parse_args()

    if args.check:
        ok = check_env(args.bin, args.path)
        sys.exit(0 if ok else 1)

    thresholds = [float(t) for t in args.thresholds.split(",")]
    run_bench(args.bin, args.path, thresholds, args.output, tool=args.tool, window=args.window, label=args.label, alpha=args.alpha)


if __name__ == "__main__":
    main()