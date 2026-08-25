#!/usr/bin/env python3
"""Audit script: for each anchor, show its line context and the dominant hit role.

This helps decide if the anchor's use_label is correct, AND it reveals what
role the tool returns most often (which is what the soft-match metric cares about).
"""
import json
import subprocess
import sys
from pathlib import Path
from collections import Counter

HERE = Path(__file__).resolve().parent

with open(HERE / "fixtures" / "homonyms.json") as f:
    data = json.load(f)

sys.path.insert(0, str(HERE))
import bench_homonyms_autolabel as al


def read_line(path, line):
    try:
        with open(path, errors='ignore') as f:
            for i, raw in enumerate(f, 1):
                if i == line:
                    return raw.rstrip('\n')
    except Exception:
        return ""
    return ""


def query_tool(bin_path, stem, anchor_file, anchor_line, threshold=0.0, workdir='/tmp/tokio-corpus/tokio/src'):
    req = json.dumps({
        'jsonrpc': '2.0', 'id': 1, 'method': 'tools/call',
        'params': {'name': 'reliary_find_references_type_flow',
                   'arguments': {'name': stem, 'anchor_file': anchor_file,
                                 'anchor_line': anchor_line - 1,
                                 'threshold': threshold, 'path': '.'}}
    })
    proc = subprocess.Popen(
        [bin_path, 'mcp'], cwd=workdir,
        stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE
    )
    try:
        out, _ = proc.communicate(req.encode(), timeout=30)
        text = json.loads(json.loads(out.decode())['result']['content'][0]['text'])
        return text.get('hits', [])
    except Exception:
        return []


def audit_anchor(bin_path, anchor):
    aid = anchor.get('id', '?')
    stem = anchor.get('stem', '?')
    anchor_file = anchor.get('anchor_file')
    anchor_line = anchor.get('anchor_line')
    use_label = anchor.get('use_label', '?')
    audit_status = anchor.get('audit_status', '')

    if audit_status == 'unbenchable':
        print(f"  {aid:8s} SKIP (unbenchable)")
        return

    anchor_line_text = read_line(anchor_file, anchor_line)
    if not anchor_line_text:
        print(f"  {aid:8s} BROKEN_PATH {anchor_file}")
        return

    print(f"\n--- {aid} ---")
    print(f"  stem: {stem} | label: {use_label}")
    print(f"  anchor: {anchor_file.split('/')[-1]}:{anchor_line}")
    print(f"  anchor line: {anchor_line_text[:80]}")

    hits = query_tool(bin_path, stem, anchor_file, anchor_line)
    if not hits:
        print(f"  HITS: 0")
        return
    print(f"  hits: {len(hits)}")

    labels = Counter()
    for h in hits[:50]:
        labels[al.autolabel(h['file'], h['line'], '/tmp/tokio-corpus/tokio/src')] += 1
    print(f"  top-50 label distribution:")
    for l, n in labels.most_common(3):
        print(f"    {l:15s}: {n}")


if __name__ == "__main__":
    bin_path = sys.argv[1] if len(sys.argv) > 1 else '/home/user/src/reliary8/target/release/reliary'
    print(f"=== Audit all anchors ===\n")

    for anchor in data['anchors']:
        audit_anchor(bin_path, anchor)