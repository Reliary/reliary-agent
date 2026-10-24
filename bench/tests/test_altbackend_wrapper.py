"""The altbackend wrappers must send the keys the server requires.

A wrong parameter name is invisible from the outside: the server writes its
complaint to stderr, exits 0, and the wrapper's `json.loads(r.stdout)` raises
on the empty string, so the model receives an error string and the benchmark
records it as the tool's behaviour.

That happened with `trace_path` from 2026-06-30 to 2026-10-23: every recorded
run sent `name` where the server requires `function_name`, disabling the
caller/callee tracing that four of the ten questions depend on. Every B result
from that period is void.

This test pins the payload shape so it cannot recur.
"""
import importlib.util
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]


def load_harness():
    # The harness imports llm_conn, which reads no key at import time (PEP 562
    # lazy), so importing is safe without credentials.
    path = ROOT / "bench" / "multi_turn_harness.py"
    spec = importlib.util.spec_from_file_location("mth_under_test", path)
    mod = importlib.util.module_from_spec(spec)
    sys.modules["mth_under_test"] = mod
    spec.loader.exec_module(mod)
    return mod


def test_trace_path_sends_function_name_not_name():
    m = load_harness()
    payload = m.build_altbackend_payload("trace_path", {"function_name": "classify_structural"})
    assert "function_name" in payload, payload
    assert payload["function_name"] == "classify_structural"
    assert "name" not in payload, "the server has no `name` parameter"


def test_trace_path_accepts_the_legacy_key_from_the_prompt():
    # The model may still emit `name`; it must not become a silent failure.
    m = load_harness()
    payload = m.build_altbackend_payload("trace_path", {"name": "build_brace_graph"})
    assert payload["function_name"] == "build_brace_graph"


def test_every_required_key_is_present_for_every_tool():
    m = load_harness()
    sample = {
        "query": "classify_structural",
        "qualified_name": "crate.structural.classify_structural",
        "function_name": "classify_structural",
    }
    for tool in m.ALTBACKEND_REQUIRED:
        payload = m.build_altbackend_payload(tool, sample)
        for key in m.ALTBACKEND_REQUIRED[tool]:
            assert payload.get(key), f"{tool} is missing required key {key}: {payload}"


def test_a_missing_required_key_raises_instead_of_being_sent():
    """Negative control: the guard must reject a payload the server would.

    Without this test the guard could be deleted and the suite would still pass,
    because every wrapper happens to pass a complete sample today.
    """
    m = load_harness()
    try:
        m.build_altbackend_payload("trace_path", {})
    except ValueError as e:
        assert "function_name" in str(e), e
    else:
        raise AssertionError("an empty trace_path payload must raise, not be sent")


def test_required_map_matches_the_real_server():
    """If the server is installed, compare against its own tools/list."""
    import shutil
    import subprocess

    binary = shutil.which("codebase-memory-mcp") or str(Path.home() / ".local/bin/codebase-memory-mcp")
    if not Path(binary).exists():
        return  # not installed: the static half above still runs

    msgs = "\n".join([
        '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"t","version":"0"}}}',
        '{"jsonrpc":"2.0","method":"notifications/initialized"}',
        '{"jsonrpc":"2.0","id":2,"method":"tools/list"}',
        "",
    ])
    try:
        r = subprocess.run([binary], input=msgs, capture_output=True, text=True, timeout=40)
    except Exception:
        return
    import json
    server = {}
    for line in r.stdout.splitlines():
        line = line.strip()
        if not line.startswith("{"):
            continue
        try:
            d = json.loads(line)
        except json.JSONDecodeError:
            continue
        for t in (d.get("result") or {}).get("tools", []):
            if t.get("name") in load_harness().ALTBACKEND_REQUIRED:
                server[t["name"]] = sorted(t.get("inputSchema", {}).get("required", []))
    if not server:
        return  # server version did not answer; do not fail on the environment
    m = load_harness()
    for tool, required in server.items():
        ours = sorted(m.ALTBACKEND_REQUIRED.get(tool, []))
        assert ours == required, (
            f"{tool}: harness sends {ours}, server requires {required}")
