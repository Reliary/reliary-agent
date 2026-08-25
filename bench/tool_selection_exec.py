"""Execute the tools the LLM picked and measure jaccard."""
import json
import subprocess
import sys
import time
from pathlib import Path

sys.path.insert(0, "/home/user/src/reliary8/bench")
from tool_selection import (get_reliary_tools, get_altbackend_tools, get_grep_tools,
                              call_reliary_tool, call_altbackend_tool,
                              strict_grep_oracle, parse_llm_json, TOKIO_CORPUS,
                              deepseek_chat)
import re

ALTBACKEND_BIN = "/home/user/.local/bin/altbackend-mcp"


def extract_refs_from_reliary(result):
    """Extract file:line refs from reliary_find_references_with_source output."""
    if not isinstance(result, dict):
        return []
    hits = result.get("hits", [])
    out = []
    for h in hits:
        f = h.get("file", "")
        if f.startswith(TOKIO_CORPUS):
            f = f[len(TOKIO_CORPUS):].lstrip("/")
        out.append(f"{f}:{h.get('line', 0)}")
    return out


def extract_refs_from_altbackend(result):
    """Extract file:line refs from altbackend search_code output."""
    if not isinstance(result, dict):
        return []
    results = result.get("results", [])
    out = []
    for r in results:
        f = r.get("file_path", "")
        if f.startswith(TOKIO_CORPUS):
            f = f[len(TOKIO_CORPUS):].lstrip("/")
        out.append(f"{f}:{r.get('start_line', 0)}")
    return out


def jaccard(pred, gt):
    p, g = set(pred), set(gt)
    if not (p | g):
        return 0.0
    return len(p & g) / len(p | g)


def exec_bash(command):
    """Execute bash command and parse for file:line output."""
    try:
        r = subprocess.run(command, shell=True, capture_output=True,
                            text=True, timeout=30)
    except Exception:
        return []
    refs = []
    for line in r.stdout.split("\n"):
        parts = line.split(":", 2)
        if len(parts) >= 3:
            try:
                ln = int(parts[1])
                f = parts[0]
                if f.startswith(TOKIO_CORPUS):
                    f = f[len(TOKIO_CORPUS):].lstrip("/")
                refs.append(f"{f}:{ln}")
            except ValueError:
                pass
    # Filter
    out = []
    for r in refs:
        if "/tests/" in r or "_test.rs" in r:
            continue
        # crude doc-comment filter: skip if line ends with comment indicator
        out.append(r)
    return out


def main():
    # Re-run the tool selection
    questions = [
        {"id": "hom-009", "stem": "consume",
         "question": f"Find all references to the method `consume` defined at `io/util/take.rs:121` in {TOKIO_CORPUS}. The anchor is a method_call. Return file:line references."},
        {"id": "hom-014", "stem": "split",
         "question": f"Find all references to the method `split` in {TOKIO_CORPUS}. Return file:line."},
        {"id": "hom-015", "stem": "kill",
         "question": f"Find all references to the method `kill` in {TOKIO_CORPUS}. Return file:line."},
        {"id": "hom-007", "stem": "send",
         "question": f"Find all references to the method `send` in {TOKIO_CORPUS}. Return file:line."},
        {"id": "hom-026", "stem": "consume",
         "question": f"Find references to the `consume` method at `io/util/buf_stream.rs:194`. Return file:line."},
    ]
    for q in questions:
        q["ground_truth"] = strict_grep_oracle(q["stem"])

    reliary_tools = get_reliary_tools()
    altbackend_tools = get_altbackend_tools()
    reliary_desc = "\n".join(f"- {t['name']}: {t['description'][:200]}"
                              for t in reliary_tools)
    altbackend_desc = "\n".join(f"- {t['name']}: {t['description'][:200]}"
                          for t in altbackend_tools)
    grep_desc = "- bash: Run shell commands. Use grep -rEn for text search."

    out_path = Path("/home/user/src/reliary8/bench/results/tool_selection_exec.jsonl")
    out_f = open(out_path, "w")

    for q in questions:
        gt = q["ground_truth"]
        for cond_name, tools_desc, call_fn in [
            ("A", reliary_desc, "reliary"),
            ("B", altbackend_desc, "altbackend"),
            ("C", grep_desc, "bash"),
        ]:
            # Step 1: LLM picks tool
            sys_msg = (
                f"You are a code analyst. You have these tools:\n\n{tools_desc}\n\n"
                f"Pick the BEST tool for the question. Output ONLY a JSON line:\n"
                f'{{"tool": "<tool_name>", "arguments": {{...}}}}'
            )
            user_msg = f"Question: {q['question']}\n\nOutput ONLY the JSON line."
            t0 = time.time()
            resp = deepseek_chat(
                [{"role": "system", "content": sys_msg},
                 {"role": "user", "content": user_msg}],
                model="deepseek-chat", max_tokens=300, timeout=30)
            pick_elapsed = time.time() - t0
            content = resp["choices"][0]["message"].get("content") or ""
            usage = resp.get("usage", {})
            pt_pick = usage.get("prompt_tokens", 0)
            ct_pick = usage.get("completion_tokens", 0)

            parsed = parse_llm_json(content)
            tool = parsed.get("tool") if parsed else None
            args = parsed.get("arguments", {}) if parsed else {}

            # Step 2: Execute
            t1 = time.time()
            preds = []
            if call_fn == "reliary" and tool == "reliary_find_references_with_source":
                # Map LLM's args to our schema
                rargs = {
                    "name": args.get("symbol") or args.get("name"),
                    "anchor_file": (args.get("anchor_file") or "io/util/take.rs")
                                   .replace(TOKIO_CORPUS + "/", "").replace(TOKIO_CORPUS, ""),
                    "anchor_line": int(args.get("anchor_line", 121)),
                    "path": ".",
                    "threshold": 0.1,
                    "context": 0,
                }
                result = call_reliary_tool(tool, rargs)
                preds = extract_refs_from_reliary(result)
            elif call_fn == "altbackend" and tool == "search_code":
                cargs = {"project": "tmp-tokio-corpus-tokio-src",
                         "pattern": args.get("query") or args.get("pattern") or q["stem"],
                         "limit": 50}
                result = call_altbackend_tool(tool, cargs)
                preds = extract_refs_from_altbackend(result)
            elif call_fn == "bash" and tool == "bash":
                cmd = args.get("command") or f"grep -rEn '\\b{q['stem']}\\b' {TOKIO_CORPUS} --include=*.rs"
                preds = exec_bash(cmd)
            exec_elapsed = time.time() - t1

            jac = jaccard(preds, gt)
            total_wc = pt_pick + 4*ct_pick
            print(f"  {q['id']} cond={cond_name}: tool={tool}, jaccard={jac:.3f} "
                  f"preds={len(preds)} wc={total_wc} pick={pick_elapsed:.1f}s "
                  f"exec={exec_elapsed:.1f}s")
            out_f.write(json.dumps({
                "task_id": q["id"], "condition": cond_name,
                "tool_picked": tool, "tool_args": args,
                "predictions": preds, "jaccard": jac,
                "gt_size": len(gt),
                "tokens_in": pt_pick, "tokens_out": ct_pick,
                "weighted_cost": total_wc,
                "pick_elapsed": pick_elapsed, "exec_elapsed": exec_elapsed,
            }) + "\n")
            out_f.flush()
    out_f.close()
    print(f"\nDone. Output: {out_path}")


if __name__ == "__main__":
    main()