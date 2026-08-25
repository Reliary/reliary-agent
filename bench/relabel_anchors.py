#!/usr/bin/env python3
"""Auto-correct anchor use_label based on the line content (not stale labels).

This is Phase C1 of arc38. The previous fixture had labels that didn't match
the anchor's actual line — likely because they described what the tool would
search for, not the line itself. This script re-labels each anchor using the
autolabeler on the anchor's own line.

Honest criteria: the anchor's `use_label` MUST match what its anchor line IS,
not what the tool returns most often for that stem.

Handles attribute lines (`#[track_caller]`, `#[derive(...)]`) and comment
lines by inspecting adjacent lines for the actual definition.
"""
import json
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent

with open(HERE / "fixtures" / "homonyms.json") as f:
    data = json.load(f)

sys.path.insert(0, str(HERE))
from bench_homonyms_autolabel import autolabel


def read_line(path, line):
    try:
        with open(path, errors='ignore') as f:
            for i, raw in enumerate(f, 1):
                if i == line:
                    return raw.rstrip('\n')
    except Exception:
        return ""
    return ""


def read_next(path, line, count=4):
    """Return up to `count` lines AFTER the target line."""
    try:
        with open(path, errors='ignore') as f:
            all_lines = f.readlines()
        end = min(len(all_lines), line + count)
        return [l.rstrip('\n') for l in all_lines[line:end]]
    except Exception:
        return []


def determine_label(p, line):
    """Determine the right label for an anchor, considering attribute lines
    and comments by inspecting adjacent lines."""
    line_text = read_line(p, line).strip()

    # Attribute line: `#[...]` — look at NEXT line for the real definition.
    if line_text.startswith('#['):
        for nl in read_next(p, line, 4):
            nl_strip = nl.strip()
            if nl_strip.startswith('pub ') or nl_strip.startswith('fn ') or nl_strip.startswith('pub fn '):
                if 'fn ' in nl_strip:
                    return 'function_def'
                if 'struct ' in nl_strip or 'enum ' in nl_strip:
                    return 'function_def'
        return 'function_def'

    # Comment line: skip and check next line.
    if line_text.startswith('//') or line_text.startswith('#') or line_text.startswith('/*') or line_text.startswith('*'):
        for nl in read_next(p, line, 4):
            nl_strip = nl.strip()
            if not nl_strip or nl_strip.startswith('//'):
                continue
            if 'fn ' in nl_strip:
                return 'function_def'
            if 'struct ' in nl_strip or 'enum ' in nl_strip:
                return 'function_def'
        return autolabel(p, line)

    return autolabel(p, line)


# Process each anchor
print("=== Re-labeling anchors based on anchor LINE content (with attribute/comment handling) ===\n")
for anchor in data['anchors']:
    aid = anchor.get('id', '?')
    if anchor.get('audit_status') == 'unbenchable':
        print(f"{aid:8s} SKIP unbenchable")
        continue

    p = anchor.get('anchor_file')
    line = anchor.get('anchor_line')
    if not p or not line:
        print(f"{aid:8s} SKIP missing path/line")
        continue

    if not read_line(p, line):
        print(f"{aid:8s} SKIP broken path {p}")
        continue

    new_label = determine_label(p, line)
    old_label = anchor.get('use_label', '')
    anchor_line_text = read_line(p, line)
    changed = "CHANGED" if old_label != new_label else "same"
    print(f"{aid:8s} {changed:8s} old={old_label:15s} new={new_label:15s} | {anchor_line_text[:50]}")

    if old_label != new_label:
        anchor['use_label'] = new_label
        existing_notes = anchor.get('notes', '')
        anchor['notes'] = f"[arc38 corrected: was '{old_label}', now '{new_label}'] {existing_notes}".strip()

out_path = HERE / "fixtures" / "homonyms.json"
with open(out_path, 'w') as f:
    json.dump(data, f, indent=2)
print(f"\nWrote updated fixture to {out_path}")