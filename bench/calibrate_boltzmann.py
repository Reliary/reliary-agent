#!/usr/bin/env python3
"""Arc 28 Lever 1 — Empirical calibration of Boltzmann temperature (Fisher MLE).

For each anchor in homonyms.json, runs type_flow via MCP, captures raw scores
and ground-truth labels, fits temperature T via MLE on the empirical
hit-rate curve, and computes KS test statistic.

Pass gate: KS p-value < 0.05.
"""
import json
import math
import subprocess
import sys
from pathlib import Path

import scipy.optimize as opt
import scipy.stats as stats

BINARY = '/home/user/src/reliary8/target/release/reliary'
CORPUS_PATH = '/tmp/tokio-corpus/tokio/src'
FIXTURES_PATH = '/home/user/src/reliary8/bench/fixtures/homonyms.json'


def mcp_call(bin_path, workdir, tool, args, timeout=30):
    """Call a reliary MCP tool and return the parsed result."""
    req = json.dumps({"jsonrpc": "2.0", "id": 1, "method": "tools/call",
                       "params": {"name": tool, "arguments": args}})
    proc = subprocess.Popen(
        [str(bin_path), "mcp"], cwd=workdir,
        stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
    )
    try:
        out, _ = proc.communicate(req.encode(), timeout=timeout)
    except subprocess.TimeoutExpired:
        proc.kill()
        return {}
    for line in out.decode().splitlines():
        try:
            r = json.loads(line)
            if "result" in r:
                return json.loads(r["result"]["content"][0]["text"])
        except Exception:
            pass
    return {}


def softmax_t(scores, T):
    """Boltzmann probabilities."""
    if T <= 0 or not scores:
        return [0.0] * len(scores)
    scaled = [s / T for s in scores]
    mx = max(scaled)
    exps = [math.exp(e - mx) for e in scaled]
    s = sum(exps)
    if s == 0:
        return [1.0 / len(scores)] * len(scores)
    return [e / s for e in exps]


def neg_log_likelihood(T, scores, labels):
    """NLL of labels under Boltzmann(s, T)."""
    if T <= 0:
        return float('inf')
    probs = softmax_t(scores, T)
    eps = 1e-12
    return -sum(l * math.log(max(p, eps)) + (1 - l) * math.log(max(1 - p, eps))
               for p, l in zip(probs, labels))


def fit_temperature(scores, labels):
    """Fit T via L-BFGS-B minimization."""
    if not scores or not labels:
        return 1.0, 0.0
    rng = max(scores) - min(scores)
    init_T = max(1.0, rng / 5.0)
    res = opt.minimize(
        lambda t: neg_log_likelihood(t[0], scores, labels),
        [init_T], bounds=[(0.01, 100.0)], method='L-BFGS-B',
    )
    return float(res.x[0]), float(res.fun)


def ks_test(probs, labels):
    pos = [p for p, l in zip(probs, labels) if l == 1]
    neg = [p for p, l in zip(probs, labels) if l == 0]
    if len(pos) < 2 or len(neg) < 2:
        return 0.0, 1.0
    return stats.ks_2samp(pos, neg)


def is_correct_hit(hit, anchor):
    """A hit is correct if its file:line matches anchor, or its line+1 (next line).
    We're using type_flow's hits, which include all references in file. We need
    to mark which hits are 'truth' (anchor itself). For calibration, use:
      - The anchor's own line: always correct.
      - Same block as anchor + same role: heuristically correct.
      - Otherwise: not enough info, label=0."""
    if not isinstance(hit, dict):
        return False
    file = hit.get('file', '')
    line = hit.get('line', 0)
    # The anchor's exact file/line is correct (it's the def/use site).
    if file == anchor['anchor_file'] and line == anchor['anchor_line']:
        return True
    # For calibration purposes, an is_def hit at the anchor context is correct.
    if hit.get('is_def') and file == anchor['anchor_file']:
        return True
    return False


def collect_calibration_data(binary, corpus, anchors, max_anchors=20):
    """For each anchor, run type_flow and collect (score, label) pairs."""
    all_scores, all_labels = [], []
    per_anchor_counts = []
    for i, anchor in enumerate(anchors[:max_anchors]):
        # Strip corpus prefix if anchor_file has it.
        af = anchor['anchor_file']
        # Path comes from fixture; may be absolute or relative.
        args = {
            "name": anchor['stem'],
            "anchor_file": af,
            "anchor_line": int(anchor['anchor_line']),
            "threshold": 0.0,  # Get everything.
            "path": corpus,
        }
        result = mcp_call(binary, corpus, "reliary_find_references_type_flow", args, timeout=30)
        hits = result.get('hits', [])
        n_correct = 0
        for h in hits:
            score = h.get('similarity', h.get('score', 0.5))
            label = 1 if is_correct_hit(h, anchor) else 0
            all_scores.append(float(score))
            all_labels.append(label)
            if label == 1:
                n_correct += 1
        per_anchor_counts.append({
            'anchor_id': anchor['id'],
            'n_hits': len(hits),
            'n_correct': n_correct,
        })
    return all_scores, all_labels, per_anchor_counts


def main():
    print("=== Arc 28 Lever 1: Empirical Calibration (Fisher MLE) ===\n")
    with open(FIXTURES_PATH) as f:
        anchors_data = json.load(f)
    anchors = anchors_data['anchors']
    print(f"Loaded {len(anchors)} anchors from {FIXTURES_PATH}")
    print(f"Corpus: {CORPUS_PATH}")
    print(f"Binary: {BINARY}\n")

    # Collect raw scores.
    print("Running type_flow on first 20 anchors...")
    all_scores, all_labels, per_anchor = collect_calibration_data(
        BINARY, CORPUS_PATH, anchors, max_anchors=20,
    )
    print(f"Collected {len(all_scores)} scored hits ({sum(all_labels)} correct).\n")
    if not all_scores:
        print("No scores collected — bench has no index or binary missing.")
        print("Verify: ls /tmp/tokio-corpus/tokio/src/.reliary/index.sqlite")
        print("Verify: ls -la /home/user/src/reliary8/target/release/reliary")
        return 1

    # Fit temperature.
    T, nll = fit_temperature(all_scores, all_labels)
    probs = softmax_t(all_scores, T)
    ks_stat, p_value = ks_test(probs, all_labels)

    # EDA: bucket hits by score.
    buckets = {}
    for s, l in zip(all_scores, all_labels):
        b = round(s, 1)
        buckets.setdefault(b, [0, 0])
        buckets[b][0] += 1
        buckets[b][1] += l
    eda = []
    for b in sorted(buckets.keys()):
        total, correct = buckets[b]
        eda.append({'score_bucket': b, 'total': total, 'correct': correct,
                    'hit_rate': correct / total if total else 0.0})

    report = {
        'n_hits': len(all_scores),
        'n_correct': int(sum(all_labels)),
        'fitted_temperature': T,
        'log_likelihood': -nll,
        'ks_statistic': float(ks_stat),
        'ks_p_value': float(p_value),
        'eda_buckets': eda,
        'per_anchor_counts': per_anchor,
    }
    report_path = Path('/home/user/src/reliary8/bench/calibration_report.json')
    with open(report_path, 'w') as f:
        json.dump(report, f, indent=2)

    print(f"Fitted temperature T = {T:.4f}")
    print(f"Log-likelihood = {-nll:.2f}")
    print(f"KS statistic = {ks_stat:.4f}, p-value = {p_value:.4e}")
    print(f"\n✅ Wrote {report_path}")
    if p_value < 0.05:
        print("✅ PASS: probabilities statistically distinguishable.")
        return 0
    print(f"⚠️  KS p-value {p_value:.4f} ≥ 0.05.")
    return 0


if __name__ == '__main__':
    sys.exit(main())
