#!/usr/bin/env python3
"""V75 integration test — the bench harness replaying a recorded session.

Unlike test_cassette.py (unit), this drives the real `run_long_session` code
path with a fake MCP session and a fake live transport, proving the bench
produces byte-identical results on replay with zero network access.
"""
import json
import os
import sys

import pytest

BENCH = os.path.join(os.path.dirname(__file__), "..")
sys.path.insert(0, BENCH)

import cassette  # noqa: E402
import llm_conn  # noqa: E402


class FakeSession:
    """Minimal stand-in for MCPSession: answers each tool with fixed text."""

    def __init__(self, binary, workdir, label="mcp", extra_env=None):
        self.calls = []

    def call(self, tool_name, arguments, timeout=60):
        self.calls.append(tool_name)
        return f"[fake {tool_name}] StructuralResult at structural.rs:16"

    def close(self):
        pass


def _install_fakes(mth, lsb, monkeypatch):
    monkeypatch.setattr(mth, "MCPSession", FakeSession)
    monkeypatch.setattr(mth, "TOKIO_CORPUS", "/tmp/fake-corpus")
    llm_conn.TOKIO_CORPUS = "/tmp/fake-corpus"
    # One query only, answerable from the fake tool output. SESSION_QUERIES
    # lives in long_session_bench (it is the module that reads it).
    monkeypatch.setattr(lsb, "SESSION_QUERIES", [
        {"id": "q_test", "question": "Where is StructuralResult defined?",
         "rubric": {"accept": ["structural.rs:16"], "min_count": 1}},
    ])


def _fake_live(counter):
    def live(request, timeout=30):
        counter[0] += 1
        n = counter[0]
        # Turn 1: ask for a tool. Turn 2+: final answer.
        if n % 2 == 1:
            content = json.dumps({"tool": "find_references", "args": {"name": "StructuralResult"}})
        else:
            content = json.dumps({"final": True,
                                  "answer": "StructuralResult at structural.rs:16"})
        return {
            "choices": [{"message": {"content": content}}],
            "usage": {"prompt_tokens": 100, "completion_tokens": 10,
                      "prompt_tokens_details": {"cached_tokens": 80}},
            "system_fingerprint": "fp-fake",
            "model": "deepseek-v4-flash",
        }
    return live


@pytest.fixture()
def bench_mods(monkeypatch):
    import multi_turn_harness as mth
    import long_session_bench as lsb
    _install_fakes(mth, lsb, monkeypatch)
    monkeypatch.setattr(lsb, "MAX_TURNS_PER_QUERY", 4)
    return mth, lsb


def test_record_then_replay_identical_results(tmp_path, monkeypatch, bench_mods):
    mth, lsb = bench_mods
    cdir = str(tmp_path / "cas")

    # ── Run 1: record ──────────────────────────────────────────────
    monkeypatch.setenv("RELIARY_CASSETTE", cdir)
    monkeypatch.setenv("RELIARY_CASSETTE_MODE", "record")
    monkeypatch.setenv("RELIARY_CASSETTE_INDEX_GEN", "11")
    counter = [0]
    cassette.configure_from_env(live_override=_fake_live(counter))
    run1 = lsb.run_long_session("A", "deepseek-v4-flash", 42, timeout_total=60)
    recorded_calls = counter[0]
    assert recorded_calls > 0

    # ── Run 2: replay-strict, network forbidden ────────────────────
    monkeypatch.setenv("RELIARY_CASSETTE_MODE", "replay-strict")

    def forbidden(req, timeout=30):
        raise AssertionError("NETWORK CALL DURING REPLAY")

    cassette.configure_from_env(live_override=forbidden)
    run2 = lsb.run_long_session("A", "deepseek-v4-flash", 42, timeout_total=60)

    assert run2["total_score"] == run1["total_score"]
    assert run2["total_tool_calls"] == run1["total_tool_calls"]
    assert run2["total_tokens_in"] == run1["total_tokens_in"]
    assert run2["total_tokens_out"] == run1["total_tokens_out"]
    assert run2["queries"][0]["answer"] == run1["queries"][0]["answer"]
    assert counter[0] == recorded_calls, "replay issued live calls"


def test_index_gen_change_breaks_replay(tmp_path, monkeypatch, bench_mods):
    """Negative control at the bench level: reindexing must invalidate."""
    mth, lsb = bench_mods
    cdir = str(tmp_path / "cas")
    monkeypatch.setenv("RELIARY_CASSETTE", cdir)
    monkeypatch.setenv("RELIARY_CASSETTE_MODE", "record")
    monkeypatch.setenv("RELIARY_CASSETTE_INDEX_GEN", "11")
    counter = [0]
    cassette.configure_from_env(live_override=_fake_live(counter))
    lsb.run_long_session("A", "deepseek-v4-flash", 42, timeout_total=60)

    monkeypatch.setenv("RELIARY_CASSETTE_MODE", "replay-strict")
    monkeypatch.setenv("RELIARY_CASSETTE_INDEX_GEN", "12")
    cassette.configure_from_env(live_override=_fake_live([0]))
    with pytest.raises(cassette.CassetteError, match="index_gen"):
        lsb.run_long_session("A", "deepseek-v4-flash", 42, timeout_total=60)


def test_session_row_carries_cassette_accounting(tmp_path, monkeypatch, bench_mods):
    mth, lsb = bench_mods
    cdir = str(tmp_path / "cas")
    monkeypatch.setenv("RELIARY_CASSETTE", cdir)
    monkeypatch.setenv("RELIARY_CASSETTE_MODE", "record")
    monkeypatch.setenv("RELIARY_CASSETTE_INDEX_GEN", "11")
    cassette.configure_from_env(live_override=_fake_live([0]))

    # Mirror main()'s accounting attachment.
    run = lsb.run_long_session("A", "deepseek-v4-flash", 42, timeout_total=60)
    c = cassette.active()
    run["cassette"] = {"mode": c.mode, "hits": c.stats["hits"],
                       "misses": c.stats["misses"], "recorded": c.stats["recorded"]}
    assert run["cassette"]["recorded"] > 0
    assert run["cassette"]["misses"] == run["cassette"]["recorded"]


def test_cassette_file_is_clean_and_versioned(tmp_path, monkeypatch, bench_mods):
    mth, lsb = bench_mods
    cdir = str(tmp_path / "cas")
    monkeypatch.setenv("RELIARY_CASSETTE", cdir)
    monkeypatch.setenv("RELIARY_CASSETTE_MODE", "record")
    monkeypatch.setenv("RELIARY_CASSETTE_INDEX_GEN", "11")
    cassette.configure_from_env(live_override=_fake_live([0]))
    lsb.run_long_session("A", "deepseek-v4-flash", 42, timeout_total=60)

    entries = [json.loads(l) for l in open(os.path.join(cdir, "cassette.jsonl"))]
    assert entries
    assert all(e["cassette_version"] == cassette.CASSETTE_VERSION for e in entries)
    assert all(e["index_gen"] == 11 for e in entries)
    blob = open(os.path.join(cdir, "cassette.jsonl")).read()
    assert "sk-" not in blob and "Bearer" not in blob
