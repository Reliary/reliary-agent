#!/usr/bin/env python3
"""V85 warrant falsifier — pre-registered kill test (docs/plans/V85_WARRANT.md).

For each mutated symbol, two variants:
  (a) incomplete rename: the definition is renamed, call sites are not
  (b) complete rename: every site is renamed

Oracles:
  - `cargo check` on variant (a) must report the old name; on (b) must be clean
  - `reliary warrant` must return OPEN (1) on (a) and CLOSED (0) on (b)

Kill criteria (any one kills or narrows the feature):
  - any silent miss on an OPEN-expectation case
  - precision < 0.90 across OPEN verdicts
  - any false positive on a complete rename
  - > 5% silent misses overall

Usage:
  python3 bench/warrant_falsifier.py --smoke      # 6 mutations, fast
  python3 bench/warrant_falsifier.py --full       # 25 mutations
  python3 bench/warrant_falsifier.py --help

No network, no LLM, ~$0.
"""
from __future__ import annotations

import argparse
import json
import os
import re
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path

RELIARY = os.environ.get("RELIARY_BIN", "target/release/reliary")


def sh(args, cwd, check=False):
    r = subprocess.run(args, cwd=cwd, capture_output=True, text=True)
    if check and r.returncode != 0:
        raise RuntimeError(f"{' '.join(args)} failed: {r.stderr[-800:]}")
    return r


def make_crate(root: Path, n_symbols: int) -> list[str]:
    """A 3-module crate; every symbol has 3 call sites across modules."""
    (root / "src").mkdir(parents=True, exist_ok=True)
    (root / "Cargo.toml").write_text(
        '[package]\nname = "warrantprobe"\nversion = "0.1.0"\nedition = "2021"\n'
    )
    a = ["pub fn api_root() -> u32 { 0 }"]
    b = ["pub fn mid_root() -> u32 { crate::api::api_root() }"]
    c = ["pub fn leaf_root() -> u32 { crate::mid::mid_root() }"]
    names = []
    for i in range(n_symbols):
        name = f"sym_{i:02d}"
        names.append(name)
        # Definition in api.rs.
        a.append(f"pub fn {name}(x: u32) -> u32 {{ x + {i} }}")
        # Three call sites spread across modules.
        b.append(f"pub fn {name}_b() -> u32 {{ crate::api::{name}(1) }}")
        c.append(
            f"pub fn {name}_c() -> u32 {{ crate::api::{name}(2) + crate::api::{name}(3) }}"
        )
    (root / "src/api.rs").write_text("\n".join(a) + "\n")
    (root / "src/mid.rs").write_text("\n".join(b) + "\n")
    (root / "src/lib.rs").write_text(
        "pub mod api;\npub mod mid;\n\n"
        + "\n".join(f"pub fn w_{n}() -> u32 {{ crate::mid::{n}_b() }}" for n in names)
        + "\n"
    )
    # Name the crate's actual root module correctly: `src/lib.rs` is the root,
    # so `crate::api` / `crate::mid` resolve.
    (root / "src/main.rs").unlink(missing_ok=True)
    return names


def rename_symbol(root: Path, name: str, new_name: str, complete: bool) -> None:
    """Rename the definition; optionally every call site + helper names."""
    for rel in ("src/api.rs", "src/mid.rs", "src/lib.rs"):
        p = root / rel
        text = p.read_text()
        if rel == "src/api.rs":
            text = text.replace(f"pub fn {name}(", f"pub fn {new_name}(")
        if complete:
            # Rename only the symbol itself: whole-token matches. Helper
            # names such as `sym_00_b` contain the symbol as a prefix but are
            # DIFFERENT symbols (there is no word boundary at the `_`), so
            # they must keep their names — renaming them would be additional
            # undeclared def changes, which the delta accounting correctly
            # reports as open.
            text = re.sub(rf"\b{re.escape(name)}\b", new_name, text)
        p.write_text(text)


def write_contract(root: Path, old: str, new: str) -> None:
    (root / "done.json").write_text(
        json.dumps(
            {
                "version": 1,
                "allow_unseen": ["Cargo.toml", "Cargo.lock", "target/"],
                "conditions": [{"kind": "rename", "from": old, "to": new}],
            }
        )
    )


def cargo_check_err(root: Path, name: str) -> bool:
    """True when `cargo check` reports an error mentioning the old name."""
    r = sh(["cargo", "check", "--quiet", "--message-format=short"], root)
    return r.returncode != 0 and re.search(rf"\b{re.escape(name)}\b", r.stderr) is not None


def cargo_check_clean(root: Path) -> bool:
    r = sh(["cargo", "check", "--quiet", "--message-format=short"], root)
    return r.returncode == 0


def run_warrant(root: Path, reliary: str) -> tuple[int, dict]:
    r = sh([reliary, "warrant", "--base", "HEAD", "--contract", "done.json"], root)
    try:
        cert = json.loads(r.stdout) if r.stdout.strip() else {}
    except json.JSONDecodeError:
        cert = {}
    return r.returncode, cert


def main() -> int:
    ap = argparse.ArgumentParser()
    g = ap.add_mutually_exclusive_group(required=True)
    g.add_argument("--smoke", action="store_true", help="6 mutations")
    g.add_argument("--full", action="store_true", help="25 mutations")
    args = ap.parse_args()

    reliary = str(Path(RELIARY).resolve())
    if not Path(reliary).exists():
        print(f"reliary binary not found: {reliary}", file=sys.stderr)
        return 2

    n = 6 if args.smoke else 25
    base = Path(tempfile.mkdtemp(prefix="warrant-falsifier-", dir=os.environ.get("TMPDIR")))
    try:
        repo = base / "crate"
        names = make_crate(repo, n)
        sh(["git", "init", "-q"], repo)
        sh(["git", "config", "user.email", "t@t"], repo)
        sh(["git", "config", "user.name", "t"], repo)
        sh(["git", "add", "-A"], repo)
        sh(["git", "commit", "-qm", "base"], repo)
        # Warm the build so cargo check is fast per mutation.
        assert cargo_check_clean(repo), "base crate must compile"

        results = []

        # Stem-collision scenario (negative control for the closure rule):
        # `handling` and `handled` are DIFFERENT symbols that both porter-stem
        # to `handl` (verified against the shipped stemmer). The rename is
        # COMPLETE and `handling` legitimately remains. Raw-token closure
        # reports CLOSED (correct); a stem-based closure sees `handl` still
        # present and reports OPEN — a false positive the harness must catch.
        collide = base / "collide"
        (collide / "src").mkdir(parents=True)
        (collide / "Cargo.toml").write_text(
            '[package]\nname = "collide"\nversion = "0.1.0"\nedition = "2021"\n'
        )
        (collide / "src/lib.rs").write_text(
            "pub fn handled() -> u32 { 1 }\n"
            "pub fn handling() -> u32 { 2 }\n"
            "pub fn run() -> u32 { handled() + handling() }\n"
        )
        sh(["git", "init", "-q"], collide)
        sh(["git", "config", "user.email", "t@t"], collide)
        sh(["git", "config", "user.name", "t"], collide)
        sh(["git", "add", "-A"], collide)
        sh(["git", "commit", "-qm", "base"], collide)
        # Complete rename of `handled`; `handling` is a different symbol and
        # legitimately keeps its name.
        (collide / "src/lib.rs").write_text(
            "pub fn dispatched() -> u32 { 1 }\n"
            "pub fn handling() -> u32 { 2 }\n"
            "pub fn run() -> u32 { dispatched() + handling() }\n"
        )
        (collide / "done.json").write_text(json.dumps(
            {
                "version": 1,
                "allow_unseen": ["Cargo.toml"],
                "conditions": [{"kind": "rename", "from": "handled", "to": "dispatched"}],
            }
        ))
        code, cert = run_warrant(collide, reliary)
        compiler_err = cargo_check_err(collide, "handled")
        results.append(
            {
                "symbol": "handled",
                "variant": "stem-collision",
                "warrant": code,
                "compiler": compiler_err,
                "verdict": cert.get("verdict"),
            }
        )

        for name in names:
            new = f"{name}_renamed"
            # (a) incomplete: def only.
            rename_symbol(repo, name, new, complete=False)
            write_contract(repo, name, new)
            code, cert = run_warrant(repo, reliary)
            compiler_err = cargo_check_err(repo, name)
            results.append(
                {
                    "symbol": name,
                    "variant": "incomplete",
                    "warrant": code,
                    "compiler": compiler_err,
                    "verdict": cert.get("verdict"),
                }
            )
            # Restore.
            sh(["git", "checkout", "--", "."], repo)
            (repo / "done.json").unlink(missing_ok=True)

            # (b) complete.
            rename_symbol(repo, name, new, complete=True)
            write_contract(repo, name, new)
            code, cert = run_warrant(repo, reliary)
            compiler_clean = cargo_check_clean(repo)
            results.append(
                {
                    "symbol": name,
                    "variant": "complete",
                    "warrant": code,
                    "compiler": compiler_clean,
                    "verdict": cert.get("verdict"),
                }
            )
            sh(["git", "checkout", "--", "."], repo)
            (repo / "done.json").unlink(missing_ok=True)

        # Score.
        open_expect = [r for r in results if r["variant"] == "incomplete"]
        closed_expect = [
            r for r in results if r["variant"] in ("complete", "stem-collision")
        ]
        tp = sum(1 for r in open_expect if r["warrant"] == 1)
        silent = sum(1 for r in open_expect if r["warrant"] == 0)
        fp = sum(1 for r in closed_expect if r["warrant"] != 0)
        precision = tp / len(open_expect) if open_expect else 0.0
        oracle_agree = sum(
            1
            for r in open_expect
            if r["compiler"] and r["warrant"] == 1
        )
        print(json.dumps(
            {
                "mutations": len(results),
                "open_expected": len(open_expect),
                "closed_expected": len(closed_expect),
                "violation_precision": round(precision, 3),
                "silent_misses": silent,
                "silent_miss_rate": round(silent / len(open_expect), 3) if open_expect else 0,
                "false_positives_on_complete": fp,
                "compiler_oracle_agreement": round(oracle_agree / len(open_expect), 3)
                if open_expect
                else 0,
                "results": results,
            },
            indent=2,
        ))

        stem_case = next(
            (r for r in results if r["variant"] == "stem-collision"), None
        )
        stem_ok = stem_case is not None and stem_case["warrant"] == 0
        kill = (
            silent > 0
            or precision < 0.90
            or fp > 0
            or not stem_ok
        )
        print(f"KILL_CRITERION_{'TRIGGERED' if kill else 'PASSED'}")
        return 1 if kill else 0
    finally:
        shutil.rmtree(base, ignore_errors=True)


if __name__ == "__main__":
    sys.exit(main())
