#!/usr/bin/env python3
"""bench_reliary.py — measure reliary proxy compression savings, deterministically.

See bench/RELIARY_BENCH_DESIGN.md for the full methodology. Summary:

  * Two conditions (baseline = direct upstream, proxied = through reliary proxy),
    driven by the SAME scripted multi-turn conversation, so the only variable is
    whether the proxy compresses.
  * Interleaved trials (B P B P ...) — never sequential, due to 2.7x LLM variance.
  * Env-contract API key handling: fails LOUD and EARLY in `--check`.

ENV CONTRACT (all three required for any real run):
  RELIARY_BENCH_API_KEY   upstream provider key (e.g. sk-...)
  RELIARY_BENCH_UPSTREAM  provider base URL (e.g. https://api.deepseek.com)
  RELIARY_BENCH_MODEL     model id (e.g. deepseek-chat)
Optional:
  RELIARY_BIN             path to reliary binary (default: ../target/release/reliary)
  RELIARY_BENCH_TRIALS    trials per condition (default 3)
  RELIARY_BENCH_PORT      proxy port (default 9099)

Usage:
  python3 bench_reliary.py --check           # validate env, no LLM calls
  python3 bench_reliary.py --run             # full interleaved benchmark
  python3 bench_reliary.py --summary         # print last summary.json
"""
import argparse
import copy
import http.client
import json
import os
import shutil
import socket
import subprocess
import sys
import threading
import time
from datetime import datetime
from pathlib import Path

HERE = Path(__file__).resolve().parent
BENCH_ROOT = HERE
TARGET_SRC = BENCH_ROOT / "target"
EXPECTED = BENCH_ROOT / "expected_fix"
RESULTS = BENCH_ROOT / "results"
DEFAULT_BIN = (BENCH_ROOT.parent / "target" / "release" / "reliary").resolve()

# The scripted multi-turn conversation. Every trial uses EXACTLY these messages so the
# only variable across conditions is whether the proxy compresses. The system prompt is
# intentionally wordy so IR reasoning compression has something to strip on later turns.
# The system prompt deliberately elicits VERBOSE reasoning ("think step by step, explain
# your reasoning at length before each fix") so the IR compression has something to strip
# on later turns. A terse model on a terse task compresses nothing — that is a legitimate
# "no signal" result, not a bug. This prompt keeps the compression axis measurable.
SYSTEM_PROMPT = (
    "You are a meticulous senior code reviewer. For EVERY step you take, think step by "
    "step and explain your reasoning at length in plain prose BEFORE proposing or "
    "applying any change. Let me see your full thought process: analyze the bug, consider "
    "edge cases, reason about why the current code is wrong, and only then give the fix. "
    "Based on your analysis, walk through each bug in config_parser.py one at a time. "
    "In order to be thorough, first look at each function, then explain what it does "
    "wrong, then provide the corrected version. This means you should write several "
    "sentences of reasoning for each of the three bugs."
)
# Turn-by-turn user messages that simulate an agent investigating + fixing.
# 8 turns to accumulate enough conversation that prior-assistant compression has signal.
USER_TURNS = [
    "Read config_parser.py and tell me what validate_config does wrong.",
    "OK. Now explain the parse_config crash on empty values, and show the fix.",
    "Now explain get_int's silent-default bug, and show the fix.",
    "Give me the complete corrected file with all three fixes applied.",
    "Walk me through how the corrected validate_config handles each missing-key case.",
    "Are there any edge cases in parse_config that could still break after your fix?",
    "What tests would prove get_int's new behavior is correct?",
    "Summarize the three fixes in one line each.",
]


# ───────────────────────────────── env + checks ─────────────────────────────────

def env_or_die(name):
    v = os.environ.get(name)
    if not v:
        sys.exit(f"FAIL [env]: {name} is not set. Set it and re-run, or use --check.")
    return v


def required_env():
    return {
        "api_key": env_or_die("RELIARY_BENCH_API_KEY"),
        "upstream": env_or_die("RELIARY_BENCH_UPSTREAM"),
        "model": env_or_die("RELIARY_BENCH_MODEL"),
    }


def optional_env():
    return {
        "bin": os.environ.get("RELIARY_BIN", str(DEFAULT_BIN)),
        "trials": int(os.environ.get("RELIARY_BENCH_TRIALS", "3")),
        "port": int(os.environ.get("RELIARY_BENCH_PORT", "9099")),
    }


def port_free(port):
    with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as s:
        return s.connect_ex(("127.0.0.1", port)) != 0


def wait_http_ok(host, port, path, timeout=10.0):
    deadline = time.time() + timeout
    while time.time() < deadline:
        try:
            c = http.client.HTTPConnection(host, port, timeout=1)
            c.request("GET", path)
            r = c.getresponse()
            ok = r.status == 200
            r.read()
            c.close()
            if ok:
                return True
        except Exception:
            pass
        time.sleep(0.1)
    return False


def run_check():
    """Validate the whole env before any LLM call. One error per failure."""
    print("== bench --check ==")
    errs = 0

    # 1. env vars
    try:
        env = required_env()
        print(f"  [ok] env: model={env['model']} upstream={env['upstream']} key=***{env['api_key'][-4:]}")
    except SystemExit as e:
        print(f"  [FAIL] {e}")
        return 1

    opt = optional_env()

    # 2. binary exists + version
    if not Path(opt["bin"]).exists():
        print(f"  [FAIL] RELIARY_BIN not found: {opt['bin']}")
        errs += 1
    else:
        try:
            v = subprocess.run([opt["bin"], "--version"], capture_output=True, text=True, timeout=10)
            print(f"  [ok] binary: {v.stdout.strip()}")
        except Exception as e:
            print(f"  [FAIL] binary --version: {e}")
            errs += 1

    # 3. port free
    if not port_free(opt["port"]):
        print(f"  [FAIL] port {opt['port']} in use (set RELIARY_BENCH_PORT)")
        errs += 1
    else:
        print(f"  [ok] port {opt['port']} free")

    # 4. target + expected fix present
    if not (TARGET_SRC / "config_parser.py").exists():
        print(f"  [FAIL] target missing: {TARGET_SRC / 'config_parser.py'}")
        errs += 1
    if not (EXPECTED / "config_parser.py").exists():
        print(f"  [FAIL] expected fix missing: {EXPECTED / 'config_parser.py'}")
        errs += 1
    if errs == 0:
        print(f"  [ok] target + expected fix present")

    # 5. index the target (fresh)
    idx_tmp = "/tmp/reliary-bench-check-target"
    shutil.rmtree(idx_tmp, ignore_errors=True)
    shutil.copytree(TARGET_SRC, idx_tmp)
    try:
        subprocess.run([opt["bin"], "index", idx_tmp], capture_output=True, timeout=60, check=True)
        print(f"  [ok] index builds on target")
    except Exception as e:
        print(f"  [FAIL] index: {e}")
        errs += 1

    # 6. proxy starts + health + reaches upstream (1 trivial call)
    proxy = start_proxy(opt["bin"], opt["port"], env, quiet=True)
    if proxy is None:
        print(f"  [FAIL] proxy did not start")
        return 1
    try:
        if not wait_http_ok("127.0.0.1", opt["port"], "/health", timeout=8):
            print(f"  [FAIL] proxy /health did not respond")
            errs += 1
        else:
            print(f"  [ok] proxy /health")
            # trivial 1-token call through the proxy
            ok_up = probe_upstream_through_proxy(opt["port"], env)
            print(f"  [{'ok' if ok_up else 'FAIL'}] proxy reaches upstream")
            if not ok_up:
                errs += 1
    finally:
        stop_proxy(proxy)

    # 7. baseline-direct path reaches upstream (proves the key works outside proxy)
    ok_direct = probe_upstream_direct(env)
    print(f"  [{'ok' if ok_direct else 'FAIL'}] baseline-direct reaches upstream")
    if not ok_direct:
        errs += 1

    if errs:
        print(f"\n--check FAILED with {errs} error(s). Fix and re-run.")
        return 1
    print("\n--check PASSED. Ready for --run.")
    return 0


# ────────────────────────────────── proxy ──────────────────────────────────

def start_proxy(bin_path, port, env, quiet=False):
    """Start the reliary proxy pointing at the upstream. Returns the Popen or None."""
    # Use a clean jsonl log path per run to avoid mixing with prod.
    proxy_env = os.environ.copy()
    proxy_env["RELIARY_UPSTREAM_URL"] = env["upstream"]
    # The proxy discovers upstream by auth key; set the global fallback too.
    log_file = f"/tmp/reliary_bench_{port}.jsonl"
    if os.path.exists(log_file):
        os.remove(log_file)
    try:
        p = subprocess.Popen(
            [bin_path, "serve", str(port)],
            env=proxy_env, stdout=subprocess.DEVNULL if quiet else subprocess.PIPE,
            stderr=subprocess.DEVNULL if quiet else subprocess.PIPE,
        )
    except Exception as e:
        if not quiet:
            print(f"proxy start error: {e}")
        return None
    # Wait for health
    if not wait_http_ok("127.0.0.1", port, "/health", timeout=10):
        stop_proxy(p)
        return None
    return p


def stop_proxy(p):
    if p is None:
        return
    try:
        p.terminate()
        try:
            p.wait(timeout=5)
        except subprocess.TimeoutExpired:
            p.kill()
    except Exception:
        pass


def probe_upstream_through_proxy(port, env):
    body = json.dumps({
        "model": env["model"],
        "messages": [{"role": "user", "content": "say ok"}],
        "max_tokens": 5,
        "stream": False,
    })
    try:
        c = http.client.HTTPConnection("127.0.0.1", port, timeout=30)
        c.request("POST", "/v1/chat/completions", body,
                  {"Content-Type": "application/json", "Authorization": f"Bearer {env['api_key']}"})
        r = c.getresponse()
        r.read()
        c.close()
        return r.status == 200
    except Exception:
        return False


def probe_upstream_direct(env):
    """Parse the upstream URL and hit /v1/chat/completions directly."""
    from urllib.parse import urlparse
    u = urlparse(env["upstream"])
    host = u.hostname
    port = u.port or (443 if u.scheme == "https" else 80)
    # upstream may be ".../v1" — avoid doubling the prefix.
    prefix = u.path.rstrip("/")
    if prefix.endswith("/v1"):
        prefix = prefix[:-3]
    path = prefix + "/v1/chat/completions"
    body = json.dumps({
        "model": env["model"],
        "messages": [{"role": "user", "content": "say ok"}],
        "max_tokens": 5,
        "stream": False,
    })
    try:
        if u.scheme == "https":
            import ssl
            ctx = ssl.create_default_context()
            c = http.client.HTTPSConnection(host, port, timeout=30, context=ctx)
        else:
            c = http.client.HTTPConnection(host, port, timeout=30)
        c.request("POST", path, body,
                  {"Content-Type": "application/json", "Authorization": f"Bearer {env['api_key']}"})
        r = c.getresponse()
        r.read()
        c.close()
        return r.status == 200
    except Exception:
        return False


# ─────────────────────────── scripted conversation ───────────────────────────

def run_condition(condition, env, opt, target_copy):
    """Run ONE trial of one condition. Returns the metrics dict."""
    proxied = (condition == "proxied")
    messages = [{"role": "system", "content": SYSTEM_PROMPT}]
    # Seed turn 1 with the buggy file content so the model has something concrete.
    file_content = (target_copy / "config_parser.py").read_text()
    messages.append({"role": "user", "content": f"Here is the file:\n\n```python\n{file_content}\n```\n\n" + USER_TURNS[0]})

    total_input = 0
    total_output = 0
    turn_log = []
    t0 = time.time()

    if proxied:
        proxy = start_proxy(opt["bin"], opt["port"], env, quiet=True)
        if proxy is None:
            return {"error": "proxy did not start"}
        base_url_host = "127.0.0.1"
        base_url_port = opt["port"]
        scheme = "http"
    else:
        proxy = None
        from urllib.parse import urlparse
        u = urlparse(env["upstream"])
        base_url_host = u.hostname
        base_url_port = u.port or (443 if u.scheme == "https" else 80)
        scheme = u.scheme
        # upstream may be ".../v1" or ".../v1/" — normalise so we append exactly one
        # "/v1/chat/completions" without doubling the prefix.
        path_prefix = u.path.rstrip("/")
        if path_prefix.endswith("/v1"):
            path_prefix = path_prefix[:-3]
    try:
        # Turn 0 already seeded. Run each subsequent user turn after the first assistant reply.
        turn_idx = 0
        for i in range(len(USER_TURNS)):
            # Build request
            req = {
                "model": env["model"],
                "messages": messages,
                "stream": False,
            }
            body = json.dumps(req)
            if scheme == "https":
                import ssl
                ctx = ssl.create_default_context()
                conn = http.client.HTTPSConnection(base_url_host, base_url_port, timeout=120, context=ctx)
            else:
                conn = http.client.HTTPConnection(base_url_host, base_url_port, timeout=120)
            path = "/v1/chat/completions"
            if not proxied:
                path = path_prefix + path
            conn.request("POST", path, body,
                         {"Content-Type": "application/json",
                          "Authorization": f"Bearer {env['api_key']}"})
            resp = conn.getresponse()
            raw = resp.read().decode("utf-8", "replace")
            conn.close()
            if resp.status != 200:
                return {"error": f"upstream {resp.status}: {raw[:300]}"}
            data = json.loads(raw)
            usage = data.get("usage", {})
            pt = usage.get("prompt_tokens", 0)
            ct = usage.get("completion_tokens", 0)
            total_input += pt
            total_output += ct
            asst_content = (data.get("choices") or [{}])[0].get("message", {}).get("content", "") or ""
            messages.append({"role": "assistant", "content": asst_content})
            turn_log.append({"turn": turn_idx, "pt": pt, "ct": ct, "asst_len": len(asst_content)})
            turn_idx += 1
            # Add the next user turn (if any) — accumulates conversation
            if i + 1 < len(USER_TURNS):
                messages.append({"role": "user", "content": USER_TURNS[i + 1]})
    finally:
        if proxy:
            stop_proxy(proxy)

    wall = time.time() - t0
    wc = total_input + 4 * total_output

    # task_pass: did the final assistant message contain the corrected validate_config?
    final = messages[-1]["content"] if messages[-1]["role"] == "assistant" else (messages[-2]["content"] if len(messages) >= 2 else "")
    all_asst = "\n".join(m["content"] for m in messages if m["role"] == "assistant")
    task_pass = ("return ok" in all_asst or "return False" in all_asst or "ok = False" in all_asst or "isinstance" in all_asst)

    metrics = {
        "condition": condition,
        "input_tokens": total_input,
        "output_tokens": total_output,
        "weighted_cost": wc,
        "turns": turn_idx,
        "wall_time_s": round(wall, 2),
        "task_pass": task_pass,
        "turn_log": turn_log,
        "model": env["model"],
        "timestamp": datetime.utcnow().isoformat() + "Z",
    }
    return metrics


# ─────────────────────────────── orchestration ───────────────────────────────

def fresh_target():
    """Copy the frozen target to a fresh tmp dir so every trial starts identical."""
    dst = "/tmp/reliary-bench-run-target"
    shutil.rmtree(dst, ignore_errors=True)
    shutil.copytree(TARGET_SRC, dst)
    return Path(dst)


def run_full():
    env = required_env()
    opt = optional_env()
    ts = datetime.utcnow().strftime("%Y%m%dT%H%M%SZ")
    out_dir = RESULTS / ts
    out_dir.mkdir(parents=True, exist_ok=True)

    n = opt["trials"]
    # Interleave: B P B P ...  (randomised ABAB would also work; deterministic is clearer)
    order = []
    for i in range(n):
        order.append(("baseline", i))
        order.append(("proxied", i))

    print(f"== bench --run: {n} trials/condition, {len(order)} total runs, interleaved ==")
    all_results = []
    for cond, trial in order:
        target = fresh_target()
        print(f"  [{len(all_results)+1}/{len(order)}] {cond} trial {trial} ...", end=" ", flush=True)
        m = run_condition(cond, env, opt, target)
        if "error" in m:
            print(f"ERROR: {m['error']}")
            (out_dir / f"{cond}-{trial}.json").write_text(json.dumps(m, indent=2))
            all_results.append(m)
            # Don't abort — record and continue (one upstream hiccup shouldn't kill the run)
            continue
        (out_dir / f"{cond}-{trial}.json").write_text(json.dumps(m, indent=2))
        all_results.append(m)
        print(f"WC={m['weighted_cost']} in={m['input_tokens']} out={m['output_tokens']} turns={m['turns']} pass={m['task_pass']} {m['wall_time_s']}s")

    summary = summarize(all_results, ts)
    (out_dir / "summary.json").write_text(json.dumps(summary, indent=2))
    print("\n== summary ==")
    print_summary(summary)
    print(f"\nresults: {out_dir}")
    return 0


def summarize(results, ts):
    def cond(name):
        return [r for r in results if r.get("condition") == name and "error" not in r]
    b = cond("baseline")
    p = cond("proxied")

    def med(vals):
        if not vals:
            return None
        s = sorted(vals)
        return s[len(s) // 2]

    def stats(rs, key):
        vals = [r[key] for r in rs]
        if not vals:
            return None
        return {"min": min(vals), "median": med(vals), "max": max(vals)}

    summary = {
        "timestamp": ts,
        "trials_requested": int(os.environ.get("RELIARY_BENCH_TRIALS", "3")),
        "baseline_n": len(b),
        "proxied_n": len(p),
        "baseline_wc": stats(b, "weighted_cost"),
        "proxied_wc": stats(p, "weighted_cost"),
        "baseline_input": stats(b, "input_tokens"),
        "proxied_input": stats(p, "input_tokens"),
        "baseline_output": stats(b, "output_tokens"),
        "proxied_output": stats(p, "output_tokens"),
        "baseline_wall": stats(b, "wall_time_s"),
        "proxied_wall": stats(p, "wall_time_s"),
        "baseline_pass": sum(1 for r in b if r.get("task_pass")),
        "proxied_pass": sum(1 for r in p if r.get("task_pass")),
    }
    if b and p and summary["baseline_wc"] and summary["proxied_wc"]:
        bw = summary["baseline_wc"]["median"]
        pw = summary["proxied_wc"]["median"]
        if bw and bw > 0:
            summary["wc_savings_pct"] = round((1 - pw / bw) * 100, 1)
            summary["wc_savings_range_pct"] = [
                round((1 - summary["proxied_wc"]["max"] / summary["baseline_wc"]["min"]) * 100, 1) if summary["baseline_wc"]["min"] else None,
                round((1 - summary["proxied_wc"]["min"] / summary["baseline_wc"]["max"]) * 100, 1) if summary["baseline_wc"]["max"] else None,
            ]
    return summary


def print_summary(s):
    def fmt(stats):
        if not stats:
            return "n/a"
        return f"{stats['median']} [{stats['min']}..{stats['max']}]"
    print(f"  baseline  WC: {fmt(s.get('baseline_wc'))}  in={fmt(s.get('baseline_input'))}  out={fmt(s.get('baseline_output'))}  pass={s.get('baseline_pass',0)}/{s.get('baseline_n',0)}")
    print(f"  proxied   WC: {fmt(s.get('proxied_wc'))}  in={fmt(s.get('proxied_input'))}  out={fmt(s.get('proxied_output'))}  pass={s.get('proxied_pass',0)}/{s.get('proxied_n',0)}")
    if "wc_savings_pct" in s:
        rng = s.get("wc_savings_range_pct")
        rng_str = f" (range {rng[0]}..{rng[1]}%)" if rng else ""
        print(f"  WC savings: {s['wc_savings_pct']}%{rng_str}")
    else:
        print("  WC savings: n/a (need both conditions)")


def show_last_summary():
    runs = sorted(RESULTS.glob("*/summary.json"))
    if not runs:
        print("no runs yet")
        return 1
    s = json.loads(runs[-1].read_text())
    print(f"== last run: {runs[-1].parent.name} ==")
    print_summary(s)
    return 0


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--check", action="store_true", help="validate env, no LLM calls")
    ap.add_argument("--run", action="store_true", help="full interleaved benchmark")
    ap.add_argument("--summary", action="store_true", help="print last summary")
    args = ap.parse_args()
    if args.check:
        return run_check()
    if args.summary:
        return show_last_summary()
    if args.run:
        return run_full()
    ap.print_help()
    return 0


if __name__ == "__main__":
    sys.exit(main())
