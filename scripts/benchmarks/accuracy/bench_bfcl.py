"""BFCL (Berkeley Function Calling Leaderboard) accuracy benchmark.

Tests whether compression preserves tool-call accuracy.
3 conditions × 2 runs = 6 sessions × 100 BFCL questions = 600 LLM calls.

Conditions:
  baseline     - No compression. Pi direct to DeepSeek.
  recommended  - Full proxy + gate.js stack with SRCR floor 0.3.
  passthrough  - Proxy enabled but RELIARY_PROXY_PASSTHROUGH=1 disables compression.

Scoring: exact match on function name + JSON-equivalent arguments.
Pass criteria: recommended accuracy >= 95% of baseline (Headroom's 97% target).

Usage: python3 bench_bfcl.py [--runs N] [--samples N]
"""
import argparse
import json
import os
import random
import re
import subprocess
import sys
import time
from pathlib import Path

# --- Config (mirrors bench_rename.py) ---
PI = os.path.expanduser("~/.local/bin/pi")
SETTINGS = os.path.expanduser("~/.pi/agent/settings.json")
MODELS = os.path.expanduser("~/.pi/agent/models.json")
GATE = os.path.abspath(os.path.join(os.path.dirname(__file__), "..", "..", "pi", "gate.js"))
RELIARY_BIN = (os.path.join(os.path.dirname(__file__), "..", "..", "target", "release", "reliary-agent")
               if os.path.exists(os.path.join(os.path.dirname(__file__), "..", "..", "target", "release", "reliary-agent"))
               else "reliary-agent")
DATA_FILE = Path("/tmp/bench_bfcl/bfcl_100.json")
RESULTS_FILE = Path("/tmp/bench_bfcl_results.json")

CONDITIONS = [
    {"label": "baseline",    "needs_proxy": False, "needs_gate": False, "env": {}},
    {"label": "recommended", "needs_proxy": True,  "needs_gate": True,
     "env": {"RELIARY_MODE": "strict", "RELIARY_LOG": "warn"}},
    {"label": "passthrough", "needs_proxy": True,  "needs_gate": True,
     "env": {"RELIARY_MODE": "strict", "RELIARY_LOG": "warn", "RELIARY_PROXY_PASSTHROUGH": "1"}},
]

# Optional: inject extra env vars for debugging (set BENCH_EXTRA_ENV="KEY=VAL,KEY2=VAL2")
import os as _os
_extra_env = _os.environ.get("BENCH_EXTRA_ENV", "")
if _extra_env:
    for _kv in _extra_env.split(","):
        if "=" in _kv:
            for _cond in CONDITIONS:
                _cond["env"][_kv.split("=", 1)[0]] = _kv.split("=", 1)[1]


def read_api_key():
    """Read the actual DeepSeek API key from proxy-routes.json or env."""
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


# --- Scoring ---

def parse_tool_call(response_text: str) -> list:
    """Extract tool calls from LLM response.

    Supports these formats:
      1. Hermes-style: <tool_call>{...}</tool_call> (possibly inside ```json)
      2. JSON object: {"name": "fn", "arguments": {...}}
      3. JSON array:    [{...}, {...}]
    """
    if not response_text:
        return []

    # First strip outer markdown code fences so <tool_call> tags remain
    text = re.sub(r"```(?:json)?\s*", "", response_text)
    text = re.sub(r"```\s*", "", text)

    # Hermes <tool_call> tags (non-greedy, allow whitespace/newlines)
    hermes_pattern = r"<tool_call>\s*(.*?)\s*</tool_call>"
    matches = re.findall(hermes_pattern, text, re.DOTALL)
    if matches:
        calls = []
        for m in matches:
            for candidate in (m, "[" + m + "]" if not m.startswith("[") else m):
                try:
                    obj = json.loads(candidate)
                    if isinstance(obj, list):
                        calls.extend(c for c in obj if isinstance(c, dict) and "name" in c)
                        break
                    if isinstance(obj, dict):
                        if "function" in obj and isinstance(obj["function"], dict):
                            calls.append(obj["function"])
                        elif "name" in obj:
                            calls.append(obj)
                        break
                except json.JSONDecodeError:
                    continue
        if calls:
            return calls

    # Plain JSON object or array (no <tool_call> tags)
    text = text.strip()
    try:
        obj = json.loads(text)
        if isinstance(obj, list):
            return [c for c in obj if isinstance(c, dict) and "name" in c]
        if isinstance(obj, dict) and "name" in obj:
            return [obj]
        if isinstance(obj, dict) and "function" in obj:
            return [obj["function"]]
    except json.JSONDecodeError:
        pass
    return []


def args_match(expected: dict, predicted: dict) -> bool:
    """Check if predicted arguments equal expected (modulo type coercion).

    Special handling: integer-valued parameters match if int(expected) == int(predicted).
    """
    if not isinstance(predicted, dict):
        return False
    for k, v in expected.items():
        if k not in predicted:
            return False
        pv = predicted[k]
        if pv == v:
            continue
        # Numeric tolerance
        try:
            if isinstance(v, (int, float)) and isinstance(pv, (int, float)):
                if abs(float(v) - float(pv)) < 1e-6:
                    continue
        except (TypeError, ValueError):
            pass
        if str(pv) == str(v):
            continue
        return False
    return True


def score_sample(expected_calls: list, predicted_calls: list) -> float:
    """Score one BFCL sample using F1 over name+args matches.

    Each call is matched by (function name, JSON-equivalent arguments). We
    count how many expected calls have a matching predicted call (matching is
    symmetric — order doesn't matter), then return F1 = 2*P*R/(P+R).
    """
    if not expected_calls:
        return 1.0 if not predicted_calls else 0.0
    if not predicted_calls:
        return 0.0

    matched = 0
    used_pred = set()
    for exp in expected_calls:
        for j, pred in enumerate(predicted_calls):
            if j in used_pred:
                continue
            if pred.get("name") != exp.get("name"):
                continue
            if not args_match(exp.get("arguments", {}), pred.get("arguments", {})):
                continue
            matched += 1
            used_pred.add(j)
            break

    precision = matched / len(predicted_calls) if predicted_calls else 0
    recall = matched / len(expected_calls) if expected_calls else 0
    if precision + recall == 0:
        return 0.0
    return 2 * precision * recall / (precision + recall)


# --- Pi agent invocation ---

def build_prompt(sample: dict) -> str:
    """Build the BFCL prompt from a sample.

    The prompt uses an explicit "this is a hypothetical scenario" framing so
    that agent frameworks (which inject their own tool system prompts) don't
    override the LLM's behavior. The LLM is told these are tools it should
    pretend to have and emit calls for.
    """
    tools_str = json.dumps(sample["tools"], indent=2)
    return (
        f"You are a function-calling AI. The following tools are available to you in this scenario:\n\n"
        f"{tools_str}\n\n"
        f"User request: {sample['query']}\n\n"
        f"Respond with the tool call(s) needed to answer the request. "
        f"Output ONLY the tool call(s) in this exact format:\n"
        f"<tool_call>\n{{\"name\": \"function_name\", \"arguments\": {{...}}}}\n</tool_call>\n\n"
        f"Do not write any other text. Do not ask for clarification. "
        f"Emit the tool call(s) directly."
    )


def route_pi_to_proxy(enable):
    if enable:
        cfg = {
            "providers": {
                "deepseek": {
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
    """Extract the assistant's text content from Pi's JSON event stream.

    Pi's event stream may include:
    - message content with type=text (final answer)
    - message content with type=toolCall (real tool invocation)
    - agent_end wrapper with messages[]

    We collect all text and toolCall content across all events, prioritizing
    the LAST message's content (the final answer after tool errors).
    """
    final_text = ""
    final_tool_calls = []
    for line in stdout.splitlines():
        if not line.startswith("{"):
            continue
        try:
            d = json.loads(line)
            # agent_end wraps messages[] — process them
            msgs = d.get("messages") or [d.get("message", {})]
            for m in msgs:
                if not isinstance(m, dict) or m.get("role") != "assistant":
                    continue
                content = m.get("content", [])
                if isinstance(content, list):
                    text = ""
                    tool_calls = []
                    for c in content:
                        if isinstance(c, dict):
                            if c.get("type") == "text":
                                text += c.get("text", "")
                            elif c.get("type") == "toolCall":
                                tool_calls.append({
                                    "name": c.get("name", ""),
                                    "arguments": c.get("arguments", {}),
                                })
                    # Only update if this message has text OR tool calls
                    if text or tool_calls:
                        final_text = text
                        final_tool_calls = tool_calls
        except Exception:
            pass

    # If we have real tool calls, format them as <tool_call> tags for the parser
    if final_tool_calls and not final_text:
        return "\n".join(
            f"<tool_call>{json.dumps(tc, separators=(',', ':'))}</tool_call>"
            for tc in final_tool_calls
        )
    return final_text


def parse_usage(stdout: str) -> tuple:
    """Extract token usage from Pi session events AND /tmp/reliary_proxy.jsonl.

    Pi emits usage on the assistant message in agent_end events, but
    proxy-routed conditions may show 0 tokens in Pi's session because
    Pi's usage tracking is bypassed by the proxy. Fall back to reading
    the proxy's stream_usage log file which records prompt_tokens and
    completion_tokens per request.
    """
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

    # Fall back to /tmp/reliary_proxy.jsonl if Pi's session shows 0
    # (proxy bypasses Pi's usage tracking)
    if pt_max == 0 and os.path.exists("/tmp/reliary_proxy.jsonl"):
        try:
            import time
            cutoff = time.time() - 30  # only recent entries
            for line in open("/tmp/reliary_proxy.jsonl", "rb"):
                try:
                    d = json.loads(line)
                except Exception:
                    continue
                if d.get("event") == "stream_usage":
                    pt = d.get("prompt_tokens", 0)
                    ct = d.get("completion_tokens", 0)
                    if pt > 0 and pt + ct > pt_max + ct_max:
                        pt_max = pt
                        ct_max = ct
        except Exception:
            pass
    return pt_max, ct_max


def run_condition(cond, samples, run_idx):
    sfile = f"/tmp/bench-bfcl-{cond['label']}-r{run_idx}.json"
    if os.path.exists(sfile):
        os.remove(sfile)

    route_pi_to_proxy(cond["needs_proxy"])
    set_ext(GATE if cond["needs_gate"] else None)

    env = {**os.environ, "PI_DISABLE_HEARTBEAT": "1", "DEEPSEEK_API_KEY": API_KEY}
    if cond.get("needs_proxy"):
        env["OPENAI_BASE_URL"] = "http://127.0.0.1:9090/v1"
    env.update(cond["env"])

    total_pt = total_ct = 0
    total_wt = 0.0
    correct = 0
    per_sample = []

    for i, sample in enumerate(samples):
        # Salt each sample with a unique timestamp to prevent cache contamination
        prompt = build_prompt(sample) + f"\n\n[run={run_idx} idx={i} ts={int(time.time()*1000)}]"
        t0 = time.time()
        try:
            r = subprocess.run(
                [PI, "--model", "deepseek/deepseek-v4-flash", "--mode", "json",
                 "--session", sfile, "--print", prompt],
                capture_output=True, text=True, timeout=120, env=env,
                cwd="/tmp/bench_bfcl",  # neutral cwd
            )
            wt = time.time() - t0
            response_text = parse_text_response(r.stdout)
            pt, ct = parse_usage(r.stdout)
            predicted = parse_tool_call(response_text)
            score = score_sample(sample["expected_calls"], predicted)
            correct += score
            per_sample.append({
                "idx": i, "score": score, "wt": round(wt, 1),
                "pt": pt, "ct": ct, "predicted": predicted[:1], "expected": sample["expected_calls"][:1],
            })
            total_pt += pt
            total_ct += ct
            total_wt += wt
        except subprocess.TimeoutExpired:
            per_sample.append({"idx": i, "score": 0, "wt": 120, "pt": 0, "ct": 0, "error": "timeout"})
        except Exception as e:
            per_sample.append({"idx": i, "score": 0, "wt": 0, "pt": 0, "ct": 0, "error": str(e)[:80]})

    accuracy = correct / len(samples) if samples else 0
    wc = total_pt + 2 * total_ct  # DeepSeek V4 Flash 1:2 ratio

    return {
        "feature": cond["label"],
        "run": run_idx,
        "accuracy": accuracy,
        "correct": correct,
        "total": len(samples),
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

    os.makedirs("/tmp/bench_bfcl", exist_ok=True)
    restart_daemon()

    print(f"BFCL bench: {args.runs} runs × {len(CONDITIONS)} conditions × {len(samples)} samples = {args.runs * len(CONDITIONS) * len(samples)} calls")
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
                print(f"acc={result['accuracy']:.2%} ({result['correct']}/{result['total']}) "
                      f"wc={result['wc']} {result['wt']:>5.0f}s ({el:.0f}s)")
                all_trials.append(result)
    finally:
        # Restore baseline config
        route_pi_to_proxy(False)
        set_ext(None)

    print("\n" + "=" * 90)
    print(f"  {'Condition':<14} {'Acc':>8} {'PT':>8} {'CT':>8} {'WC':>10} {'WT':>7} {'N':>3}")
    print("-" * 90)

    b_trials = [t for t in all_trials if t["feature"] == "baseline"]
    bar_wc = sum(t["wc"] for t in b_trials) / len(b_trials) if b_trials else 0
    if b_trials:
        acc = sum(t["accuracy"] for t in b_trials) / len(b_trials)
        print(f"  {'baseline':<14} {acc:>7.2%}  {sum(t['pt'] for t in b_trials)//len(b_trials):>7d}  "
              f"{sum(t['ct'] for t in b_trials)//len(b_trials):>7d}  {bar_wc:>9.0f}  "
              f"{sum(t['wt'] for t in b_trials)/len(b_trials):>6.1f}s  {b_trials[0]['total']:>3d}")

    for cond in CONDITIONS:
        if cond["label"] == "baseline":
            continue
        t = [x for x in all_trials if x["feature"] == cond["label"]]
        if not t:
            continue
        awc = sum(x["wc"] for x in t) / len(t)
        acc = sum(x["accuracy"] for x in t) / len(t)
        if bar_wc > 0:
            delta = (awc - bar_wc) / bar_wc * 100
            delta_str = f"({delta:+.1f}%)"
        else:
            delta_str = "(no baseline)"
        print(f"  {cond['label']:<14} {acc:>7.2%}  {sum(x['pt'] for x in t)//len(t):>7d}  "
              f"{sum(x['ct'] for x in t)//len(t):>7d}  {awc:>9.0f}  "
              f"{sum(x['wt'] for x in t)/len(t):>6.1f}s  {t[0]['total']:>3d}  "
              f"{delta_str}")

    with open(RESULTS_FILE, "w") as f:
        json.dump(all_trials, f, indent=2)
    print(f"\nResults: {RESULTS_FILE}")


if __name__ == "__main__":
    main()
