import os
#!/usr/bin/env python3
"""Dual-domain experiment: does a structured task entry in holographic format
help the model cross-reference between codebase and task context?"""
import json, time, urllib.request, subprocess, sys

DEEPSEEK_KEY = os.environ.get("DEEPSEEK_API_KEY", "")
DEEPSEEK_URL = "https://api.deepseek.com/chat/completions"
RELIARY_BIN = "$HOME/src/reliary8/target/release/reliary"

TASK_FREETEXT = (
    "Current task: The user is debugging OAuth token refresh failures in a Rust web service. "
    "Files changed: auth.rs:142 (refresh_token), session.go:89 (validate_session). "
    "Tools used: grep, search, goto_def, find_references. "
    "Findings: refresh_token returns Ok even though expired_at < now. "
    "Clock skew suspected — NTP sync is enabled but off by 3 seconds. "
    "Provider confirms the token is valid; session expiry check may be wrong. "
    "The validate_session function checks expired_at against current time, "
    "but the time source might be stale. Needs investigation into the expiry "
    "check logic and whether a tolerance buffer exists."
)

TASK_STRUCTURED = """\
## current-task
L0: Debugging OAuth token refresh failures in a Rust web service
L2: files_changed(auth.rs:142, session.go:89), tools_called(search, goto_def, find_references)
L3: refresh_token returns Ok despite expired_at < now; clock skew suspected;
    NTP sync enabled but off by 3 seconds; provider confirms token valid;
    validate_session expiry check may be wrong; time source potentially stale
Cross-refs: refresh_token(auth.rs:142), validate_session(session.go:89),
    expiry_check(models.go:45), clock_source(util.rs:23)
"""

SYSTEM_PROMPT = (
    "You have codebase documentation and task context. Answer each question "
    "by cross-referencing between them. Reference specific functions, files, "
    "and facts from the documentation. Be concise."
)

def load_probes():
    with open("bench/dual_domain_probes.json") as f:
        return json.load(f)

def call_deepseek(sys_prompt, user_msg, timeout=30):
    payload = json.dumps({
        "model": "deepseek-chat",
        "messages": [
            {"role": "system", "content": sys_prompt},
            {"role": "user", "content": user_msg},
        ],
        "max_tokens": 300, "temperature": 0,
    }).encode()
    req = urllib.request.Request(
        DEEPSEEK_URL, data=payload,
        headers={"Content-Type": "application/json", "Authorization": f"Bearer {DEEPSEEK_KEY}"},
    )
    t0 = time.time()
    try:
        with urllib.request.urlopen(req, timeout=timeout) as resp:
            data = json.loads(resp.read())
        return (data["choices"][0]["message"]["content"],
                data.get("usage", {}), time.time() - t0)
    except Exception as e:
        return f"ERROR: {e}", {}, time.time() - t0

def score_answer(answer, expected, min_facts):
    answer_lower = answer.lower()
    found = sum(1 for kw in expected if kw.lower() in answer_lower)
    frac = found / len(expected) if expected else 0
    if frac >= 0.7: return 3
    if frac >= 0.4: return 2
    if frac >= 0.15: return 1
    return 0

def main():
    # Generate the 10-function pack
    print("Generating codebase pack...")
    pack = subprocess.run(
        [RELIARY_BIN, "pack", ".", "--format", "l2l3"],
        capture_output=True, text=True, timeout=30
    ).stdout

    probes = load_probes()
    print(f"Running {len(probes)} probes, 3 conditions...\n")

    conditions = {
        "A_free_text": f"{pack}\n\n{TASK_FREETEXT}",
        "B_structured": f"{pack}\n\n{TASK_STRUCTURED}",
        "C_pack_only": pack,
    }

    results = {c: [] for c in conditions}

    for cond_name, full_context in conditions.items():
        print(f"\n=== Condition {cond_name} ({len(full_context)} chars, ~{len(full_context)//4} tokens) ===")
        for probe in probes:
            answer, usage, wall = call_deepseek(
                SYSTEM_PROMPT,
                f"Documentation:\n\n{full_context}\n\nQuestion: {probe['question']}",
                timeout=30)
            score = score_answer(answer, probe["expected_facts"], probe["min_facts"])
            results[cond_name].append({
                "probe": probe["id"], "score": score, "wall": wall,
                "tokens_in": usage.get("prompt_tokens", 0),
                "tokens_out": usage.get("completion_tokens", 0),
                "answer": answer[:300],
            })
            print(f"  {probe['id']:<30} score={score}/3  {wall:.1f}s  in={usage.get('prompt_tokens', 0)}  out={usage.get('completion_tokens', 0)}  | {answer[:80].replace(chr(10), ' ')}")

    # Aggregate
    print("\n" + "=" * 80)
    print(f"{'Cond':<20} {'Score':>10} {'Pct':>8} {'Wall':>10} {'In':>10} {'Out':>10}")
    print("-" * 80)
    for cond_name in conditions:
        total = sum(r["score"] for r in results[cond_name])
        max_p = len(results[cond_name]) * 3
        wall = sum(r["wall"] for r in results[cond_name])
        tin = sum(r["tokens_in"] for r in results[cond_name])
        tout = sum(r["tokens_out"] for r in results[cond_name])
        print(f"{cond_name:<20} {total}/{max_p:>7} {total/max_p*100:>7.1f}% {wall:>9.1f}s {tin:>10} {tout:>10}")

    # Per-probe
    print(f"\n{'Probe':<35} {'A_freetext':>10} {'B_structured':>12} {'C_packonly':>12}  Verdict")
    print("-" * 85)
    b_wins = 0
    for i in range(len(probes)):
        pid = probes[i]["id"]
        a = results["A_free_text"][i]["score"]
        b = results["B_structured"][i]["score"]
        c = results["C_pack_only"][i]["score"]
        if b > a and b > c:
            verdict = "← STRUCTURED wins (dual-domain helps)"
            b_wins += 1
        elif b > a:
            verdict = "← B > A (structured > free-text)"
            b_wins += 1
        elif a >= b:
            verdict = "← A ≥ B (free-text is fine)"
        elif c >= b:
            verdict = "← C ≥ B (pack alone is enough)"
        else:
            verdict = ""
        print(f"{pid:<35} {a:>10} {b:>12} {c:>12}  {verdict}")

    b_total = sum(r["score"] for r in results["B_structured"])
    a_total = sum(r["score"] for r in results["A_free_text"])
    c_total = sum(r["score"] for r in results["C_pack_only"])

    print(f"\n--- VERDICT ---")
    print(f"A (free-text task):   {a_total}")
    print(f"B (structured task):  {b_total}")
    print(f"C (pack only):        {c_total}")
    if b_total > a_total:
        print(f"Dual-domain format WINS: B beats A by {b_total - a_total} points on {b_wins}/{len(probes)} probes")
    elif b_total == a_total:
        print(f"Dual-domain format DOESN'T help: B == A")
    else:
        print(f"Dual-domain format LOSES: A beats B by {a_total - b_total} points")

    # Save
    with open("bench/results/dual_domain.jsonl", "w") as f:
        for cond_name in conditions:
            for r in results[cond_name]:
                r["cond"] = cond_name
                f.write(json.dumps(r) + "\n")
    print("\nSaved to bench/results/dual_domain.jsonl")

if __name__ == "__main__":
    main()
