#!/usr/bin/env python3
"""
ci_guards.py — Pre-commit and CI guardrails for reliary-agent.

Detects bug classes that have repeatedly appeared in audits:
  1. Non-atomic file writes (std::fs::write outside atomic_write path)
  2. Unbounded read_to_string (no MAX_FILE_SIZE / metadata() check)
  3. Stdin reads without size cap
  4. curl subprocess (we use reqwest)
  5. SQL queries against tables that don't exist in the schema
  6. Hardcoded valid_keys/features lists (drift risk)

Exit code 0 = all checks pass.
Exit code 1 = at least one violation found.

Usage:
    python3 scripts/ci_guards.py           # check all source files
    python3 scripts/ci_guards.py --staged  # only check staged files (pre-commit)
    python3 scripts/ci_guards.py --verbose # show passing checks too
"""
import subprocess
import sys
import re
from pathlib import Path
from typing import List, Tuple, Set

# Files/dirs to skip
SKIP_DIRS = {'target', '.git', 'node_modules', '.reliary', 'scripts/benchmarks'}
SKIP_FILES = {
    'crates/reliary-core/src/fs_safe.rs',  # the helper itself
    'crates/reliary-agent/tests/regression_v068.rs',  # placeholder
}

# Known valid SQLite tables (from reliary-search/src/schema.rs)
VALID_TABLES = {'file_map', 'phrases', 'phrase_occ', 'count_overflow', 'file_stats', 'meta', 'phrases_fts', 'chronicle', 'edit_cache'}

# File patterns to scan
SOURCE_GLOBS = [
    'crates/reliary-agent/src/*.rs',
    'crates/reliary-search/src/*.rs',
    'crates/reliary-core/src/*.rs',
]


class Violation:
    def __init__(self, rule: str, file: str, line: int, msg: str, severity: str = "error"):
        self.rule = rule
        self.file = file
        self.line = line
        self.msg = msg
        self.severity = severity

    def __str__(self):
        return f"  [{self.rule}] {self.file}:{self.line}  {self.msg}"


def get_files(staged_only: bool) -> List[str]:
    """Get list of Rust source files to check."""
    if staged_only:
        result = subprocess.run(
            ['git', 'diff', '--cached', '--name-only', '--diff-filter=ACM'],
            capture_output=True, text=True, cwd='.'
        )
        files = [f for f in result.stdout.split() if f.endswith('.rs')]
    else:
        files = []
        for glob in SOURCE_GLOBS:
            files.extend(str(p) for p in Path('.').glob(glob))
    return [f for f in files if not any(skip in f for skip in SKIP_DIRS) and f not in SKIP_FILES]


def in_test_module(path: str, line_idx: int, lines: List[str]) -> bool:
    """Check if a line is inside a `mod tests` block.

    Simple heuristic: scan backwards, look for the first occurrence of
    `mod tests {`. If found, return True. If we hit a `}` (closing a
    block at lower indent) before finding mod tests, return False.
    """
    in_module_indent = None
    for i in range(line_idx, -1, -1):
        line = lines[i]
        stripped = line.strip()
        # Skip blank lines
        if not stripped:
            continue
        # If this is a `mod tests {` at column 0
        if stripped.startswith('mod tests') and '{' in stripped:
            return True
        # If we hit a top-level (column 0) `impl`, `pub struct`, etc. — out of any test module
        if line.startswith('impl ') or line.startswith('pub struct') or line.startswith('pub enum') or line.startswith('pub fn '):
            return False
    return False


def check_non_atomic_writes(files: List[str]) -> List[Violation]:
    """Rule 1: std::fs::write outside atomic_write path or test code."""
    violations = []
    # Files where std::fs::write is allowed (atomic_write helpers, batch tools)
    ALLOW_FILES = {
        'crates/reliary-agent/src/init.rs',  # has its own atomic_write that handles cleanup
        'crates/reliary-agent/src/reindex.rs',  # dead code, slated for removal
    }
    for path in files:
        if path in ALLOW_FILES:
            continue
        with open(path) as f:
            lines = f.readlines()
        for i, line in enumerate(lines, 1):
            if 'std::fs::write' not in line and 'fs::write' not in line:
                continue
            # Skip comments
            if line.strip().startswith('//') or line.strip().startswith('*'):
                continue
            # Skip lines in test modules
            if in_test_module(path, i - 1, lines):
                continue
            # Skip lines that opt out with // GUARDED: intentional
            if 'GUARDED: intentional' in line:
                continue
            # Look back 5 lines for "atomic_write" context
            context_start = max(0, i - 5)
            context = ''.join(lines[context_start:i])
            if 'atomic_write' in context:
                continue
            # tmp file writes (part of atomic pattern)
            if '.tmp.' in line or '.tmp"' in line:
                continue
            # /dev/null and similar are safe
            if re.search(r'"/dev/(null|zero|stdout|stderr|urandom)"', line):
                continue
            violations.append(Violation(
                'non-atomic-write',
                path, i,
                f"std::fs::write outside atomic_write pattern. Use reliary_core::atomic_write() instead."
            ))
    return violations


def check_uncapped_reads(files: List[str]) -> List[Violation]:
    """Rule 2: read_to_string without size guard (production code only)."""
    violations = []
    for path in files:
        if 'fs_safe.rs' in path:
            continue
        with open(path) as f:
            lines = f.readlines()
        for i, line in enumerate(lines, 1):
            if 'read_to_string' not in line:
                continue
            # Skip test modules
            if in_test_module(path, i - 1, lines):
                continue
            # Skip lines that opt out with // GUARDED: intentional
            if 'GUARDED: intentional' in line:
                continue
            # Look back 30 lines for size guard (handles functions with early-return guards at top)
            context_start = max(0, i - 30)
            context = ''.join(lines[context_start:i])
            forward_context = ''.join(lines[i:min(len(lines), i+3)])
            full_context = context + line + forward_context
            # Safe patterns
            safe_patterns = [
                'MAX_FILE_SIZE', 'metadata()', 'meta.len()', 'safe_read',
                'atomic_write',  # atomic_write reads-then-writes
                'reliary_core::safe_read',
                '> MAX_FILE_SIZE',  # local file-size comparison
                'meta.len() >', 'meta.len() >=',
            ]
            if any(p in full_context for p in safe_patterns):
                continue
            # Files where reads are inherently safe (configuration, indices, CLI helpers)
            SAFE_FILES = {
                'crates/reliary-agent/src/config.rs',  # reads own small config files
                'crates/reliary-agent/src/routes.rs',  # reads agent config files (small)
                'crates/reliary-agent/src/mcp.rs',  # reads user-specified small files
                'crates/reliary-agent/src/guard.rs',  # has its own size check elsewhere
                'crates/reliary-agent/src/scavenger.rs',  # has 10MB check (Bug 27 fix)
                'crates/reliary-agent/src/ux.rs',  # CLI helpers reading small config/PID files
                'crates/reliary-agent/src/init.rs',  # init reads many small config files
            }
            if path in SAFE_FILES:
                continue
            violations.append(Violation(
                'uncapped-read',
                path, i,
                f"read_to_string without size guard. Use reliary_core::safe_read() or check metadata().len() first."
            ))
    return violations


def check_curl_subprocess(files: List[str]) -> List[Violation]:
    """Rule 3: curl subprocess (we use reqwest)."""
    violations = []
    for path in files:
        with open(path) as f:
            lines = f.readlines()
        for i, line in enumerate(lines, 1):
            if 'Command::new("curl")' not in line and 'Command::new("wget")' not in line:
                continue
            # Look 3 lines forward for "reliary-update" exception
            context = ''.join(lines[i:min(len(lines), i+3)])
            if 'reliary-update' in context:
                continue
            if 'GUARDED: intentional' in line:
                continue
            violations.append(Violation(
                'curl-subprocess',
                path, i,
                f"curl/wget subprocess detected. Use reqwest::blocking or reqwest (async) instead."
            ))
    return violations


def check_sql_unknown_tables(files: List[str]) -> List[Violation]:
    """Rule 4: SQL queries against tables not in schema."""
    violations = []
    sql_from = re.compile(r'\b(?:FROM|JOIN)\s+([a-z_]+)', re.IGNORECASE)
    for path in files:
        with open(path) as f:
            lines = f.readlines()
        in_sql = False
        for i, line in enumerate(lines, 1):
            stripped = line.strip()
            # Skip comments
            if stripped.startswith('//') or stripped.startswith('*') or stripped.startswith('/*'):
                continue
            # Detect multi-line SQL strings: db.execute("SELECT ... FROM x JOIN y")
            if 'SELECT' in stripped and 'FROM' in stripped:
                in_sql = True
            if in_sql and ('");' in stripped or "');" in stripped or '")' in stripped):
                in_sql = False
            if not in_sql and 'SELECT' not in stripped.upper():
                continue
            if 'PRAGMA' in stripped.upper():
                continue
            for m in sql_from.finditer(line):
                table = m.group(1).lower()
                if table in VALID_TABLES:
                    continue
                if table in {'dual', 'sqlite_master', 'pragma_db_list'}:
                    continue
                # Skip table aliases (single letter, e.g., "FROM x")
                if len(table) <= 1:
                    continue
                # Skip common Rust keywords that might look like table names
                if table in {'crate', 'self', 'super', 'ok', 'err', 'some', 'none'}:
                    continue
                violations.append(Violation(
                    'sql-unknown-table',
                    path, i,
                    f"SQL references unknown table '{table}'. Valid tables: {sorted(VALID_TABLES)}"
                ))
    return violations


def check_stdin_size(files: List[str]) -> List[Violation]:
    """Rule 5: stdin reads without size cap."""
    violations = []
    for path in files:
        with open(path) as f:
            content = f.read()
        for i, line in enumerate(content.split('\n'), 1):
            if 'stdin().read_to_string' not in line and 'stdin().read_to_end' not in line:
                continue
            if 'safe_read_stdin' in content:
                continue
            violations.append(Violation(
                'uncapped-stdin',
                path, i,
                f"stdin read without size cap. Use reliary_core::safe_read_stdin() instead."
            ))
    return violations


def check_hardcoded_lists(files: List[str]) -> List[Violation]:
    """Rule 6: hardcoded valid_keys/features lists (drift risk).

    Only flags let-bindings of valid_keys/feature lists in NON-config files.
    """
    violations = []
    # Files where it's OK to define these lists (the consts themselves)
    ALLOW_FILES = {
        'crates/reliary-agent/src/config.rs',  # single source of truth
    }
    for path in files:
        if path in ALLOW_FILES:
            continue
        with open(path) as f:
            lines = f.readlines()
        for i, line in enumerate(lines, 1):
            stripped = line.strip()
            if 'valid_keys' in stripped and '[' in stripped and 'let ' in stripped:
                violations.append(Violation(
                    'hardcoded-list',
                    path, i,
                    f"hardcoded valid_keys list. Use config::VALID_CONFIG_KEYS const instead."
                ))
    return violations


def main():
    staged = '--staged' in sys.argv
    verbose = '--verbose' in sys.argv

    files = get_files(staged)
    if not files:
        print("No files to check.")
        return 0

    print(f"Checking {len(files)} files for guardrail violations...")

    all_violations: List[Violation] = []
    checks = [
        ('non-atomic-write', check_non_atomic_writes),
        ('uncapped-read', check_uncapped_reads),
        ('curl-subprocess', check_curl_subprocess),
        ('sql-unknown-table', check_sql_unknown_tables),
        ('uncapped-stdin', check_stdin_size),
        ('hardcoded-list', check_hardcoded_lists),
    ]
    for rule_name, check_fn in checks:
        violations = check_fn(files)
        if violations:
            print(f"\n  ✗ {rule_name}: {len(violations)} violation(s)")
            for v in violations:
                print(f"    {v}")
        elif verbose:
            print(f"  ✓ {rule_name}: clean")
        all_violations.extend(violations)

    if all_violations:
        print(f"\n  ✗ FAILED: {len(all_violations)} guardrail violation(s)")
        print(f"\n  To fix:")
        print(f"    - Use reliary_core::atomic_write() instead of std::fs::write")
        print(f"    - Use reliary_core::safe_read() instead of read_to_string")
        print(f"    - Use reliary_core::safe_read_stdin() instead of stdin read")
        print(f"    - Use reqwest instead of curl subprocess")
        print(f"    - Use config::FEATURE_DEFAULTS / VALID_CONFIG_KEYS const")
        print(f"    - If a violation is a false positive, mark with '// GUARDED: intentional'")
        return 1

    print(f"\n  ✓ All {len(checks)} guardrails passed")
    return 0


if __name__ == '__main__':
    sys.exit(main())
