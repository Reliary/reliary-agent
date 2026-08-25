#!/usr/bin/env python3
"""Persistent-session test: load pack once, measure 20-turn cost vs fresh sessions."""
import json, subprocess, time, os, sys, re

PROJECT = "$HOME/src/reliary8"
RESULTS = os.path.join(os.path.dirname(os.path.abspath(__file__)), "results", "persistent_session.jsonl")
OPENCODE = "/home/linuxbrew/.linuxbrew/bin/opencode"
MODEL = "deepseek/deepseek-v4-flash"
TIMEOUT = 45
WARMUP_ENABLED = bool(os.environ.get("RELIARY_PACK_WARMUP_ENABLED"))

TASKS = [
    ("hex_bounds", "In `skeleton()`, what are the minimum and maximum hex hash lengths it detects?"),
    ("first_comment", "In `compress_content()`, is the first comment KEPT or STRIPPED?"),
    ("skeleton_return", "What does `skeleton()` return for an EMPTY input string?"),
    ("djb_hash", "What hash algorithm does `skeleton_hash()` use? What's the initial value?"),
    ("aggressive_diff", "How does `aggressive_skeleton()` handle single-letter words vs `skeleton()`?"),
    ("maxwell_gates", "What are the three gates in `MaxwellGate`? How are they combined?"),
    ("uuid_positions", "What are the UUID dash positions in `skeleton()`? List all four numbers."),
    ("compress_imports", "How does `compress_content()` handle import lines? Does it collapse them?"),
    ("linetype_order", "In `classify_line()`, does Error come BEFORE or AFTER Comment?"),
    ("skeleton_crate", "Which CRATE is `skeleton()` defined in?"),
    ("auto_pass", "What is the auto-pass threshold in `MaxwellGate::score()`? For what length?"),
    ("should_drop", "What does `should_drop()` return for lines with fewer than 2 characters?"),
    ("version_detect", "How does `skeleton()` detect version strings like 1.2.3?"),
    ("progress_bars", "How does `skeleton()` handle progress bars?"),
    ("find_clusters", "What does `find_clusters()` return? What min_run default?"),
    ("skeleton_groups", "What does `skeleton_groups()` do that `find_clusters()` doesn't?"),
    ("brace_cache", "What is `BRACE_CACHE` and what does it cache?"),
    ("word_rule", "In `skeleton()`, when does the word-NNN rule fire?"),
    ("skeleton_hash_input", "Does `skeleton_hash()` hash the original text or the skeleton output?"),
    ("djb_multiplier", "What multiplier does the djb hash in `skeleton_hash()` use?"),
]

EXPECTED = {
    "hex_bounds": ["7", "40", "hex", "hash"],
    "first_comment": ["first", "comment", "keep"],
    "skeleton_return": ["empty", "string", "new"],
    "djb_hash": ["djb", "5381"],
    "aggressive_diff": ["single", "letter", "verbatim", "keep"],
    "maxwell_gates": ["entropy", "compression", "diversity", "and"],
    "uuid_positions": ["8", "13", "18", "23"],
    "compress_imports": ["collapse", "import"],
    "linetype_order": ["error", "before", "comment"],
    "skeleton_crate": ["sift", "reliary"],
    "auto_pass": ["pass", "50"],
    "should_drop": ["true", "empty", "blank"],
    "version_detect": ["dot", "separated", "numeric"],
    "progress_bars": ["progress", "bar"],
    "find_clusters": ["cluster", "line"],
    "skeleton_groups": ["group", "prefix"],
    "brace_cache": ["cache", "brace"],
    "word_rule": ["word", "nnn"],
    "skeleton_hash_input": ["skeleton", "output"],
    "djb_multiplier": ["33"],
}

def score(task_id, text):
    keywords = EXPECTED.get(task_id, [])
    found = sum(1 for kw in keywords if kw.lower() in text.lower())
    pct = found / len(keywords) if keywords else 0
    if pct >= 0.7: return 3
    if pct >= 0.4: return 2
    if pct >= 0.15: return 1
    return 0

def run_opencode(task, session_id=None):
    args = [OPENCODE, "run", "--model", MODEL, "--agent", "build", "--format", "json", task]
    if session_id:
        args = [OPENCODE, "run", "-c", "-s", session_id, "--model", MODEL, "--agent", "build", "--format", "json", task]
    
    t0 = time.time()
    try:
        result = subprocess.run(args, capture_output=True, text=True, timeout=TIMEOUT, cwd=PROJECT)
    except subprocess.TimeoutExpired:
        return {"elapsed": TIMEOUT, "score": 0, "tools": 0, "cost": 0, "text": "TIMEOUT", "session_id": session_id}
    
    elapsed = time.time() - t0
    events = []
    for l in result.stdout.splitlines():
        l = l.strip()
        if l.startswith('{'):
            try: events.append(json.loads(l))
            except: pass
    
    texts, cost, tools, sid = [], 0, 0, session_id
    for e in events:
        if e.get('type') == 'text':
            texts.append(e.get('part', {}).get('text', ''))
        elif e.get('type') == 'tool_use':
            tools += 1
        elif e.get('type') == 'step_finish':
            cost = e.get('part', {}).get('cost', 0)
        elif e.get('sessionID'):
            sid = e['sessionID']
    
    return {"elapsed": elapsed, "tools": tools, "cost": cost, "text": '\n'.join(texts), "session_id": sid}


def warmup_session(session_id: str, task_text: str):
    """S3 fix: inject a warmup question before the first real task.
    Forces the model to actively read the pack before answering.
    """
    # Extract the primary function name from the first task
    fn_match = re.search(r'`(\w+)`', task_text)
    func = fn_match.group(1) if fn_match else "skeleton"

    warmup = f"""Before answering the real questions, familiarize yourself with the pack.

Read these entries and answer briefly:
1. What does `{func}` return for empty/blank input?
2. What's a key surprise or edge case in `{func}`?
3. Which functions call or interact with `{func}`?

Reply briefly for each."""
    r = run_opencode(warmup, session_id=session_id)
    return {
        "warmup_cost": r['cost'],
        "warmup_elapsed": r['elapsed'],
        "warmup_tools": r['tools'],
        "warmup_text": r['text'][:200],
    }


# === SESSION MODE: Turn 0 loads pack, turns 1-20 use -c ===
print("=== SESSION MODE ===")
pack = subprocess.run(
    ["$HOME/src/reliary8/target/release/reliary", "pack", PROJECT, "--format", "l2l3"],
    capture_output=True, text=True, timeout=30, cwd=PROJECT,
).stdout
print(f"Pack: {len(pack.splitlines())} lines, {len(pack)} chars")

# Turn 0: load pack, get session ID
print("Turn 0: loading pack...", end=" ", flush=True)
t0 = run_opencode(f"You are analyzing the reliary8 codebase. Read this pack carefully and reply READY.\n\n{pack[:25000]}")
sid = t0.get("session_id") or "session-0"
print(f"sid={sid[:20]}... elapsed={t0['elapsed']:.1f}s cost=\${t0['cost']:.5f}")

s_cost, s_tools, s_score, s_time = t0['cost'], 0, 0, t0['elapsed']
s_results = []
for i, (tid, task) in enumerate(TASKS):
    print(f"  Turn {i+1}/20: {tid}...", end=" ", flush=True)
    r = run_opencode(task, session_id=sid)
    sc = score(tid, r['text'])
    s_cost += r['cost']; s_tools += r['tools']; s_score += sc; s_time += r['elapsed']
    s_results.append({**r, "id": tid, "score": sc, "mode": "session", "cond": "F"})
    print(f"score={sc}/3 elapsed={r['elapsed']:.1f}s tools={r['tools']} cost=\${r['cost']:.5f}")

# === FRESH MODE: no pack, independent queries ===
print("\n=== FRESH MODE ===")
f_cost, f_tools, f_score, f_time = 0, 0, 0, 0
f_results = []
for i, (tid, task) in enumerate(TASKS):
    print(f"  Task {i+1}/20: {tid}...", end=" ", flush=True)
    r = run_opencode(task)
    sc = score(tid, r['text'])
    f_cost += r['cost']; f_tools += r['tools']; f_score += sc; f_time += r['elapsed']
    f_results.append({**r, "id": tid, "score": sc, "mode": "fresh", "cond": "A"})
    print(f"score={sc}/3 elapsed={r['elapsed']:.1f}s tools={r['tools']} cost=\${r['cost']:.5f}")

n = len(TASKS)
print(f"\n=== COMPARISON ({n} turns) ===")
print(f"SESSION (pack loaded once, -c continuation):")
print(f"  Score: {s_score}/{n*3} ({s_score/(n*3)*100:.0f}%)")
print(f"  Cost:  \${s_cost:.5f} total  (\${s_cost/n:.5f}/turn)")
print(f"  Tools: {s_tools} ({s_tools/n:.1f}/turn)")
print(f"  Time:  {s_time:.0f}s ({s_time/n:.1f}s/turn)")
print()
print(f"FRESH (no pack, independent queries):")
print(f"  Score: {f_score}/{n*3} ({f_score/(n*3)*100:.0f}%)")
print(f"  Cost:  \${f_cost:.5f} total  (\${f_cost/n:.5f}/turn)")
print(f"  Tools: {f_tools} ({f_tools/n:.1f}/turn)")
print(f"  Time:  {f_time:.0f}s ({f_time/n:.1f}s/turn)")

with open(RESULTS, "w") as f:
    for r in s_results + f_results:
        f.write(json.dumps(r) + "\n")
print(f"\nResults: {RESULTS}")
