#!/usr/bin/env python3
"""Edit-bench: run code edit tasks, apply model output, compile, test.

Conditions:
  A — reliary tools, no pack
  F — reliary tools + per-query slice pack
  E — CBM (codebase-memory-mcp)

Each task is scored on 4 axes:
  1. Structural: did the model touch the right file/function? (0-1)
  2. Syntactic: does cargo check pass? (0-1)
  3. Behavioral: does the pre-written test pass? (0-1)
  4. Keyword: did the model mention the right concepts? (0-3)
  Total: 0-6
"""
import json, os, sys, time, shutil, subprocess, tempfile, re, hashlib

RELIARY_BIN = "$HOME/src/reliary8/target/release/reliary"
RELIARY_REPO = "$HOME/src/reliary8"
DEEPSEEK_KEY = os.environ.get("DEEPSEEK_API_KEY", "")

# ─── Pack generation ───────────────────────────────────────────────

def build_full_pack():
    """Build the full holographic pack for reliary8."""
    r = subprocess.run(
        [RELIARY_BIN, "pack", RELIARY_REPO, "--format", "l2l3"],
        capture_output=True, text=True, timeout=30)
    return r.stdout if r.returncode == 0 else ""

def slice_pack(full_pack, query, top_k=10):
    """Slice the full pack for a query via BM25.
    The slicer builds the pack from the repo index, then slices.
    We don't need to pass the full pack — the slicer reads the index directly.
    """
    r = subprocess.run(
        [RELIARY_BIN, "pack", "--slice-query", query, "--top-k", str(top_k), RELIARY_REPO],
        capture_output=True, text=True, timeout=30)
    return r.stdout if r.returncode == 0 else ""

# ─── DeepSeek API ──────────────────────────────────────────────────

def call_deepseek(system_prompt, user_msg, max_tokens=4000, timeout=90):
    """Call DeepSeek API and return (text, usage_dict, wall_time)."""
    import urllib.request, urllib.error
    t0 = time.time()
    body = json.dumps({
        "model": "deepseek-chat",
        "messages": [
            {"role": "system", "content": system_prompt},
            {"role": "user", "content": user_msg},
        ],
        "max_tokens": max_tokens,
        "temperature": 0,
        "stream": False,
    }).encode()
    req = urllib.request.Request(
        "https://api.deepseek.com/chat/completions",
        data=body,
        headers={
            "Authorization": f"Bearer {DEEPSEEK_KEY}",
            "Content-Type": "application/json",
        },
    )
    try:
        with urllib.request.urlopen(req, timeout=timeout) as resp:
            data = json.loads(resp.read())
        wall = time.time() - t0
        text = data["choices"][0]["message"]["content"]
        usage = data.get("usage", {})
        return text, usage, wall
    except Exception as e:
        return f"ERROR: {e}", {}, time.time() - t0

# ─── Code extraction ───────────────────────────────────────────────

def extract_code_blocks(text):
    """Extract Rust code blocks from markdown, with optional file annotations."""
    blocks = []
    # Pattern: optional file path comment before ```rust, then code
    # Also handle: ```rust\n// File: path/to/file.rs\n...\n```
    for m in re.finditer(r'```(?:rust|rs)?\n(.*?)```', text, re.DOTALL):
        code = m.group(1)
        # Check if the first line is a file annotation
        first_line = code.split('\n')[0].strip()
        file_path = None
        if first_line.startswith('// File:'):
            file_path = first_line.replace('// File:', '').strip()
            code = '\n'.join(code.split('\n')[1:])
        elif first_line.startswith('// In '):
            file_path = first_line.replace('// In ', '').strip().rstrip(':')
            code = '\n'.join(code.split('\n')[1:])
        blocks.append({"code": code, "file_hint": file_path})
    return blocks

def extract_function_from_block(code_block, func_name):
    """Try to extract a specific function from a code block."""
    patterns = [
        rf'(pub\s+)?fn\s+{re.escape(func_name)}\s*\(',
    ]
    for pattern in patterns:
        m = re.search(pattern, code_block)
        if m:
            start = m.start()
            brace_count = 0
            for i in range(start, len(code_block)):
                if code_block[i] == '{':
                    brace_count += 1
                elif code_block[i] == '}':
                    brace_count -= 1
                    if brace_count == 0:
                        return code_block[start:i+1]
    return None

def extract_enum_from_block(code_block, enum_name):
    """Try to extract a specific enum from a code block."""
    pattern = rf'(pub\s+)?enum\s+{re.escape(enum_name)}\s*\{{'
    m = re.search(pattern, code_block)
    if m:
        start = m.start()
        brace_count = 0
        for i in range(start, len(code_block)):
            if code_block[i] == '{':
                brace_count += 1
            elif code_block[i] == '}':
                brace_count -= 1
                if brace_count == 0:
                    return code_block[start:i+1]
    return None

def extract_struct_from_block(code_block, struct_name):
    """Try to extract a specific struct from a code block."""
    pattern = rf'(pub\s+)?struct\s+{re.escape(struct_name)}\s*\{{'
    m = re.search(pattern, code_block)
    if m:
        start = m.start()
        brace_count = 0
        for i in range(start, len(code_block)):
            if code_block[i] == '{':
                brace_count += 1
            elif code_block[i] == '}':
                brace_count -= 1
                if brace_count == 0:
                    return code_block[start:i+1]
    return None

# ─── Edit application + compilation ────────────────────────────────

def apply_edit_and_test(task, model_output, repo_path):
    """Apply the model's edit to a temp git worktree and run cargo check + test.

    Handles multi-site edits: the model may produce multiple code blocks
    for different files (e.g., enum definition + function change).

    Returns: (structural, syntactic, behavioral, details)
    """
    # 1. Extract code blocks (now with file hints)
    blocks = extract_code_blocks(model_output)
    if not blocks:
        return 0, 0, 0, "no code blocks found"

    # 2. Create a git worktree
    tmpdir = tempfile.mkdtemp(prefix="edit_bench_")
    worktree_path = os.path.join(tmpdir, "worktree")
    try:
        wt_result = subprocess.run(
            ["git", "worktree", "add", "--detach", worktree_path],
            cwd=repo_path, capture_output=True, text=True, timeout=30
        )
        if wt_result.returncode != 0:
            return 0, 0, 0, f"git worktree failed: {wt_result.stderr[:200]}"

        # 3. Apply each code block to the appropriate file
        target_file_rel = task["target_file"]
        target_func = task["target_function"]
        files_modified = set()

        for block_info in blocks:
            code = block_info["code"]
            file_hint = block_info["file_hint"]

            # Determine which file to apply this block to
            if file_hint:
                # Model annotated the block with a file path
                target_file = file_hint
            else:
                # Use the task's target file
                target_file = target_file_rel

            target_file_abs = os.path.join(worktree_path, target_file)
            if not os.path.exists(target_file_abs):
                # Try to find the file by basename
                basename = os.path.basename(target_file)
                for root, dirs, files in os.walk(os.path.join(worktree_path, "crates")):
                    if basename in files:
                        target_file_abs = os.path.join(root, basename)
                        target_file = os.path.relpath(target_file_abs, worktree_path)
                        break

            if not os.path.exists(target_file_abs):
                continue  # skip blocks for non-existent files

            with open(target_file_abs, "r") as f:
                original_source = f.read()

            edited_source = original_source

            # Try to find and replace a function
            replaced = False
            for func_name in [target_func, "normalize", "normalize_uuid"]:
                original_func = extract_function_from_block(original_source, func_name)
                edited_func = extract_function_from_block(code, func_name)
                if original_func and edited_func:
                    edited_source = edited_source.replace(original_func, edited_func)
                    replaced = True
                    break

            # Try to find and replace an enum
            if not replaced:
                for enum_name in ["LineType"]:
                    original_enum = extract_enum_from_block(original_source, enum_name)
                    edited_enum = extract_enum_from_block(code, enum_name)
                    if original_enum and edited_enum:
                        edited_source = edited_source.replace(original_enum, edited_enum)
                        replaced = True
                        break

            # Try to find and replace a struct
            if not replaced:
                for struct_name in ["MaxwellGate"]:
                    original_struct = extract_struct_from_block(original_source, struct_name)
                    edited_struct = extract_struct_from_block(code, struct_name)
                    if original_struct and edited_struct:
                        edited_source = edited_source.replace(original_struct, edited_struct)
                        replaced = True
                        break

            # If no specific replacement found, check if the block is a full file
            # (model might have output the entire file)
            if not replaced:
                # Check if the code block looks like a full source file
                if code.count('\n') > 50 and ('use ' in code or 'pub fn' in code or 'pub enum' in code):
                    # It's a full file replacement
                    edited_source = code

            with open(target_file_abs, "w") as f:
                f.write(edited_source)
            files_modified.add(target_file)

        if not files_modified:
            return 0, 0, 0, "no files were modified"

        # 4. Write the test file
        test_dir = os.path.join(worktree_path, "crates/reliary-sift/tests")
        os.makedirs(test_dir, exist_ok=True)
        test_file = os.path.join(test_dir, f"{task['test_name']}.rs")
        with open(test_file, "w") as f:
            f.write(task["test_code"])

        # 5. Run cargo check
        check_result = subprocess.run(
            ["cargo", "check", "-p", "reliary-sift"],
            cwd=worktree_path,
            capture_output=True, text=True, timeout=120
        )
        syntactic = 1 if check_result.returncode == 0 else 0

        # 6. Run the test
        test_result = subprocess.run(
            ["cargo", "test", "-p", "reliary-sift", "--test", task["test_name"]],
            cwd=worktree_path,
            capture_output=True, text=True, timeout=120
        )
        behavioral = 1 if test_result.returncode == 0 else 0

        # 7. Structural: did we modify the right file?
        structural = 1 if target_file_rel in files_modified else 0

        details = {
            "check_stderr": check_result.stderr[-500:],
            "test_stdout": test_result.stdout[-500:],
            "test_stderr": test_result.stderr[-500:],
            "files_modified": list(files_modified),
        }

        return structural, syntactic, behavioral, details

    except Exception as e:
        return 0, 0, 0, f"exception: {e}"
    finally:
        try:
            subprocess.run(["git", "worktree", "remove", "--force", worktree_path],
                          cwd=repo_path, capture_output=True, timeout=10)
        except: pass
        shutil.rmtree(tmpdir, ignore_errors=True)

# ─── Keyword scoring ───────────────────────────────────────────────

def get_function_source(repo_path, target_file, target_func):
    """Read the actual source of the target function from the repo."""
    file_path = os.path.join(repo_path, target_file)
    if not os.path.exists(file_path):
        return ""
    with open(file_path, "r") as f:
        source = f.read()
    func = extract_function_from_block(source, target_func)
    return func or ""

def get_file_source(repo_path, target_file):
    """Read the entire target file."""
    file_path = os.path.join(repo_path, target_file)
    if not os.path.exists(file_path):
        return ""
    with open(file_path, "r") as f:
        return f.read()

def score_keywords(text, expected_keywords):
    """Score keyword coverage (0-3)."""
    text_lower = text.lower()
    found = sum(1 for kw in expected_keywords if kw.lower() in text_lower)
    frac = found / len(expected_keywords) if expected_keywords else 0
    if frac >= 0.7: return 3
    if frac >= 0.4: return 2
    if frac >= 0.15: return 1
    return 0
    """Score keyword coverage (0-3)."""
    text_lower = text.lower()
    found = sum(1 for kw in expected_keywords if kw.lower() in text_lower)
    frac = found / len(expected_keywords) if expected_keywords else 0
    if frac >= 0.7: return 3
    if frac >= 0.4: return 2
    if frac >= 0.15: return 1
    return 0

# ─── Main ──────────────────────────────────────────────────────────

def main():
    import argparse
    parser = argparse.ArgumentParser()
    parser.add_argument("--tasks", default="bench/edit_tasks.json")
    parser.add_argument("--conditions", default="A,F,E")
    parser.add_argument("--output", default="bench/results/edit_bench_results.jsonl")
    parser.add_argument("--max-tasks", type=int, default=10)
    parser.add_argument("--prompt-mode", default="e5", choices=["original", "e5"],
                       help="original=simple prompt, e5=checklist prompt (enumerate sites + trace behavior)")
    args = parser.parse_args()

    with open(args.tasks) as f:
        tasks = json.load(f)[:args.max_tasks]

    conditions = args.conditions.split(",")
    full_pack = build_full_pack() if "F" in conditions else ""

    results = []
    for cond in conditions:
        for task in tasks:
            print(f"  [{cond}] {task['id']}...", end=" ", flush=True)
            t0 = time.time()

            # Build prompt based on condition and prompt mode
            func_source = get_function_source(RELIARY_REPO, task["target_file"], task["target_function"])

            if args.prompt_mode == "e5":
                base_instruction = (
                    "You are a Rust code editor. Before writing code, complete this checklist:\n\n"
                    "1. LOCATIONS: List every file and function that must change for this task. "
                    "Include type definitions (enums, structs), callers, and match sites.\n"
                    "2. BEHAVIOR: Trace what the code does for the expected test cases. "
                    "Write the expected input → output BEFORE writing code.\n"
                    "3. CODE: Produce the FULL edited code for EACH location in separate ```rust blocks. "
                    "If editing multiple files, annotate each block with `// File: path/to/file.rs` as the first line. "
                    "Keep the same function signatures unless the task explicitly renames.\n"
                )
            else:
                base_instruction = (
                    "You are a Rust code editor. Given the current source of a function and an editing task, "
                    "produce the FULL edited function in a ```rust code block. "
                    "Do NOT show diffs — show the complete function with your edits applied. "
                    "Keep the same function signature. Explain your reasoning briefly before the code."
                )

            if cond == "A":
                sys_prompt = base_instruction
                user_msg = (f"The codebase is at {RELIARY_REPO} (Rust workspace).\n"
                           f"File: {task['target_file']}\n"
                           f"Current source of `{task['target_function']}()`:\n```rust\n{func_source}\n```\n\n"
                           f"Task: {task['task']}")
            elif cond == "F":
                sliced = slice_pack(full_pack, task['task'])
                sys_prompt = f"Here is codebase context:\n\n{sliced}\n\n{base_instruction}"
                user_msg = (f"File: {task['target_file']}\n"
                           f"Current source of `{task['target_function']}()`:\n```rust\n{func_source}\n```\n\n"
                           f"Task: {task['task']}")
            elif cond == "E":
                sys_prompt = ("You are a Rust code editor. Use the available codebase tools to understand the code. "
                              "Then complete the checklist: list all locations that must change, trace expected behavior, "
                              "and produce the FULL edited code for EACH location in separate ```rust blocks. "
                              "If editing multiple files, annotate each block with `// File: path/to/file.rs`. "
                              "Keep the same function signatures unless the task explicitly renames.")
                user_msg = (f"The codebase is at {RELIARY_REPO} (Rust workspace).\n"
                           f"File: {task['target_file']}\n"
                           f"Current source of `{task['target_function']}()`:\n```rust\n{func_source}\n```\n\n"
                           f"Task: {task['task']}")
            else:
                continue

            text, usage, wall = call_deepseek(sys_prompt, user_msg, timeout=90)

            # Score
            structural, syntactic, behavioral, details = apply_edit_and_test(task, text, RELIARY_REPO)
            keyword = score_keywords(text, task["expected_keywords"])
            total = structural + syntactic + behavioral + keyword

            result = {
                "cond": cond,
                "task_id": task["id"],
                "difficulty": task["difficulty"],
                "structural": structural,
                "syntactic": syntactic,
                "behavioral": behavioral,
                "keyword": keyword,
                "total": total,
                "wall": wall,
                "tokens_in": usage.get("prompt_tokens", 0),
                "tokens_out": usage.get("completion_tokens", 0),
                "cached_tokens": usage.get("prompt_cache_hit_tokens", 0),
                "answer_preview": text[:300],
                "details": str(details)[:300],
            }
            results.append(result)
            print(f"total={total}/6 (s={structural} c={syntactic} b={behavioral} k={keyword}) {wall:.1f}s")

    # Write results
    os.makedirs(os.path.dirname(args.output), exist_ok=True)
    with open(args.output, "w") as f:
        for r in results:
            f.write(json.dumps(r) + "\n")

    # Summary
    print("\n" + "=" * 80)
    print(f"{'Cond':<6} {'Total':>8} {'Struct':>8} {'Synt':>8} {'Behav':>8} {'Keywd':>8} {'Wall':>8}")
    print("-" * 80)
    for cond in conditions:
        cond_results = [r for r in results if r["cond"] == cond]
        if not cond_results: continue
        avg = lambda k: sum(r[k] for r in cond_results) / len(cond_results)
        print(f"{cond:<6} {avg('total'):>8.1f} {avg('structural'):>8.2f} {avg('syntactic'):>8.2f} {avg('behavioral'):>8.2f} {avg('keyword'):>8.1f} {avg('wall'):>8.1f}")

if __name__ == "__main__":
    main()