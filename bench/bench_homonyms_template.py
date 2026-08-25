#!/usr/bin/env python3
"""bench_homonyms_template.py — generate the empty template for manual labeling.

Reads fixtures/homonym_candidates.json, picks the top candidates by the combined
homonym score (context_diversity * min(blocks,10)), and emits an empty template
with empty slots for the user to fill in.

Usage:
  python3 bench_homonyms_template.py --top 50 --out fixtures/homonyms_template.json

The user edits the output: for each entry, fill in `anchor_file`, `anchor_line`,
`use_label`, and `notes`. The 8 `use_label` categories are:

  field_access     struct/class field (e.g. self.name)
  method_call      calling a function on a receiver
  local_var        local variable (let/const bound)
  param            function parameter
  module_name      namespace/module reference
  type_name        type/struct/enum usage
  function_def     function definition site
  import_or_use    import/use statement
"""
import argparse
import json
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
FIX = HERE / "fixtures"

USE_LABELS = [
    "field_access",
    "method_call",
    "local_var",
    "param",
    "module_name",
    "type_name",
    "function_def",
    "import_or_use",
]


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--candidates", default=str(FIX / "homonym_candidates.json"))
    ap.add_argument("--top", type=int, default=50)
    ap.add_argument("--out", default=str(FIX / "homonyms_template.json"))
    args = ap.parse_args()

    cands = json.loads(Path(args.candidates).read_text())["candidates"]
    # Rank by combined score: high diversity AND high block count.
    # Cap blocks at 10 so a wildly popular trait method doesn't dominate.
    ranked = sorted(cands, key=lambda c: -(c["context_diversity"] * min(c["def_blocks"], 10)))
    top = ranked[:args.top]

    template = {
        "_instructions": {
            "what_this_is": (
                "Manual labels for the homonym disambiguation bench. "
                "Each entry is an anchor point: a specific (stem, file, line) "
                "triple where the user has identified the semantic role of that "
                "occurrence of the stem."
            ),
            "how_to_label": (
                "For each entry below: (1) confirm the stem is interesting "
                "(same name, different semantic role in different contexts). "
                "(2) Choose an anchor file/line that exemplifies ONE role. "
                "(3) Pick a use_label from the 8 categories. "
                "(4) Add a one-line note explaining the role."
            ),
            "use_labels": USE_LABELS,
            "fill_in": [
                "anchor_file", "anchor_line", "use_label", "notes"
            ],
        },
        "corpus": json.loads(Path(args.candidates).read_text())["corpus"],
        "labeled_at": "",
        "anchors": [
            {
                "id": f"hom-{i+1:03d}",
                "stem": c["stem"],
                "anchor_file": "",      # FILL: which sample_location to anchor at
                "anchor_line": 0,       # FILL
                "use_label": "",        # FILL: one of USE_LABELS
                "notes": "",            # FILL: one-line explanation
                "_candidate_info": {
                    "context_diversity": c["context_diversity"],
                    "def_blocks": c["def_blocks"],
                    "def_files": c["def_files"],
                    "sample_locations": c["sample_locations"],
                }
            }
            for i, c in enumerate(top)
        ],
    }
    Path(args.out).write_text(json.dumps(template, indent=2))
    print(f"wrote template with {len(top)} empty anchor slots to {args.out}")
    print(f"\nusage: open {args.out}, fill in `anchor_file`, `anchor_line`, `use_label`, `notes` for each anchor.")
    print(f"  use_label values: {USE_LABELS}")
    print(f"  when done, save as {FIX / 'homonyms.json'} (NOT _template).")


if __name__ == "__main__":
    main()