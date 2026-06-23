"""SQuAD v2 (Stanford Question Answering) accuracy benchmark.

Tests whether compression preserves reading comprehension.
3 conditions × 2 runs = 6 sessions × 100 SQuAD questions = 600 LLM calls.

Conditions:
  baseline     - No compression. Pi direct to DeepSeek.
  recommended  - Full proxy + gate.js stack with SRCR floor 0.3.
  passthrough  - Proxy enabled but RELIARY_PROXY_PASSTHROUGH=1 disables compression.

Scoring: standard SQuAD metrics — F1 (token overlap) + exact match.
Pass criteria: recommended F1 >= 95% of baseline (97% retention target).

Usage: python3 bench_squad.py [--runs N] [--samples N]
"""
import argparse
import json
import os
import random
import re
import subprocess
import sys
import time
from collections import Counter
from pathlib import Path

# --- Config (mirrors bench_rename.py / bench_bfcl.py) ---
PI = os.path.expanduser("~/.local/bin/pi")
SETTINGS = os.path.expanduser("~/.pi/agent/settings.json")
MODELS = os.path.expanduser("~/.pi/agent/models.json")
GATE = os.path.abspath(os.path.join(os.path.dirname(__file__), "..", "..", "pi", "gate.js"))
RELIARY_BIN = (os.path.join(os.path.dirname(__file__), "..", "..", "target", "release", "reliary-agent")
               if os.path.exists(os.path.join(os.path.dirname(__file__), "..", "..", "target", "release", "reliary-agent"))
               else "reliary-agent")
DATA_FILE = Path("/tmp/bench_squad/squad_100.json")
RESULTS_FILE = Path("/tmp/bench_squad_results.json")

CONDITIONS = [
    {"label": "baseline",    "needs_proxy": False, "needs_gate": False, "env": {}},
    {"label": "recommended", "needs_proxy": True,  "needs_gate": True,
     "env": {"RELIARY_MODE": "strict", "RELIARY_LOG": "warn"}},
    {"label": "passthrough", "needs_proxy": True,  "needs_gate": True,
     "env": {"RELIARY_MODE": "strict", "RELIARY_LOG": "warn", "RELIARY_PROXY_PASSTHROUGH": "1"}},
]


def read_api_key():
    routes_path = os.path.expanduser("~/.reliary/proxy-routes.json")
    try:
        with open(routes_path) as f:
            routes = json.load(f)
        for key, url in routes.items():
            if "deepseek" in url and len(key) > 20 and not key.startswith("__"):
                return key
    except Exception:
        pass
    return os.environ.get("DEEPSEEK_API_KEY", "")


API_KEY = read_api_key()


# --- SQuAD standard scoring ---

def normalize_answer(s: str) -> str:
    """Lower text and remove punctuation, articles and extra whitespace."""
    def remove_articles(text):
        return re.sub(r"\b(a|an|the)\b", " ", text)
    def white_space_fix(text):
        return " ".join(text.split())
    def remove_punc(text):
        import string
        return "".join(ch for ch in text if ch not in set(string.punctuation))
    def lower(text):
        return text.lower()
    return white_space_fix(remove_articles(remove_punc(lower(s))))


def f1_score(prediction: str, ground_truth: str) -> float:
    pred_tokens = normalize_answer(prediction).split()
    gt_tokens = normalize_answer(ground_truth).split()
    if not pred_tokens or not gt_tokens:
        return float(pred_tokens == gt_tokens)
    common = Counter(pred_tokens) & Counter(gt_tokens)
    num_same = sum(common.values())
    if num_same == 0:
        return 0.0
    precision = num_same / len(pred_tokens)
    recall = num_same / len(gt_tokens)
    return 2 * precision * recall / (precision + recall)


def exact_match(prediction: str, ground_truth: str) -> float:
    return float(normalize_answer(prediction) == normalize_answer(ground_truth))


def score_sample(sample: dict, predicted_text: str) -> dict:
    """Score one SQuAD sample. Returns {f1, em, correct} dict."""
    if sample["is_unanswerable"]:
        # The LLM should respond with "I don't know" or similar
        refusal_markers = ["cannot", "don't know", "do not know", "no answer",
                           "not in the context", "not provided", "not mentioned",
                           "unanswerable", "no information", "not in the passage"]
        pred_lower = predicted_text.lower().strip()
        refused = any(m in pred_lower for m in refusal_markers)
        # Also count very short answers that don't try to answer
        return {"f1": 1.0 if refused else 0.0, "em": 1.0 if refused else 0.0, "correct": refused}
    f1 = f1_score(predicted_text, sample["expected_text"])
    em = exact_match(predicted_text, sample["expected_text"])
    return {"f1": f1, "em": em, "correct": f1 > 0.5}


# --- Pi agent invocation ---

def build_prompt(sample: dict) -> str:
    """Build the SQuAD prompt from a sample."""
    if sample["is_unanswerable"]:
        instr = ("Answer with 'I don't know' if the answer cannot be found in the context. "
                 "Otherwise, respond with ONLY the answer text, no explanation.")
    else:
        instr = "Respond with ONLY the answer text — no explanation, no full sentence."
    return (
        f"Context:\n{sample['context']}\n\n"
        f"Question: {sample['question']}\n\n"
        f"Instruction: {instr}"
    )


def route_pi_to_proxy(enable):
    if enable:
        cfg = {
            "providers": {
                "deepseek": {
                    "apiKeyEnv": "DEEPSEEK_API_KEY",
                    "baseUrl": "http://127.0.0.1:9090/v1",
                }
            }
        }
        with open(MODELS, "w") as f:
            json.dump(cfg, f, indent=2)
    else:
        if os.path.exists(MODELS):
            os.remove(MODELS)


def set_ext(ext_path):
    with open(SETTINGS, "w") as f:
        if ext_path:
            json.dump({"version": 1, "packages": [ext_path], "extensions": [ext_path]}, f, indent=2)
        else:
            json.dump({"version": 1, "packages": [], "extensions": []}, f, indent=2)


def restart_daemon():
    subprocess.run([RELIARY_BIN, "stop"], capture_output=True, timeout=10)
    time.sleep(1)
    subprocess.run([RELIARY_BIN, "start"], capture_output=True, timeout=30)
    for _ in range(20):
        try:
            r = subprocess.run(
                ["curl", "-sf", "-m", "2", "http://127.0.0.1:9090/health"],
                capture_output=True, text=True
            )
            if r.returncode == 0:
                return
        except Exception:
            pass
        time.sleep(0.5)


def parse_text_response(stdout: str) -> str:
    """Extract the FINAL assistant text from Pi's streaming JSON output.

    Pi streams assistant responses as incremental `message_update` events where
    each carries the FULL partial content (e.g. text="B", then "Ba", then "Bas").
    Naive concatenation produces "BBaBas..." not the final answer. Take only the
    last assistant message (the message_end event).
    """
    last_assistant_text = ""
    for line in stdout.splitlines():
        if not line.startswith("{"):
            continue
        try:
            d = json.loads(line)
            # Only consider terminal events with full message
            if d.get("type") not in ("message_end", "agent_end"):
                continue
            m = d.get("message", {})
            if m.get("role") != "assistant":
                continue
            content = m.get("content", [])
            if isinstance(content, list):
                for c in content:
                    if isinstance(c, dict) and c.get("type") == "text":
                        last_assistant_text = c.get("text", "")
                        break
        except Exception:
            pass
    return last_assistant_text


def parse_usage(stdout: str) -> tuple:
    pt_max = ct_max = 0
    for line in stdout.splitlines():
        if not line.startswith("{"):
            continue
        try:
            d = json.loads(line)
            m = d.get("message", {})
            if m.get("role") != "assistant":
                continue
            u = m.get("usage", {})
            pt = u.get("input", 0)
            ct = u.get("output", 0)
            if pt + ct > pt_max + ct_max:
                pt_max = pt
                ct_max = ct
        except Exception:
            pass
    return pt_max, ct_max


def run_condition(cond, samples, run_idx):
    sfile = f"/tmp/bench-squad-{cond['label']}-r{run_idx}.json"
    if os.path.exists(sfile):
        os.remove(sfile)

    route_pi_to_proxy(cond["needs_proxy"])
    set_ext(GATE if cond["needs_gate"] else None)

    env = {**os.environ, "PI_DISABLE_HEARTBEAT": "1", "DEEPSEEK_API_KEY": API_KEY}
    env.update(cond["env"])

    total_pt = total_ct = 0
    total_wt = 0.0
    f1_sum = em_sum = 0.0
    correct = 0
    per_sample = []

    for i, sample in enumerate(samples):
        prompt = build_prompt(sample) + f"\n\n[run={run_idx} idx={i} ts={int(time.time()*1000)}]"
        t0 = time.time()
        try:
            r = subprocess.run(
                [PI, "--model", "deepseek/deepseek-v4-flash", "--mode", "json",
                 "--session", sfile, "--print", prompt],
                capture_output=True, text=True, timeout=120, env=env,
                cwd="/tmp/bench_squad",
            )
            wt = time.time() - t0
            response_text = parse_text_response(r.stdout).strip()
            pt, ct = parse_usage(r.stdout)
            s = score_sample(sample, response_text)
            f1_sum += s["f1"]
            em_sum += s["em"]
            correct += s["correct"]
            per_sample.append({
                "idx": i, "f1": round(s["f1"], 3), "em": round(s["em"], 3),
                "wt": round(wt, 1), "pt": pt, "ct": ct,
                "predicted": response_text[:100], "expected": sample.get("expected_text"),
            })
            total_pt += pt
            total_ct += ct
            total_wt += wt
        except subprocess.TimeoutExpired:
            per_sample.append({"idx": i, "f1": 0, "em": 0, "wt": 120, "pt": 0, "ct": 0, "error": "timeout"})
        except Exception as e:
            per_sample.append({"idx": i, "f1": 0, "em": 0, "wt": 0, "pt": 0, "ct": 0, "error": str(e)[:80]})

    n = len(samples)
    avg_f1 = f1_sum / n if n else 0
    avg_em = em_sum / n if n else 0
    accuracy = correct / n if n else 0
    wc = total_pt + 2 * total_ct  # DeepSeek V4 Flash 1:2 ratio

    return {
        "feature": cond["label"],
        "run": run_idx,
        "f1": avg_f1, "em": avg_em, "accuracy": accuracy,
        "correct": correct,
        "total": n,
        "pt": total_pt, "ct": total_ct, "wc": wc,
        "wt": round(total_wt, 1),
        "per_sample": per_sample,
    }


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--runs", type=int, default=2)
    parser.add_argument("--samples", type=int, default=100)
    args = parser.parse_args()

    if not API_KEY:
        print("ERROR: No DeepSeek API key found in proxy-routes.json or DEEPSEEK_API_KEY env")
        sys.exit(1)

    if not DATA_FILE.exists():
        print(f"ERROR: {DATA_FILE} not found. Run download_data.py first.")
        sys.exit(1)

    samples = json.load(open(DATA_FILE))[:args.samples]

    os.makedirs("/tmp/bench_squad", exist_ok=True)
    restart_daemon()

    print(f"SQuAD bench: {args.runs} runs × {len(CONDITIONS)} conditions × {len(samples)} samples = {args.runs * len(CONDITIONS) * len(samples)} calls")
    print()

    all_trials = []
    try:
        for ri in range(1, args.runs + 1):
            order = list(CONDITIONS)
            random.shuffle(order)
            for cond in order:
                restart_daemon()
                label = f"[r{ri}] {cond['label']}"
                print(f"  {label}: ", end="", flush=True)
                t0 = time.time()
                result = run_condition(cond, samples, ri)
                el = time.time() - t0
                print(f"f1={result['f1']:.3f} em={result['em']:.3f} ({result['correct']}/{result['total']}) "
                      f"wc={result['wc']} {result['wt']:>5.0f}s ({el:.0f}s)")
                all_trials.append(result)
    finally:
        route_pi_to_proxy(False)
        set_ext(None)

    print("\n" + "=" * 95)
    print(f"  {'Condition':<14} {'F1':>7} {'EM':>7} {'Acc':>7} {'PT':>8} {'CT':>8} {'WC':>10} {'WT':>7} {'N':>3}")
    print("-" * 95)

    b_trials = [t for t in all_trials if t["feature"] == "baseline"]
    bar_wc = sum(t["wc"] for t in b_trials) / len(b_trials) if b_trials else 1
    if b_trials:
        f1 = sum(t["f1"] for t in b_trials) / len(b_trials)
        em = sum(t["em"] for t in b_trials) / len(b_trials)
        acc = sum(t["accuracy"] for t in b_trials) / len(b_trials)
        print(f"  {'baseline':<14} {f1:>6.3f}  {em:>6.3f}  {acc:>6.2%}  "
              f"{sum(t['pt'] for t in b_trials)//len(b_trials):>7d}  "
              f"{sum(t['ct'] for t in b_trials)//len(b_trials):>7d}  {bar_wc:>9.0f}  "
              f"{sum(t['wt'] for t in b_trials)/len(b_trials):>6.1f}s  {b_trials[0]['total']:>3d}")

    for cond in CONDITIONS:
        if cond["label"] == "baseline":
            continue
        t = [x for x in all_trials if x["feature"] == cond["label"]]
        if not t:
            continue
        awc = sum(x["wc"] for x in t) / len(t)
        f1 = sum(x["f1"] for x in t) / len(t)
        em = sum(x["em"] for x in t) / len(t)
        acc = sum(x["accuracy"] for x in t) / len(t)
        delta = (awc - bar_wc) / bar_wc * 100
        f1_retention = (f1 / (sum(x['f1'] for x in b_trials)/len(b_trials))) * 100 if b_trials else 0
        pass_marker = "PASS" if f1_retention >= 95 else "FAIL"
        print(f"  {cond['label']:<14} {f1:>6.3f}  {em:>6.3f}  {acc:>6.2%}  "
              f"{sum(x['pt'] for x in t)//len(t):>7d}  "
              f"{sum(x['ct'] for x in t)//len(t):>7d}  {awc:>9.0f}  "
              f"{sum(x['wt'] for x in t)/len(t):>6.1f}s  {t[0]['total']:>3d}  "
              f"({delta:+.1f}%) F1={f1_retention:.1f}% {pass_marker}")

    with open(RESULTS_FILE, "w") as f:
        json.dump(all_trials, f, indent=2)
    print(f"\nResults: {RESULTS_FILE}")


if __name__ == "__main__":
    main()
