"""Arc 28 Lever 6 — Task suite: altbackend-mcp vs reliary8 comparison.

Each task has:
- id: unique identifier
- question: the question posed to the LLM
- ground_truth: list of file:line references (the correct answer)
- category: type of question (find_references, call_graph, search, etc.)

Ground truth for find_references comes from homonyms.json (50 anchors).
Other questions have manually-curated ground truth.
"""
import json
import os
import sys
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from llm_conn import mcp_call, TOKIO_CORPUS, HOMONYMS_FIXTURE


def build_find_references_tasks():
    """Tasks with oracle ground truth from homonyms fixture + reliary_find_references."""
    with open(HOMONYMS_FIXTURE) as f:
        data = json.load(f)
    anchors = [a for a in data["anchors"]
               if a.get("audit_status") != "unbenchable"]
    tasks = []
    for a in anchors:
        rel = a["anchor_file"]
        if rel.startswith(TOKIO_CORPUS):
            rel = rel[len(TOKIO_CORPUS):].lstrip("/")
        try:
            r = mcp_call("reliary_find_references_type_flow",
                          {"name": a["stem"], "anchor_file": rel,
                           "anchor_line": a["anchor_line"], "path": TOKIO_CORPUS,
                           "threshold": 0.5},
                          workdir=TOKIO_CORPUS, timeout=60)
            gt = [f"{h['file']}:{h['line']}" for h in r.get("hits", [])]
        except Exception:
            gt = []
        if 5 <= len(gt) <= 50:
            tasks.append({
                "id": a["id"],
                "category": "find_references",
                "question": (
                    f"Find references to `{a['stem']}` matching the same class as the "
                    f"definition at `{a['anchor_file']}:{a['anchor_line']}` "
                    f"(label={a['use_label']}). "
                    f"Return a single JSON object on one line, no prose: "
                    f'{{"references": [{{"file": "<relative path>", "line": <int>}}, ...]}}'
                ),
                "ground_truth": gt,
                "anchor": a,
            })
    return tasks


def build_call_graph_tasks():
    """Manually-curated call-graph tasks. Use altbackend-mcp trace_path
    to get ground truth, then have LLM discover same."""
    import subprocess
    tasks = []
    # Task 1: who calls spawn?
    try:
        r = subprocess.run(["/home/user/.local/bin/altbackend-mcp", "cli",
                             "trace_path",
                             '{"function_name": "spawn", "project": "tmp-tokio-corpus-tokio-src", "direction": "inbound", "depth": 2}'],
                            capture_output=True, text=True, timeout=30)
        data = json.loads(r.stdout)
        # trace_path returns {"callers": [...]} with each caller having qn (qualified name)
        callers = []
        for c in data.get("callers", []):
            qn = c.get("qualified_name", "")
            # qn format: project.path.to.file.name
            parts = qn.split(".")
            # Find the file_path
            if len(parts) >= 2:
                file_part = ".".join(parts[1:-1])
                callers.append(file_part + ".rs")
        callers = sorted(set(callers))[:15]
        if callers:
            tasks.append({
                "id": "call-spawn",
                "category": "call_graph",
                "question": (
                    f"Who calls the function `spawn` in {TOKIO_CORPUS}? "
                    f"Return a single JSON object on one line, no prose: "
                    f'{{"callers": [{{"file": "<path>", "line": <int>}}, ...]}}'
                ),
                "ground_truth": callers,
            })
    except Exception as e:
        print(f"WARN: call-spawn failed: {e}")
    return tasks


def build_search_tasks():
    """Tasks where ground truth is the file list."""
    tasks = []
    # Task: Find files containing `Pin::new_unchecked`
    try:
        import subprocess
        r = subprocess.run(["grep", "-rln", r"\bPin::new_unchecked\b", TOKIO_CORPUS,
                             "--include=*.rs"],
                            capture_output=True, text=True, timeout=15)
        files = []
        for line in r.stdout.strip().split("\n")[:20]:
            if line.startswith(TOKIO_CORPUS):
                files.append(line[len(TOKIO_CORPUS):].lstrip("/"))
        if files:
            tasks.append({
                "id": "search-pin-new-unchecked",
                "category": "search",
                "question": (
                    f"Find files in {TOKIO_CORPUS} that use `Pin::new_unchecked`. "
                    f"Return a single JSON object on one line, no prose: "
                    f'{{"files": ["<path>", ...]}}'
                ),
                "ground_truth": files,
            })
    except Exception as e:
        print(f"WARN: search-pin failed: {e}")
    return tasks


def build_dead_code_tasks():
    """Find functions with no inbound CALLS edges."""
    tasks = []
    try:
        import subprocess
        r = subprocess.run(["/home/user/.local/bin/altbackend-mcp", "cli",
                             "query_graph",
                             '{"query": "MATCH (f:Function) WHERE NOT EXISTS { (f)<-[:CALLS]-() } AND NOT f.is_entry_point RETURN f.name, f.file_path, f.start_line LIMIT 30", "project": "tmp-tokio-corpus-tokio-src"}'],
                            capture_output=True, text=True, timeout=30)
        data = json.loads(r.stdout)
        dead = []
        for row in data.get("rows", []):
            name, fp, line = row[0], row[1], row[2]
            if fp:
                if fp.startswith(TOKIO_CORPUS):
                    fp = fp[len(TOKIO_CORPUS):].lstrip("/")
                dead.append(f"{fp}:{line}")
        if dead:
            tasks.append({
                "id": "dead-code",
                "category": "dead_code",
                "question": (
                    f"Find functions in {TOKIO_CORPUS} that have NO callers (dead code). "
                    f"Return ONLY a JSON line: "
                    f'{{"dead": [{{"file": "<path>", "line": <int>}}, ...]}}'
                ),
                "ground_truth": dead,
            })
    except Exception as e:
        print(f"WARN: dead-code failed: {e}")
    return tasks


def build_all_tasks():
    tasks = []
    tasks.extend(build_find_references_tasks()[:10])  # 10 find-references
    tasks.extend(build_call_graph_tasks()[:3])        # 3 call-graph
    tasks.extend(build_search_tasks()[:3])            # 3 search
    tasks.extend(build_dead_code_tasks()[:2])         # 2 dead-code
    return tasks


if __name__ == "__main__":
    tasks = build_all_tasks()
    print(f"Built {len(tasks)} tasks:")
    by_cat = {}
    for t in tasks:
        by_cat.setdefault(t["category"], []).append(t["id"])
    for cat, ids in by_cat.items():
        print(f"  {cat}: {len(ids)} — {', '.join(ids[:5])}")
    # Show first task structure
    if tasks:
        print("\nFirst task example:")
        print(json.dumps(tasks[0], indent=2)[:500])