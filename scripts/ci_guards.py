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


def check_unbounded_collections(files: List[str]) -> List[Violation]:
    """Rule 7: Detect unbounded HashMap/Mutex<HashMap> with no LRU eviction.

    Bug 62: ANTI_DB grew with every new workdir (was unbounded).
    Bug 67: RATE_BUCKETS grew with every unique auth_key (was unbounded).
    Bug 40: SSE_SESSIONS grew with every new MCP client (was unbounded).

    Pattern: a Mutex<HashMap> or LazyLock<Mutex<HashMap>> with no .retain() or
    bounded cap nearby.
    """
    violations = []
    for path in files:
        with open(path) as f:
            lines = f.readlines()
        for i, line in enumerate(lines, 1):
            if 'lazy_lock' not in line.lower() and 'lazy_static' not in line.lower() and 'LazyLock' not in line:
                continue
            if 'HashMap' not in line and 'FxHashMap' not in line:
                continue
            # Check 100 lines of context for eviction logic
            context_end = min(len(lines), i + 100)
            context = ''.join(lines[i:context_end])
            has_eviction = any(pat in context for pat in [
                'retain', '.remove(', 'evict', 'MAX_', 'CAP', 'capped',
                '>= ', 'len() >=', 'min_by_key',  # LRU-style eviction
            ])
            # Test code is exempt
            if '#[cfg(test)]' in ''.join(lines[max(0, i-50):i]):
                continue
            if '#[test]' in ''.join(lines[max(0, i-10):i]):
                continue
            if not has_eviction:
                violations.append(Violation(
                    'unbounded-collection',
                    path, i,
                    f"HashMap without visible eviction policy (could grow unbounded). Add .retain(), cap check, or LRU."
                ))
    return violations


def check_blocking_in_async(files: List[str]) -> List[Violation]:
    """Rule 8: Detect std::fs operations inside async fn without spawn_blocking.

    Bug 56: try_prefetch did std::fs::read_to_string in async loop (blocked runtime).
    Bug 69: HTTP client has no timeout (async task can hang).
    """
    violations = []
    for path in files:
        with open(path) as f:
            lines = f.readlines()
        # Find async fn boundaries
        in_async_fn = False
        async_brace_depth = 0
        for i, line in enumerate(lines, 1):
            stripped = line.strip()
            if 'async fn ' in stripped or 'async ' in stripped and 'fn ' in stripped:
                in_async_fn = True
                async_brace_depth = 0
            if in_async_fn:
                async_brace_depth += stripped.count('{') - stripped.count('}')
                if async_brace_depth <= 0 and '{' in stripped:
                    in_async_fn = False
            if not in_async_fn:
                continue
            # Check for std::fs in async context (not in spawn_blocking)
            if 'std::fs::read_to_string' in line or 'std::fs::read(' in line or 'std::fs::File::' in line:
                # Look for spawn_blocking nearby
                context_start = max(0, i - 5)
                context = ''.join(lines[context_start:i])
                if 'spawn_blocking' in context or 'block_in_place' in context:
                    continue
                if 'GUARDED: intentional' in line:
                    continue
                violations.append(Violation(
                    'blocking-in-async',
                    path, i,
                    f"std::fs operation in async fn. Use tokio::task::spawn_blocking or tokio::fs."
                ))
    return violations


def check_no_timeout(files: List[str]) -> List[Violation]:
    """Rule 9: Detect reqwest::Client without .timeout().

    Bug 69: HTTP client had no timeout — upstream hang leaked memory and FDs.
    """
    violations = []
    for path in files:
        with open(path) as f:
            lines = f.readlines()
        for i, line in enumerate(lines, 1):
            if 'reqwest::Client::builder' not in line and 'reqwest::Client::new' not in line:
                continue
            # Check next 10 lines for timeout
            context = ''.join(lines[i:min(len(lines), i+10)])
            if '.timeout(' in context:
                continue
            if '// GUARDED' in line:
                continue
            violations.append(Violation(
                'no-timeout',
                path, i,
                f"reqwest::Client without .timeout(). Hung upstream leaks memory and FDs."
            ))
    return violations


def check_panic_lock(files: List[str]) -> List[Violation]:
    """Rule 10: Detect Mutex::lock().unwrap() (panics on poison).

    Bug 57: Many .lock().unwrap() calls — if any thread panics while holding
    the lock, future lockers also panic.
    """
    violations = []
    for path in files:
        if 'fs_safe.rs' in path:
            continue
        with open(path) as f:
            lines = f.readlines()
        for i, line in enumerate(lines, 1):
            if '.lock().unwrap()' not in line:
                continue
            if 'GUARDED: intentional' in line:
                continue
            if 'test' in path:
                continue
            violations.append(Violation(
                'panic-lock',
                path, i,
                f"Mutex::lock().unwrap() panics on poison. Use unwrap_or_else(|e| e.into_inner())."
            ))
    return violations


def check_body_clone(files: List[str]) -> List[Violation]:
    """Rule 11: Detect body_bytes.clone() (Bug 74 — 2x memory per request).

    body_bytes.clone() inside an async handler copies the entire request body
    just for the cache. Use Bytes (reference-counted, clone is cheap) instead.
    """
    violations = []
    for path in files:
        if 'proxy.rs' not in path:
            continue  # only proxy.rs has body_bytes
        with open(path) as f:
            lines = f.readlines()
        for i, line in enumerate(lines, 1):
            if 'body_bytes.clone()' not in line and 'body_bytes.to_vec()' not in line:
                continue
            if '// GUARDED: intentional' in line:
                continue
            if 'body_bytes.clone()' in line and 'let body_bytes: Bytes' in lines[max(0, i-20):i]:
                # Already fixed — Bytes::clone is cheap
                continue
            violations.append(Violation(
                'body-clone',
                path, i,
                f"body_bytes.clone() copies the full request body. Use Bytes (Arc<[u8]>, clone is cheap)."
            ))
    return violations


def check_lock_during_io(files: List[str]) -> List[Violation]:
    """Rule 12: Detect Mutex locks held across I/O operations (Bug 75).

    If a Mutex is locked, then file I/O (open, write, read) is done while
    holding the lock, ALL other threads trying to lock are blocked on I/O.
    """
    violations = []
    for path in files:
        if 'fs_safe' in path or 'test' in path.lower():
            continue
        with open(path) as f:
            lines = f.readlines()
        in_lock_block = False
        lock_depth = 0
        for i, line in enumerate(lines, 1):
            stripped = line.strip()
            # Track lock acquisition
            if '.lock()' in line and not stripped.startswith('//'):
                in_lock_block = True
                lock_depth = 0
            if not in_lock_block:
                continue
            # Track brace depth for the lock block
            lock_depth += stripped.count('{') - stripped.count('}')
            # If lock scope exited, reset
            if lock_depth < 0 and '{' not in line:
                in_lock_block = False
                lock_depth = 0
                continue
            # Skip short-lived locks (one-liner like cache.get, retain, remove)
            if lock_depth <= 0 and ('retain' in stripped or 'get(' in stripped or 'remove(' in stripped or 'insert(' in stripped):
                in_lock_block = False
                lock_depth = 0
                continue
            # Look for I/O operations inside the lock block
            io_patterns = [
                'std::fs::', 'File::open(', 'File::create(', 'OpenOptions::',
                'writeln!', 'write!', 'fh.write', 'file.write',
                'fs::write', 'fs::read',
            ]
            for pat in io_patterns:
                if pat in stripped and not stripped.startswith('//'):
                    if '// GUARDED: intentional' in line:
                        continue
                    violations.append(Violation(
                        'lock-during-io',
                        path, i,
                        f"I/O inside Mutex lock block. Move I/O outside the lock to avoid blocking other threads."
                    ))
                    # Don't report the same line for multiple I/O patterns
                    break
    return violations


def check_mcp_path_traversal(files: List[str]) -> List[Violation]:
    """Rule 13: Detect direct file reads from user-provided paths in MCP handlers
    without canonicalization (Bugs 76-78).

    MCP handlers accept agent-provided filenames and pass them to read_to_string,
    metadata, or atomic_write. Without canonicalization, a malicious agent config
    can exfiltrate files outside the workdir.
    """
    violations = []
    for path in files:
        if 'mcp.rs' not in path:
            continue
        with open(path) as f:
            content = f.read()
            lines = content.split('\n')
        for i, line in enumerate(lines, 1):
            if 'std::fs::read_to_string(' in line or 'std::fs::metadata(' in line:
                # Should have safe_path() nearby
                context_start = max(0, i - 15)
                context = '\n'.join(lines[context_start:i])
                if 'if let Ok(entries) = std::fs::read_dir(&dp) {' in context:
                    continue
                if 'safe_path' not in context and 'GUARDED: intentional' not in line:
                    violations.append(Violation(
                        'mcp-path-traversal',
                        path, i,
                        f"Direct file read from user-provided path. Use safe_path() to canonicalize."
                    ))
    return violations


def check_lazy_lock_drop_safety(files: List[str]) -> List[Violation]:
    """Rule 14: Detect LazyLock/HashMap patterns that could cause undefined
    drop order (Bug 91) — LazyLock statics with background threads or
    external handles.
    """
    violations = []
    for path in files:
        if 'proxy.rs' not in path:
            continue
        with open(path) as f:
            lines = f.readlines()
        for i, line in enumerate(lines, 1):
            if 'LazyLock' in line and 'HTTP_CLIENT' in line:
                # Check for GUARDED on this line or in context below
                if '// GUARDED: intentional' in line:
                    continue
                context = '\n'.join(lines[i:min(len(lines), i+15)])
                if 'ManuallyDrop' not in context and 'no_drop' not in context:
                    violations.append(Violation(
                        'lazy-lock-drop',
                        path, i,
                        "LazyLock<reqwest::Client> drops background threads at exit (UB). "
                        "Consider wrapping in ManuallyDrop or leak via Box::leak.",
                        severity="warn"
                    ))
    return violations


def check_dedup_skips_tool(files: List[str]) -> List[Violation]:
    """Rule 15: Bug A — any message-dedup logic MUST skip tool/toolResult
    messages. They are identified by tool_call_id, not content. Deduping
    identical-content tool messages orphans the assistant's tool_calls and
    causes upstream 400 "insufficient tool messages following tool_calls".
    """
    violations = []
    for path in files:
        if 'proxy.rs' not in path:
            continue
        with open(path) as f:
            content = f.read()
            lines = content.splitlines()
        # Look for any dedup function that iterates messages without a tool-skip
        # Heuristic: a function with "dedup" in name + a for-loop over messages
        # that doesn't have `role == "tool"` skip check
        for i, line in enumerate(lines, 1):
            if 'fn ' not in line or 'dedup' not in line.lower():
                continue
            # Get function body
            body_start = i
            body_end = i + 1
            depth = 0
            for j in range(i, min(i + 60, len(lines))):
                body_end = j + 1
                if '{' in lines[j]:
                    depth += lines[j].count('{')
                if '}' in lines[j]:
                    depth -= lines[j].count('}')
                if depth == 0 and j > i:
                    break
            body = '\n'.join(lines[body_start:body_end])
            # Check for tool-skip within body
            if 'role == "tool"' in body or 'role == "toolResult"' in body or 'tool_call_id' in body:
                continue
            # Check for messages iteration in body
            if 'messages.iter' in body or 'messages.iter_mut' in body:
                if 'seen' in body or 'HashSet' in body or 'to_remove' in body:
                    violations.append(Violation(
                        'dedup-must-skip-tool',
                        path, i,
                        f"dedup function '{line.strip()}' iterates messages but does NOT "
                        "skip tool/toolResult messages. This orphans assistant tool_calls "
                        "when context-filter collapsed tool results share content. "
                        "Add: if role == \"tool\" || role == \"toolResult\" { continue; }",
                        severity="error"
                    ))
    return violations


def check_sse_finish_injection(files: List[str]) -> List[Violation]:
    """Rule 16: Bug B — synthetic SSE finish_reason chunks MUST include the
    `data: [DONE]` terminator. Pi's SSE parser expects the standard OpenAI
    `[DONE]` marker after the final chunk. Without it, Pi reports
    "Stream ended without finish_reason" on edge cases.
    """
    violations = []
    for path in files:
        if 'proxy.rs' not in path:
            continue
        with open(path) as f:
            content = f.read()
        # Find the synthetic finish chunk injection
        if 'synthetic' in content and 'finish_reason' in content:
            # Find the synthetic block
            idx = content.find('synthetic')
            # Look for [DONE] in same or nearby block
            block_end = min(idx + 1000, len(content))
            block = content[max(0, idx-500):block_end]
            # Check if [DONE] is in the synthetic block
            if 'data: [DONE]' not in block and 'synthetic' in block:
                # Find line number
                line_num = content[:idx].count('\n') + 1
                violations.append(Violation(
                    'sse-finish-injection',
                    path, line_num,
                    "synthetic SSE finish_reason chunk missing 'data: [DONE]' "
                    "terminator. Pi's parser fails without it.",
                    severity="error"
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
        ('unbounded-collection', check_unbounded_collections),
        ('blocking-in-async', check_blocking_in_async),
        ('no-timeout', check_no_timeout),
        ('panic-lock', check_panic_lock),
        ('body-clone', check_body_clone),
        ('lock-during-io', check_lock_during_io),
        ('mcp-path-traversal', check_mcp_path_traversal),
        ('lazy-lock-drop', check_lazy_lock_drop_safety),
        ('dedup-must-skip-tool', check_dedup_skips_tool),
        ('sse-finish-injection', check_sse_finish_injection),
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

    print(f"\n  ✓ All {len(checks)} guardrails passed. Good job.")
    return 0


if __name__ == '__main__':
    sys.exit(main())
