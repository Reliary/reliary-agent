"""Multi-turn benchmark harness — measures workflow efficiency.

Runs the LLM through structured tasks with different tool sets.
Each task requires tool use (the LLM must explore the codebase).
Measures wall time, tool calls, turns, tokens, task score.

Three conditions:
- A (reliary): reliary_find_references_with_source + reliary_callgraph + reliary_goto_def
- B (altbackend): search_graph + get_code_snippet + trace_path + get_architecture
- C (grep): bash grep -rn + read

Per-condition, per-task metrics (3 seeds):
- wall_time: end-to-end seconds
- tool_calls: total tool invocations
- turns: LLM reasoning cycles (including follow-up questions)
- tokens_in: total prompt tokens
- tokens_out: total completion tokens
- weighted_cost: prompt + 4*completion
- task_score: 0-3 rubric
- first_code_latency: seconds until LLM first sees actual code
- dead_end_calls: calls returning 0 hits
- tool_bytes: cumulative tool output bytes
"""
import argparse
import json
import os
import random
import re
import subprocess
import sys
import threading
import time
from datetime import datetime, timezone, timedelta
from pathlib import Path

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from llm_conn import (deepseek_chat, mcp_call, TOKIO_CORPUS, DEEPSEEK_MODEL,
                       DEEPSEEK_BASE_URL)

SCRIPT_DIR = Path(os.path.dirname(os.path.abspath(__file__)))
RESULTS_DIR = SCRIPT_DIR / "results"
def _expand(p: str) -> str:
    """Expand a literal $HOME prefix (the scrub left unexpanded paths)."""
    return os.path.expandvars(p)
ALTBACKEND_BIN = _expand("$HOME/.local/bin/codebase-memory-mcp")
ALTBACKEND_PROJECT = "tmp-tokio-corpus-tokio-src"
RELIARY_BIN = _expand("$HOME/src/reliary8/target/release/reliary")

MAX_TURNS = 6
SEEDS = [42, 123, 789]

# ============================================================
# Persistent MCP session — one process per condition run
# ============================================================

class MCPSession:
    """Persistent MCP server connection. Spawns once, reuses for all tool calls."""
    def __init__(self, binary, workdir, label="mcp", extra_env=None):
        self.binary = binary
        self.workdir = workdir
        self.label = label
        self.extra_env = extra_env or {}
        self.proc = None
        self._next_id = 1
        self._initialize()

    def _initialize(self):
        env = os.environ.copy()
        env["RELIARY_EAGER_INDEX"] = "1"  # Arc 50: pre-build lazy tables so queries are fast
        # V13: enable centralized sift + freeze for tool output compression
        if os.environ.get("RELIARY_SIFT_BASH", "0") == "1":
            env["RELIARY_SIFT_BASH"] = "1"
        # Apply per-session extra env (e.g., for E/F/G condition routing)
        for k, v in self.extra_env.items():
            env[k] = v
        self.proc = subprocess.Popen(
            [self.binary, "mcp"],
            cwd=self.workdir,
            stdin=subprocess.PIPE, stdout=subprocess.PIPE,
            stderr=subprocess.DEVNULL,
            env=env)
        # MCP initialize handshake
        self._write({"jsonrpc": "2.0", "id": self._next_id,
                      "method": "initialize",
                      "params": {"protocolVersion": "2024-11-05",
                                 "capabilities": {},
                                 "clientInfo": {"name": "bench-harness", "version": "1.0"}}})
        self._next_id += 1
        # Read init response (eager indexing happens here; allow up to 120s for tokio)
        self._read_response(timeout=120)
        # Send initialized notification (no response expected)
        self._write({"jsonrpc": "2.0", "method": "notifications/initialized"})

    def _write(self, msg):
        """Write a JSON-RPC message to stdin."""
        payload = json.dumps(msg) + "\n"
        self.proc.stdin.write(payload.encode())
        self.proc.stdin.flush()

    def _readline_with_timeout(self, timeout=60):
        """Read one line from stdout with a timeout."""
        result = [None]
        def _read():
            try:
                result[0] = self.proc.stdout.readline()
            except Exception:
                result[0] = b""
        t = threading.Thread(target=_read, daemon=True)
        t.start()
        t.join(timeout)
        if result[0] is None:
            raise TimeoutError(f"MCP read timed out after {timeout}s")
        return result[0]

    def _read_response(self, timeout=60):
        """Read one JSON-RPC response, skipping notifications."""
        deadline = time.time() + timeout
        while time.time() < deadline:
            line = self._readline_with_timeout(timeout - (time.time() - (deadline - timeout)))
            if not line:
                raise TimeoutError("MCP closed connection")
            try:
                resp = json.loads(line.decode().strip())
            except Exception:
                continue
            # Skip notifications (no id field)
            if "id" not in resp:
                continue
            if "error" in resp:
                return {"error": resp["error"]}
            return resp.get("result", {})
        raise TimeoutError(f"No response in {timeout}s")

    def call(self, tool_name, arguments, timeout=60):
        """Call an MCP tool and return the result text."""
        msg_id = self._next_id
        self._next_id += 1
        self._write({"jsonrpc": "2.0", "id": msg_id,
                      "method": "tools/call",
                      "params": {"name": tool_name, "arguments": arguments}})
        # Read response — skip any notifications from init
        deadline = time.time() + timeout
        while time.time() < deadline:
            line = self._readline_with_timeout(timeout - (time.time() - (deadline - timeout)))
            if not line:
                return "(MCP closed connection)"
            try:
                resp = json.loads(line.decode().strip())
            except Exception:
                continue
            # Skip notifications
            if "id" not in resp:
                continue
            if resp.get("id") != msg_id:
                # Wrong id — shouldn't happen, but be lenient
                continue
            if "error" in resp:
                return f"(MCP error: {resp['error']})"
            result = resp.get("result", {})
            content = result.get("content", [])
            if content:
                return content[0].get("text", "")
            return ""
        return f"(MCP timeout after {timeout}s)"

    def close(self):
        if self.proc:
            try:
                self.proc.kill()
                self.proc.wait(timeout=2)
            except Exception:
                pass
            self.proc = None


# ============================================================
# Tool registries — each condition registers its tools here
# ============================================================

# Active sessions (created per run_task, closed at end)
_sessions = {}

# Session key for the current condition (set by run_task before tool calls).
# Allows E/F/G to use different MCP sessions with different env vars.

def _reliary_session_key():
    """Return the _sessions dict key for the current condition's reliary MCP."""
    cond = _current_cond.get("cond")
    return {
        "A": "reliary", "D": "reliary",
        "F": "reliary_sift_bash",
    }.get(cond, "reliary")

_current_cond = {"cond": "A"}

def tool_reliary_find_references(args):
    """Reliary type-flow in grep format. Auto-anchored internally."""
    name = args.get("name", "")
    anchor_file = args.get("anchor_file", "")
    anchor_line = args.get("anchor_line", 0)
    call_args = {
        "name": name, "anchor_file": anchor_file, "anchor_line": anchor_line,
        "path": ".", "threshold": 0.05, "format": "grep",
        "limit": args.get("limit", 30),
    }
    # V57: forward reliary-specific params the model may request.
    for k in ("def_only", "usage_only", "methods", "dead_only", "path_filter"):
        if k in args:
            call_args[k] = args[k]
    try:
        text = _sessions[_reliary_session_key()].call("reliary_find_references_with_source", call_args)
        return text if text else "(no references)"
    except Exception as e:
        return f"(find_references error: {e})"


def tool_reliary_callgraph(args):
    """Reliary callgraph with anchor + expansion."""
    name = args.get("name", "")
    anchor_file = args.get("anchor_file", "")
    anchor_line = args.get("anchor_line", 0)
    try:
        text = _sessions[_reliary_session_key()].call("reliary_callgraph_v2",
            {"name": name, "anchor_file": anchor_file, "anchor_line": anchor_line, "path": "."})
        return text if text else "(no callgraph)"
    except Exception as e:
        return f"(callgraph error: {e})"


def tool_reliary_goto_def(args):
    """Reliary goto definition. Auto-anchored via find_references_with_source."""
    name = args.get("name", "")
    try:
        text = _sessions[_reliary_session_key()].call("reliary_find_references_with_source",
            {"name": name, "path": ".", "threshold": 0.05, "format": "grep", "limit": 5})
        defs = [l for l in text.split("\n") if "fn " + name in l]
        return "\n".join(defs[:5]) if defs else text[:500]
    except Exception as e:
        return f"(goto_def error: {e})"


def tool_reliary_methods_on(args):
    """Reliary methods-on-type enumeration."""
    name = args.get("name", "")
    try:
        text = _sessions[_reliary_session_key()].call("reliary_methods_on", {"name": name})
        d = json.loads(text) if text.startswith("{") else {}
        methods = d.get("methods", [])
        return "\n".join(f"{m['name']}: {m['file'].split('/')[-1]}:{m['line']}" for m in methods[:30])
    except Exception as e:
        return f"(methods_on error: {e})"


def tool_reliary_search(args):
    """Reliary BM25 search."""
    query = args.get("query", "")
    try:
        text = _sessions[_reliary_session_key()].call("reliary_search", {"query": query, "path": "."})
        if text.startswith("["):
            results = json.loads(text)
        else:
            return "(no search results)"
        lines = []
        for r in results[:10]:
            f = r.get("file", "").replace(TOKIO_CORPUS + "/", "")
            lines.append(f"{f} score={r.get('score', 0):.2f}")
        return "\n".join(lines) if lines else "(no search results)"
    except Exception as e:
        return f"(search error: {e})"


def tool_altbackend_search_graph(args):
    """ALTBACKEND search_graph via CLI."""
    try:
        r = subprocess.run(
            [ALTBACKEND_BIN, "cli", "search_graph",
             json.dumps({"project": ALTBACKEND_PROJECT, "query": args.get("query", ""),
                         "limit": args.get("limit", 20)})],
            capture_output=True, text=True, timeout=30)
        data = json.loads(r.stdout)
        results = data.get("results", [])
        out = []
        for h in results[:20]:
            out.append(f"{h.get('name','')}:{h.get('label','')} {h.get('file_path','')}:{h.get('start_line',0)}")
        return "\n".join(out) if out else "(no search results)"
    except Exception as e:
        return f"(altbackend error: {e})"


def tool_altbackend_get_code_snippet(args):
    """ALTBACKEND get_code_snippet via CLI."""
    try:
        r = subprocess.run(
            [ALTBACKEND_BIN, "cli", "get_code_snippet",
             json.dumps({"project": ALTBACKEND_PROJECT,
                         "qualified_name": args.get("qualified_name", ""),
                         "show_lines": args.get("show_lines", 3)})],
            capture_output=True, text=True, timeout=30)
        data = json.loads(r.stdout)
        if isinstance(data, dict) and "content" in data:
            return data["content"][:2000]
        return json.dumps(data)[:2000]
    except Exception as e:
        return f"(altbackend error: {e})"


def tool_altbackend_trace_path(args):
    """ALTBACKEND trace_path via CLI."""
    try:
        r = subprocess.run(
            [ALTBACKEND_BIN, "cli", "trace_path",
             json.dumps({"project": ALTBACKEND_PROJECT,
                         "name": args.get("name", ""),
                         "direction": args.get("direction", "both")})],
            capture_output=True, text=True, timeout=30)
        data = json.loads(r.stdout)
        callers = data.get("callers", []) if isinstance(data, dict) else []
        callees = data.get("callees", []) if isinstance(data, dict) else []
        lines = []
        if callers:
            lines.append(f"Callers: {', '.join(c.get('name','?') for c in callers[:10])}")
        if callees:
            lines.append(f"Callees: {', '.join(c.get('name','?') for c in callees[:10])}")
        return "\n".join(lines) if lines else "(no trace results)"
    except Exception as e:
        return f"(altbackend error: {e})"


def tool_altbackend_get_architecture(args):
    """ALTBACKEND get_architecture via CLI."""
    try:
        r = subprocess.run(
            [ALTBACKEND_BIN, "cli", "get_architecture",
             json.dumps({"project": ALTBACKEND_PROJECT})],
            capture_output=True, text=True, timeout=30)
        data = json.loads(r.stdout)
        return json.dumps(data, indent=2)[:3000]
    except Exception as e:
        return f"(altbackend error: {e})"


def tool_grep(args):
    """Bash grep -rn."""
    pattern = args.get("pattern", "")
    include = args.get("include", "*.rs")
    try:
        r = subprocess.run(
            ["grep", "-rEn", pattern, TOKIO_CORPUS, f"--include={include}"],
            capture_output=True, text=True, timeout=15)
        lines = r.stdout.strip().split("\n")[:50]
        formatted = []
        for line in lines:
            parts = line.split(":", 2)
            if len(parts) >= 3:
                file_path = parts[0].replace(TOKIO_CORPUS + "/", "")
                formatted.append(f"{file_path}:{parts[1]}: {parts[2].rstrip()}")
        return "\n".join(formatted) if formatted else "(no grep results)"
    except Exception as e:
        return f"(grep error: {e})"


def tool_read(args):
    """Read file content."""
    file_path = args.get("file", "")
    line_start = args.get("line_start", 1)
    line_end = args.get("line_end", 100)
    try:
        full_path = os.path.join(TOKIO_CORPUS, file_path)
        with open(full_path) as f:
            lines = f.readlines()
        start = max(0, line_start - 1)
        end = min(len(lines), line_end)
        return "".join(lines[start:end])
    except Exception as e:
        return f"(read error: {e})"


def tool_bash(args):
    """Run a bash command. When RELIARY_SIFT_BASH=1, pipes through `reliary wrap`."""
    cmd = args.get("command", "")
    if not cmd:
        return "(no command)"
    use_sift = os.environ.get("RELIARY_SIFT_BASH") == "1"
    try:
        if use_sift and RELIARY_BIN:
            r = subprocess.run(
                [RELIARY_BIN, "wrap", "bash", "-c", cmd],
                capture_output=True, text=True, timeout=60, cwd=TOKIO_CORPUS)
        else:
            r = subprocess.run(
                ["bash", "-c", cmd],
                capture_output=True, text=True, timeout=60, cwd=TOKIO_CORPUS)
        output = r.stdout
        if r.stderr and r.returncode != 0:
            output += f"\n[stderr]\n{r.stderr[:500]}"
        return output[:8000] if output else f"(exit code {r.returncode}, no output)"
    except subprocess.TimeoutExpired:
        return "(command timed out)"
    except Exception as e:
        return f"(bash error: {e})"


def tool_reliary_pack_query(args):
    """Reliary pack_query — focused slice for a specific symbol."""
    name = args.get("name", "")
    path = args.get("path", ".")
    try:
        text = _sessions[_reliary_session_key()].call("reliary_pack_query",
            {"name": name, "path": path})
        return text[:5000] if text else "(no pack slice)"
    except Exception as e:
        return f"(pack_query error: {e})"


def tool_reliary_dead_symbols(args):
    """V27: Find dead code. Wraps reliary_dead_symbols."""
    path = args.get("path", ".")
    limit = args.get("limit", 30)
    functions_only = args.get("functions_only", True)
    try:
        text = _sessions[_reliary_session_key()].call("reliary_dead_symbols",
            {"path": path, "limit": limit, "functions_only": functions_only})
        return text[:3000] if text else "(no dead code found)"
    except Exception as e:
        return f"(find_dead_code error: {e})"


# Tool registries per condition — V27 7-tool surface
RELIARY_TOOLS = {
    "search": tool_reliary_search,
    "find_references": tool_reliary_find_references,
    "goto_def": tool_reliary_goto_def,
    "call_graph": tool_reliary_callgraph,
    "list_methods": tool_reliary_methods_on,
    "find_dead_code": tool_reliary_dead_symbols,
    "describe": tool_reliary_pack_query,
}

RELIARY_TOOLS["dead_symbols"] = tool_reliary_dead_symbols

ALTBACKEND_TOOLS = {
    "search_graph": tool_altbackend_search_graph,
    "get_code_snippet": tool_altbackend_get_code_snippet,
    "trace_path": tool_altbackend_trace_path,
    "get_architecture": tool_altbackend_get_architecture,
}

GREP_TOOLS = {
    "grep": tool_grep,
    "read": tool_read,
    "bash": tool_bash,
}

CONDITION_TOOLS = {
    "A": RELIARY_TOOLS,
    "B": ALTBACKEND_TOOLS,
    "C": GREP_TOOLS,
    "D": RELIARY_TOOLS,  # pack + reliary tools
    # F: same tools as A but with RELIARY_SIFT_BASH=1 (bash auto-rewrite).
    # E/G removed: RELIARY_SIFT_TOOLS compressed MCP output that was already compact.
    "F": RELIARY_TOOLS,
}

CONDITION_NAMES = {"A": "reliary", "B": "altbackend", "C": "grep", "D": "reliary+pack",
                   "F": "reliary+sift_bash"}

# ============================================================
# System prompts per condition
# ============================================================

RELIARY_SYS = """You are a code intelligence agent. Answer questions about the codebase using the tools below.

You have 4 tools:

1. search(query) — Find files/symbols by topic. BM25 full-text search.
2. find_references(name, def_only?, usage_only?, methods?, dead_only?, path_filter?) — Find symbol usages.
   - def_only=true → "where is X defined" (returns top definition)
   - usage_only=true → "who calls X" (returns call sites)
   - methods=true → "list methods on Type X"
   - dead_only=true → "find dead code" (pass path to scope)
   - path_filter="crates/reliary-search" → restrict to a module
3. call_graph(name, direction="both") — Call graph for a symbol.
   - direction="outbound" → "what does X call" (callees/helpers)
   - direction="inbound" → "who calls X" (callers)
4. describe(name) — Explain a symbol: purpose, signature, callers.

TOOL SELECTION:
- "where is X defined?" → find_references(name=X, def_only=true)
- "who calls X?" → find_references(name=X, usage_only=true)
- "what does X call?" / "which helpers/functions does X use internally?" → call_graph(name=X) — find_references CANNOT answer this; usage_only returns callers, not callees
- "find references/usages of X" → find_references(name=X)
- "find implementations in module X" → find_references(name=X, path_filter="X/")
- "list methods on Type X" → find_references(name=X, methods=true)
- "find dead code in module X" → find_references(dead_only=true, path="X") — ALWAYS pass the module/crate path from the question (e.g. "crates/reliary-search/src"). Never pass path="." or omit it; a whole-repo dead-code scan returns irrelevant files. If the first result contains files outside the asked module, re-run with the scoped path.
- "explain X" / "what does X do" → describe(name=X)
- "search for files about topic" → search(query="topic")

ANSWER RULES:
- If a tool result already answers the question, STOP exploring and give your final answer immediately.
- Your tools return one-line answers. COPY the tool output verbatim into your final answer.
- Do NOT add types, files, methods, or line numbers not in the tool output.
- Do NOT remove items from the tool output.
- If the tool says "X is defined at file:line", your answer is "X is defined at file:line".
- Your training data may be from a different version — trust the tool output over your training data.
- COMPLETENESS: if the question asks for a list (callers, methods, fields), include EVERY item the tool returned with its file:line. Never summarize to names alone.
- AMBIGUITY: many names match both a struct and a function. If the question says "struct"/"type", report the tag=2 definition; if "function"/"fn", report tag=1. When unsure, report BOTH locations and label each.

To use a tool, respond with ONE LINE of JSON:
{"tool": "<name>", "args": {"name": "...", "anchor_file": "...", "anchor_line": <int>}}

When done, respond with ONE LINE of JSON:
{"final": true, "answer": "your complete answer"}

NO prose. NO markdown. JSON only. ONE line per response."""

ALTBACKEND_SYS = """You are a code intelligence agent with limited turns. You MUST answer in 3-5 tool calls. Do not explore endlessly.

Tools:
- search_graph(query, limit=20): Search symbols. Returns name, label, file_path, start_line.
- get_code_snippet(qualified_name, show_lines=3): Read source code for a symbol.
- trace_path(name, direction="both"): Show callers/callees.
- get_architecture(): Codebase structure overview.

To use a tool, respond with ONE LINE of JSON:
{"tool": "<name>", "args": {"query": "...", "qualified_name": "...", "limit": <int>}}

When done, respond with ONE LINE of JSON:
{"final": true, "answer": "your complete answer"}

NO prose. NO markdown. JSON only. ONE line per response. Answer in 3-5 tool calls maximum."""

GREP_SYS = """You are a code intelligence agent with limited turns. You MUST answer in 3-5 tool calls. Do not explore endlessly.

Tools:
- grep(pattern, include="*.rs"): Search files. Returns file:line: code lines.
- read(file, line_start=1, line_end=100): Read file content.
- bash(command): Run a shell command in the tokio corpus directory. Use for tests, grep, find, wc.

To use a tool, respond with ONE LINE of JSON:
{"tool": "<name>", "args": {"pattern": "...", "file": "..."}}

When done, respond with ONE LINE of JSON:
{"final": true, "answer": "your complete answer"}

NO prose. NO markdown. JSON only. ONE line per response. Answer in 3-5 tool calls maximum."""

SYSTEM_PROMPTS = {"A": RELIARY_SYS, "B": ALTBACKEND_SYS, "C": GREP_SYS}

# Condition D: reliary tools + holographic pack pre-loaded as context.
# The pack is a cache-stable prefix giving the model a structural overview
# of the codebase before any tool calls. This tests whether pre-loading
# context reduces tool-call count without hurting accuracy.
PACK_PATH = "/tmp/tokio-corpus-pack.md"

def _load_pack():
    p = Path(PACK_PATH)
    if p.exists() and p.stat().st_mtime > (time.time() - 7200):
        return p.read_text()
    return None

PACK_PREFIX = _load_pack()
if PACK_PREFIX:
    RELIARY_PACK_SYS = RELIARY_SYS + "\n\nCODEBASE STRUCTURE (pre-loaded for context, do not re-fetch):\n" + PACK_PREFIX[:50000]
    SYSTEM_PROMPTS["D"] = RELIARY_PACK_SYS
    CONDITION_NAMES["D"] = "reliary+pack"

# F shares RELIARY_SYS — differs from A only in bash auto-rewrite env var.
for cond in ("F",):
    SYSTEM_PROMPTS[cond] = RELIARY_SYS

# ============================================================
# Task definitions
# ============================================================

TASKS = [
    {
        "id": "task_consume_impls",
        "question": "In the tokio codebase, how many different types implement a `consume` method? List the type names and file paths where each is defined. Example: 'Take in io/util/take.rs'",
        "rubric": {
            "accept": ["Take", "BufStream", "BufWriter", "Empty", "Chain",
                       "AsyncBufRead", "reader"],
            "min_count": 3,
            "max_count": 12,
        },
        "category": "find_references",
    },
    {
        "id": "task_block_on_chain",
        "question": "Trace the call chain from `Runtime::block_on` to the point where a task is added to a work queue. List each function in the chain with its file:line. Give me the path as: fn1 -> fn2 -> fn3.",
        "rubric": {
            "accept_keywords": ["block_on", "spawn", "schedule", "wake",
                                "push", "queue", "sqlite_worker", "reactor"],
            "min_steps": 2,
        },
        "category": "call_graph",
    },
    {
        "id": "task_split_return_type",
        "question": "Find the `split` method on semaphore permit types in tokio. What does it return? Find the return type, its definition file:line, and list all methods defined on that return type.",
        "rubric": {
            "accept_keywords": ["SemaphorePermit", "semaphore", "permit",
                                "split", "forget"],
            "min_methods": 0,
        },
        "category": "goto_def",
    },
    {
        "id": "task_bufwriter_write_chain",
        "question": "When you call `write` on a `BufWriter`, which inner methods does it eventually call? Trace through the delegation: BufWriter::write -> ??? -> ???. List file:line for each step.",
        "rubric": {
            "accept_keywords": ["write", "flush", "poll_write", "inner",
                                "pin", "write_buf", "buffer"],
            "min_steps": 2,
        },
        "category": "call_graph",
    },
    {
        "id": "task_poll_method_search",
        "question": "Find the `poll` method on the `Sleep` type in the time module. List file:line for the definition, and find 3 places where Sleep::poll is actually called. (The type might be `Sleep` or something related to timers.)",
        "rubric": {
            "accept_keywords": ["sleep", "timer", "poll", "time", "waker",
                                "future", "delay"],
            "min_calls": 2,
        },
        "category": "find_references",
    },
    {
        "id": "task_test_failures",
        "question": "Run `cargo test --workspace` in the tokio codebase and report: (1) total number of test suites run, (2) how many passed, (3) how many failed, (4) if any failed, list the failing test names.",
        "rubric": {
            "accept_keywords": ["test", "passed", "run", "result"],
            "min_steps": 1,
        },
        "category": "bash_test",
    },
    {
        "id": "task_find_todos",
        "question": "Find all TODO and FIXME comments in the tokio source code (src/ directory). Count them and list the first 5 with file:line:comment.",
        "rubric": {
            "accept_keywords": ["TODO", "FIXME", "todo", "fixme"],
            "min_count": 1,
        },
        "category": "bash_grep",
    },
    {
        "id": "task_count_fns",
        "question": "Count the total number of `pub fn` and `pub async fn` declarations in the tokio src/ directory. Give the total count.",
        "rubric": {
            "accept_keywords": ["pub", "fn", "count", "total"],
            "min_steps": 1,
        },
        "category": "bash_count",
    },
]

# ============================================================
# LLM response parsing
# ============================================================

def _extract_json_objects(text):
    """Extract balanced JSON objects from text.
    
    Handles nested braces correctly — unlike regex, which stops at the first
    closing brace. Scans for '{' and tracks depth to find the matching '}'.
    """
    objects = []
    i = 0
    while i < len(text):
        if text[i] == '{':
            depth = 0
            start = i
            in_string = False
            escape = False
            while i < len(text):
                c = text[i]
                if escape:
                    escape = False
                elif c == '\\' and in_string:
                    escape = True
                elif c == '"' and not escape:
                    in_string = not in_string
                elif not in_string:
                    if c == '{':
                        depth += 1
                    elif c == '}':
                        depth -= 1
                        if depth == 0:
                            objects.append(text[start:i+1])
                            break
                i += 1
        i += 1
    return objects


def parse_llm_response(content):
    """Parse the LLM's response. Returns (is_final, payload_dict)."""
    content = (content or "").strip()
    if not content:
        return (None, None)
    # Try direct JSON parse first (fastest path)
    try:
        d = json.loads(content)
        if "tool" in d:
            return ("tool", d)
        if "final" in d or "answer" in d:
            return ("final", d)
    except Exception:
        pass
    # Extract balanced JSON objects (handles nested braces)
    for obj_text in _extract_json_objects(content):
        try:
            d = json.loads(obj_text)
            if "tool" in d:
                return ("tool", d)
            if "final" in d or "answer" in d:
                return ("final", d)
        except Exception:
            continue
    # Fallback: check for tool-call or final-answer patterns before treating
    # as final. This prevents premature "final" on prose that precedes a
    # tool call the parser might have missed.
    has_tool_pattern = '"tool"' in content or '"tool_name"' in content
    has_final_pattern = '"final"' in content or '"answer"' in content
    if has_tool_pattern and not has_final_pattern:
        # Model intended a tool call but JSON was malformed — don't treat as final
        return (None, None)
    if len(content) > 20 and ("{" not in content):
        return ("final", {"final": True, "answer": content[:2000]})
    # Last resort: if content has no JSON structure at all, treat as final
    if not has_tool_pattern and not has_final_pattern and len(content) > 20:
        return ("final", {"final": True, "answer": content[:2000]})
    return (None, None)


def execute_tool(cond, tool_name, tool_args):
    """Execute a tool call and return output string."""
    # V14: set current cond so tool dispatchers (E/F/G) can pick the right session
    _current_cond["cond"] = cond
    tools = CONDITION_TOOLS[cond]
    fn = tools.get(tool_name)
    if not fn:
        return f"(tool '{tool_name}' not found in condition {cond})"
    t0 = time.time()
    try:
        result = fn(tool_args)
    except Exception as e:
        result = f"(tool error: {e})"
    elapsed = time.time() - t0
    return result, elapsed


def _ensure_sessions(cond):
    """Create persistent MCP sessions for the condition if needed."""
    # A/D: reliary tools (no sift). F: reliary + bash sift (RELIARY_SIFT_BASH=1).
    # E/G removed: RELIARY_SIFT_TOOLS was the wrong layer (MCP output is already
    # compact; sift belongs on bash output, RTK-style).
    if cond in ("A", "D") and "reliary" not in _sessions:
        _sessions["reliary"] = MCPSession(RELIARY_BIN, TOKIO_CORPUS, "reliary")
    elif cond == "F" and "reliary_sift_bash" not in _sessions:
        _sessions["reliary_sift_bash"] = MCPSession(
            RELIARY_BIN, TOKIO_CORPUS, "reliary+sift_bash",
            extra_env={"RELIARY_SIFT_BASH": "1"})
    if cond == "B" and "altbackend" not in _sessions:
        _sessions["altbackend"] = MCPSession(ALTBACKEND_BIN, TOKIO_CORPUS, "altbackend")


def _close_sessions():
    """Close all persistent MCP sessions."""
    for key in list(_sessions):
        _sessions[key].close()
        del _sessions[key]


# ============================================================
# Single task run
# ============================================================

def run_task(task, cond, model, seed, timeout_total=300):
    """Run one task with one condition and seed. Returns metrics dict."""
    _ensure_sessions(cond)
    _current_cond["cond"] = cond
    rng = random.Random(seed)
    sys_prompt = SYSTEM_PROMPTS[cond]

    messages = [
        {"role": "system", "content": sys_prompt},
        {"role": "user", "content": task["question"]},
    ]

    metrics = {
        "task_id": task["id"],
        "cond": cond,
        "cond_name": CONDITION_NAMES[cond],
        "model": model,
        "seed": seed,
        "wall_time": 0,
        "tool_calls": 0,
        "turns": 0,
        "tokens_in": 0,
        "tokens_out": 0,
        "weighted_cost": 0,
        "tool_bytes": 0,
        "dead_end_calls": 0,
        "first_code_latency": None,
        "final_answer": "",
        "error": None,
        "timed_out": False,
    }

    t_start = time.time()
    last_message = ""

    for turn in range(MAX_TURNS):
        if time.time() - t_start > timeout_total:
            metrics["timed_out"] = True
            metrics["wall_time"] = time.time() - t_start
            metrics["turns"] = turn
            return metrics

        # On last turn, force the LLM to give final answer
        if turn == MAX_TURNS - 1:
            messages.append({"role": "user", "content": "This is your LAST turn. Give your FINAL answer now. ONE LINE of JSON: {\"final\": true, \"answer\": \"...\"}"})

        resp = deepseek_chat(messages, model=model, max_tokens=1500,
                              timeout=120, disable_thinking=True)

        if "error" in resp:
            metrics["error"] = str(resp["error"])[:200]
            metrics["wall_time"] = time.time() - t_start
            metrics["turns"] = turn
            return metrics

        msg = resp.get("choices", [{}])[0].get("message", {})
        content = msg.get("content", "") or msg.get("reasoning_content", "")
        usage = resp.get("usage", {})

        metrics["tokens_in"] += usage.get("prompt_tokens", 0)
        metrics["tokens_out"] += usage.get("completion_tokens", 0)
        metrics["turns"] = turn + 1
        last_message = content

        action_type, action = parse_llm_response(content)

        if action_type == "final":
            metrics["wall_time"] = time.time() - t_start
            metrics["final_answer"] = action.get("answer", content[:2000])
            metrics["weighted_cost"] = metrics["tokens_in"] + 4 * metrics["tokens_out"]
            return metrics

        elif action_type == "tool":
            tool_name = action.get("tool", action.get("name", ""))
            tool_args = action.get("args", action.get("arguments", action))
            if isinstance(tool_args, str):
                try:
                    tool_args = json.loads(tool_args) if tool_args.strip().startswith("{") else {"query": tool_args}
                except Exception:
                    tool_args = {"query": tool_args}

            output, tool_elapsed = execute_tool(cond, tool_name, tool_args)
            metrics["tool_calls"] += 1
            metrics["tool_bytes"] += len(output.encode())

            # Debug log
            print(f"\n      [tool] {tool_name}({tool_args}) -> {repr(output[:120])}", file=sys.stderr)

            # Dead-end detection
            no_hits_markers = ["(no ", "(no results)", "(no definition", "(no trace",
                               "(no search", "(error", "(altbackend error", "(grep error", "(read error"]
            if any(output.strip().startswith(m) for m in no_hits_markers):
                metrics["dead_end_calls"] += 1

            # First code latency: time until LLM sees actual source code
            if metrics["first_code_latency"] is None and len(output) > 100:
                # Heuristic: if output contains source code (not just metadata)
                if "fn " in output or "def " in output or "pub " in output or "impl " in output:
                    metrics["first_code_latency"] = time.time() - t_start

            # Append assistant + tool result to messages
            messages.append({"role": "assistant", "content": content})
            messages.append({"role": "user", "content": f"Tool result:\n{output[:4000]}"})

        else:
            # Neither tool nor final — treat as final answer
            metrics["wall_time"] = time.time() - t_start
            metrics["final_answer"] = content[:2000]
            metrics["weighted_cost"] = metrics["tokens_in"] + 4 * metrics["tokens_out"]
            return metrics

    # Exceeded max turns
    metrics["timed_out"] = True
    metrics["wall_time"] = time.time() - t_start
    metrics["final_answer"] = last_message[:2000]
    metrics["weighted_cost"] = metrics["tokens_in"] + 4 * metrics["tokens_out"]
    return metrics


# ============================================================
# Scoring
# ============================================================

def score_answer(task, answer_text):
    """Score the answer against the task rubric. Returns 0-3."""
    rubric = task["rubric"]
    answer = (answer_text or "").lower()
    if len(answer) < 5:
        return 0

    if "accept" in rubric:
        expected = rubric["accept"]
        found = sum(1 for e in expected if e.lower() in answer)
        total = len(expected)
        ratio = found / total if total > 0 else 0
        if ratio >= 0.7:
            return 3
        elif ratio >= 0.4:
            return 2
        elif ratio >= 0.15:
            return 1
        else:
            return 0

    elif "accept_keywords" in rubric:
        keywords = rubric["accept_keywords"]
        found = sum(1 for kw in keywords if kw.lower() in answer)
        ratio = found / len(keywords) if keywords else 0
        if ratio >= 0.7:
            return 3
        elif ratio >= 0.4:
            return 2
        elif ratio >= 0.15:
            return 1
        else:
            return 0

    # Fallback: any answer with code references
    if "file" in answer or ":" in answer:
        return 1
    return 0


# ============================================================
# Main runner
# ============================================================

def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--tasks", type=int, default=5)
    parser.add_argument("--model", default=DEEPSEEK_MODEL)
    parser.add_argument("--seeds", type=int, nargs="+", default=SEEDS)
    parser.add_argument("--conditions", default="A,B,C")
    parser.add_argument("--out", default=None)
    parser.add_argument("--timeout", type=int, default=300)
    args = parser.parse_args()

    tasks = TASKS[:args.tasks]
    conditions = args.conditions.split(",")
    seeds = args.seeds

    if args.out is None:
        ts = datetime.now(timezone.utc).strftime("%Y%m%dT%H%M%SZ")
        out_path = RESULTS_DIR / f"multi_turn_{ts}.jsonl"
    else:
        out_path = Path(args.out)
    out_path.parent.mkdir(parents=True, exist_ok=True)

    n_runs = len(tasks) * len(conditions) * len(seeds)
    print(f"=== Multi-Turn Benchmark ===")
    print(f"Tasks: {len(tasks)} x Conditions: {len(conditions)} x Seeds: {len(seeds)} = {n_runs} runs")
    print(f"Model: {args.model}")
    print(f"Output: {out_path}")
    print(f"Timeout: {args.timeout}s\n")

    all_results = []
    out_f = open(out_path, "w")

    # Determine already-completed runs from existing output
    existing = set()
    if out_path.exists():
        with open(out_path) as f:
            for line in f:
                try:
                    d = json.loads(line)
                    existing.add((d['task_id'], d['cond'], d['seed']))
                except Exception:
                    pass

    for task_idx, task in enumerate(tasks):
        print(f"Task {task_idx+1}/{len(tasks)}: {task['id']}")
        for seed_idx, seed in enumerate(seeds):
            # Interleave conditions per seed
            if seed_idx % 2 == 0:
                order = conditions
            else:
                order = list(reversed(conditions))
            for cond in order:
                key = (task['id'], cond, seed)
                if key in existing:
                    # Already done — just load and accumulate
                    with open(out_path) as f:
                        for line in f:
                            try:
                                d = json.loads(line)
                                if (d['task_id'], d['cond'], d['seed']) == key:
                                    all_results.append(d)
                                    break
                            except Exception:
                                pass
                    continue
                name = CONDITION_NAMES[cond]
                print(f"  seed={seed} cond={cond} ({name}) ... ", end="", flush=True)
                result = run_task(task, cond, args.model, seed, args.timeout)
                score = score_answer(task, result["final_answer"])
                result["task_score"] = score
                print(f"turns={result['turns']} calls={result['tool_calls']} "
                      f"wc={result['weighted_cost']} t={result['wall_time']:.0f}s "
                      f"score={score} answer={repr(result['final_answer'][:80])}")
                out_f.write(json.dumps(result) + "\n")
                out_f.flush()
                all_results.append(result)

    out_f.close()
    _close_sessions()
    print(f"\nDone. Output: {out_path}")
    print(f"Runs: {len(all_results)}")

    # Quick summary
    from statistics import median, mean
    for cond in conditions:
        sub = [r for r in all_results if r["cond"] == cond]
        if not sub:
            continue
        name = CONDITION_NAMES[cond]
        scores = [r["task_score"] for r in sub]
        wc = [r["weighted_cost"] for r in sub]
        wall = [r["wall_time"] for r in sub]
        calls = [r["tool_calls"] for r in sub]
        turns = [r["turns"] for r in sub]
        print(f"\n{cond} ({name}):")
        print(f"  score:      median={median(scores)} mean={mean(scores):.2f}")
        print(f"  wc:         median={median(wc):.0f} mean={mean(wc):.0f}")
        print(f"  wall_time:  median={median(wall):.0f}s mean={mean(wall):.0f}s")
        print(f"  tool_calls: median={median(calls):.0f} mean={mean(calls):.1f}")
        print(f"  turns:      median={median(turns):.0f} mean={mean(turns):.1f}")


if __name__ == "__main__":
    main()