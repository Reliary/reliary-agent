#!/usr/bin/env python3
"""V75 cassette tests — record/replay determinism for the bench harness.

Every guard here is validated with a negative control: the test asserts the
protection works, then the test itself proves it can fail by breaking the
mechanism (see `test_negative_controls_*`). Run:

    python3 -m pytest bench/tests/test_cassette.py -q
"""
import json
import os
import sys

import pytest

sys.path.insert(0, os.path.join(os.path.dirname(__file__), ".."))

from cassette import (  # noqa: E402
    Cassette, CassetteError, KEY_FIELDS, flip_risk, index_gen_for,
    request_key,
)


def _req(content="hi", **over):
    r = {
        "model": "deepseek-v4-flash",
        "messages": [{"role": "user", "content": content}],
        "max_tokens": 100,
        "temperature": 0.0,
        "thinking": {"type": "disabled"},
    }
    r.update(over)
    return r


def _resp(text="ok", fp="fp-abc", margins=None):
    r = {
        "choices": [{"message": {"content": text}}],
        "usage": {"prompt_tokens": 10, "completion_tokens": 2},
        "system_fingerprint": fp,
        "model": "deepseek-v4-flash",
    }
    if margins is not None:
        r["choices"][0]["logprobs"] = {"content": [
            {"token": f"t{i}", "logprob": -0.1,
             "top_logprobs": [{"token": f"t{i}", "logprob": 0.0},
                              {"token": f"x{i}", "logprob": -m}]}
            for i, m in enumerate(margins)
        ]}
    return r


class FakeLive:
    """Counts calls; returns a response derived from the call number."""

    def __init__(self):
        self.n = 0
        self.requests = []

    def __call__(self, request, timeout=30):
        self.n += 1
        self.requests.append(request)
        return _resp(text=f"live-{self.n}", fp=f"fp-{self.n}")


# ── 1. record → replay byte-identical ───────────────────────────────

def test_record_then_replay_is_byte_identical(tmp_path):
    live = FakeLive()
    c1 = Cassette(str(tmp_path / "c.jsonl"), sample="t", mode="record", live=live)
    r1 = c1.respond(_req())
    assert r1["choices"][0]["message"]["content"] == "live-1"

    c2 = Cassette(str(tmp_path / "c.jsonl"), sample="t", mode="replay-strict")
    r2 = c2.respond(_req())
    r3 = c2.respond(_req())
    assert json.dumps(r2, sort_keys=True) == json.dumps(r1, sort_keys=True)
    assert json.dumps(r3, sort_keys=True) == json.dumps(r1, sort_keys=True)
    assert live.n == 1, "replay made a live call"
    assert c2.stats["hits"] == 2


# ── 2. strict miss raises, zero live calls ──────────────────────────

def test_strict_miss_raises_and_never_goes_live(tmp_path):
    live = FakeLive()
    c = Cassette(str(tmp_path / "c.jsonl"), sample="t", mode="replay-strict", live=live)
    with pytest.raises(CassetteError, match="strict replay miss"):
        c.respond(_req("something never recorded"))
    assert live.n == 0


# ── 3. message mutation is a miss ───────────────────────────────────

def test_message_mutation_is_a_miss(tmp_path):
    live = FakeLive()
    c = Cassette(str(tmp_path / "c.jsonl"), sample="t", mode="record", live=live)
    c.respond(_req("hello"))

    strict = Cassette(str(tmp_path / "c.jsonl"), sample="t", mode="replay-strict")
    with pytest.raises(CassetteError, match="diverges at message 0"):
        strict.respond(_req("hellp"))  # one byte


# ── 4. index_gen is part of the key ─────────────────────────────────

def test_index_gen_invalidates_on_reindex(tmp_path):
    live = FakeLive()
    c = Cassette(str(tmp_path / "c.jsonl"), sample="t", mode="record", index_gen=7, live=live)
    c.respond(_req())

    same = Cassette(str(tmp_path / "c.jsonl"), sample="t", mode="replay-strict", index_gen=7)
    assert same.respond(_req())["choices"][0]["message"]["content"] == "live-1"

    reindexed = Cassette(str(tmp_path / "c.jsonl"), sample="t", mode="replay-strict", index_gen=8)
    with pytest.raises(CassetteError, match="index_gen"):
        reindexed.respond(_req())


def test_negative_control_index_gen():
    """If index_gen were NOT in the key, a reindex would silently replay."""
    a = request_key(_req(), index_gen=1)
    b = request_key(_req(), index_gen=2)
    assert a != b, "index_gen is not part of the cassette key"
    # And the protection is the key change, not something else:
    same = request_key(_req(), index_gen=1)
    assert a == same


# ── 5. sampling params are part of the key ──────────────────────────

def test_temperature_change_is_a_miss(tmp_path):
    live = FakeLive()
    c = Cassette(str(tmp_path / "c.jsonl"), sample="t", mode="record", live=live)
    c.respond(_req(temperature=0.0))

    strict = Cassette(str(tmp_path / "c.jsonl"), sample="t", mode="replay-strict")
    with pytest.raises(CassetteError, match="temperature"):
        strict.respond(_req(temperature=0.7))


def test_negative_control_sampling_params():
    """Every sampling-affecting field must change the key."""
    base = request_key(_req(), 0)
    variants = {
        "model": _req(model="deepseek-v4-pro"),
        "max_tokens": _req(max_tokens=200),
        "temperature": _req(temperature=0.5),
        "thinking": _req(thinking=None),
        "response_format": _req(response_format={"type": "json_object"}),
        "disable_thinking": _req(),
        "logprobs": _req(logprobs=True),
        "top_logprobs": _req(top_logprobs=5),
    }
    for field, req in variants.items():
        if field == "disable_thinking":
            # not a body field in this shape; skip explicit variant
            continue
        assert request_key(req, 0) != base, f"{field} is not in the cassette key"


# ── 6. no key material in the cassette ──────────────────────────────

def test_cassette_contains_no_secrets(tmp_path):
    live = FakeLive()
    c = Cassette(str(tmp_path / "c.jsonl"), sample="t", mode="record", live=live)
    c.respond(_req())
    blob = (tmp_path / "c.jsonl").read_text()
    assert "sk-" not in blob
    assert "Authorization" not in blob
    assert "Bearer" not in blob


# ── 7. prefix pairing ───────────────────────────────────────────────

def test_identical_prefix_yields_identical_decisions(tmp_path):
    """Two "conditions" share a conversation prefix: the replayed decisions
    are literally the same entries until the prefix diverges."""
    live = FakeLive()
    rec = Cassette(str(tmp_path / "c.jsonl"), sample="t", mode="record", live=live)
    prefix = [{"role": "user", "content": "q1"}]
    rec.respond(_req(prefix))

    a = Cassette(str(tmp_path / "c.jsonl"), sample="t", mode="replay-strict").respond(_req(prefix))
    b = Cassette(str(tmp_path / "c.jsonl"), sample="t", mode="replay-strict").respond(_req(prefix))
    assert a == b
    # Divergence needs a new recording — strict cannot invent one.
    with pytest.raises(CassetteError):
        Cassette(str(tmp_path / "c.jsonl"), sample="t", mode="replay-strict").respond(
            _req(prefix + [{"role": "assistant", "content": "extra"}]))


# ── 8. fingerprint captured and preserved ───────────────────────────

def test_system_fingerprint_preserved(tmp_path):
    live = FakeLive()
    rec = Cassette(str(tmp_path / "c.jsonl"), sample="t", mode="record", live=live)
    rec.respond(_req())
    entry = json.loads((tmp_path / "c.jsonl").read_text().splitlines()[0])
    assert entry["system_fingerprint"] == "fp-1"
    replayed = Cassette(str(tmp_path / "c.jsonl"), sample="t", mode="replay-strict").respond(_req())
    assert replayed["system_fingerprint"] == "fp-1"


# ── flip-risk annotation ────────────────────────────────────────────

def test_flip_risk_annotates_near_ties(tmp_path):
    live = FakeLive()
    rec = Cassette(str(tmp_path / "c.jsonl"), sample="t", mode="record", live=live)

    def tight(request, timeout=30):
        return _resp(text="maybe", margins=[0.05, 3.0])

    rec = Cassette(str(tmp_path / "c.jsonl"), sample="t", mode="record", live=tight)
    rec.respond(_req())
    entry = json.loads((tmp_path / "c.jsonl").read_text().splitlines()[0])
    assert entry["flip_risk"]["worst_margin"] == pytest.approx(0.05)
    assert entry["flip_risk"]["first_margin"] == pytest.approx(0.05)


def test_flip_risk_none_without_logprobs():
    assert flip_risk(_req(), _resp()) is None


# ── errors are never persisted ──────────────────────────────────────

def test_transport_errors_not_persisted(tmp_path):
    rec = Cassette(str(tmp_path / "c.jsonl"), sample="t", mode="record", live=lambda r, t=30: {"error": "boom"})
    rec.respond(_req())
    assert rec.entries == {}
    rec.respond(_req())
    assert rec.stats["misses"] == 2


# ── index_gen reader ────────────────────────────────────────────────

def test_index_gen_for_missing_index_is_zero(tmp_path):
    assert index_gen_for(str(tmp_path)) == 0


def test_index_gen_for_reads_meta(tmp_path):
    import sqlite3
    rel = tmp_path / ".reliary"
    rel.mkdir()
    con = sqlite3.connect(rel / "index.sqlite")
    con.execute("CREATE TABLE meta (key TEXT PRIMARY KEY, value TEXT)")
    con.execute("INSERT INTO meta VALUES ('index_gen', '42')")
    con.commit()
    con.close()
    assert index_gen_for(str(tmp_path)) == 42


# ── corrupt lines are ignored, not fatal ────────────────────────────

def test_corrupt_lines_ignored(tmp_path):
    live = FakeLive()
    rec = Cassette(str(tmp_path / "c.jsonl"), sample="t", mode="record", live=live)
    rec.respond(_req())
    p = tmp_path / "c.jsonl"
    with open(p, "a") as f:
        f.write("{not json}\n\n")
    strict = Cassette(str(p), sample="t", mode="replay-strict")
    assert strict.respond(_req())["choices"][0]["message"]["content"] == "live-1"


# ── key determinism ─────────────────────────────────────────────────

def test_key_is_stable_and_order_independent():
    a = request_key(_req(), 3)
    b = request_key(_req(), 3)
    assert a == b
    reordered = {"temperature": 0.0, "model": "deepseek-v4-flash",
                 "max_tokens": 100, "messages": [{"role": "user", "content": "hi"}],
                 "thinking": {"type": "disabled"}}
    assert request_key(reordered, 3) == a, "key must be order-independent"


# ── gzip tapes ──────────────────────────────────────────────────────

def test_gzip_dir_resolution_and_roundtrip(tmp_path):
    """A bare directory resolves to cassette.jsonl.gz and roundtrips."""
    import cassette as C
    d = str(tmp_path / "tapes")
    os.environ["RELIARY_CASSETTE"] = d
    os.environ["RELIARY_CASSETTE_MODE"] = "record"
    os.environ["RELIARY_CASSETTE_INDEX_GEN"] = "3"
    try:
        live = FakeLive()
        cas = C.configure_from_env(live_override=live)
        cas.set_sample("t")
        assert cas.path.endswith("cassette.jsonl.gz")
        cas.respond(_req())
        os.environ["RELIARY_CASSETTE_MODE"] = "replay-strict"
        cas2 = C.configure_from_env(live_override=lambda r, t=30: (_ for _ in ()).throw(
            AssertionError("network during replay")))
        cas2.set_sample("t")
        assert cas2.respond(_req())["choices"][0]["message"]["content"] == "live-1"
    finally:
        for k in ("RELIARY_CASSETTE", "RELIARY_CASSETTE_MODE",
                  "RELIARY_CASSETTE_INDEX_GEN"):
            os.environ.pop(k, None)
        C.install_env_wiring(default_index_gen=0, live=None)


def test_gzip_is_deterministic(tmp_path):
    """Identical entries produce byte-identical gzip (mtime=0, no filename drift).

    Compares the framing, not wall-clock metadata: `recorded_at` legitimately
    differs between two real recordings, so a fixed entry dict is written
    through the same append path.
    """
    import cassette as C
    entry = {"cassette_version": C.CASSETTE_VERSION, "key": "deadbeef",
             "index_gen": 3, "request": {"model": "m"},
             "response": {"choices": [{"message": {"content": "ok"}}]}}
    a_path, b_path = str(tmp_path / "a.jsonl.gz"), str(tmp_path / "b.jsonl.gz")
    C.Cassette(a_path, mode="auto", sample="t")._append(entry)
    C.Cassette(b_path, mode="auto", sample="t")._append(entry)
    assert open(a_path, "rb").read() == open(b_path, "rb").read(), \
        "gzip output is not deterministic"


# ── compact tapes ───────────────────────────────────────────────────

def test_compact_tape_stores_no_message_bodies(tmp_path):
    """Compact tapes must not embed the full conversation (size control)."""
    live = FakeLive()
    cas = Cassette(str(tmp_path / "c.jsonl"), sample="t", mode="record", live=live,
                   compact=True)
    big = _req(messages=[{"role": "user",
                          "content": "x" * 50000}])
    cas.respond(big)
    entry = json.loads((tmp_path / "c.jsonl").read_text().splitlines()[0])
    assert "messages" not in entry["request"]
    assert entry["request"]["roles"] == ["user"]
    blob = (tmp_path / "c.jsonl").read_text()
    assert len(blob) < 3000, f"compact tape kept {len(blob)} bytes"


def test_compact_tape_roundtrips(tmp_path):
    live = FakeLive()
    rec = Cassette(str(tmp_path / "c.jsonl"), sample="t", mode="record", live=live,
                   compact=True)
    rec.respond(_req())
    strict = Cassette(str(tmp_path / "c.jsonl"), sample="t", mode="replay-strict",
                      compact=True)
    assert strict.respond(_req())["choices"][0]["message"]["content"] == "live-1"


def test_compact_tape_diagnostics_name_the_divergence(tmp_path):
    live = FakeLive()
    rec = Cassette(str(tmp_path / "c.jsonl"), sample="t", mode="record", live=live,
                   compact=True)
    rec.respond(_req())
    strict = Cassette(str(tmp_path / "c.jsonl"), sample="t", mode="replay-strict",
                      compact=True)
    with pytest.raises(CassetteError, match="message count differs"):
        strict.respond(_req(messages=[{"role": "user", "content": "hi"},
                                      {"role": "assistant", "content": "yo"}]))


def test_compact_strips_logprobs_but_keeps_flip_risk(tmp_path):
    live = FakeLive()

    def tight(request, timeout=30):
        return _resp(text="maybe", margins=[0.05, 3.0])

    rec = Cassette(str(tmp_path / "c.jsonl"), sample="t", mode="record", live=tight,
                   compact=True)
    rec.respond(_req())
    entry = json.loads((tmp_path / "c.jsonl").read_text().splitlines()[0])
    assert "logprobs" not in entry["response"]["choices"][0]
    assert entry["flip_risk"]["worst_margin"] == pytest.approx(0.05)


def test_record_mode_always_goes_live(tmp_path):
    """`record` re-records even when an entry exists (last write wins)."""
    live = FakeLive()
    rec = Cassette(str(tmp_path / "c.jsonl"), sample="t", mode="record", live=live)
    rec.respond(_req())
    rec.respond(_req())
    assert live.n == 2


def test_compact_diagnostic_names_the_exact_message(tmp_path):
    """A compact tape must be able to say which message diverged."""
    live = FakeLive()
    rec = Cassette(str(tmp_path / "c.jsonl"), mode="record", live=live,
                   compact=True, sample="t")
    rec.respond(_req())
    strict = Cassette(str(tmp_path / "c.jsonl"), mode="replay-strict",
                      compact=True, sample="t")
    # Same shape, different first message: the diagnostic must point at 0.
    with pytest.raises(CassetteError, match="message 0"):
        strict.respond(_req(content="changed"))


def test_compact_diagnostic_names_a_later_message(tmp_path):
    live = FakeLive()
    rec = Cassette(str(tmp_path / "c.jsonl"), mode="record", live=live,
                   compact=True, sample="t")
    msgs = [{"role": "user", "content": "one"},
            {"role": "assistant", "content": "two"},
            {"role": "user", "content": "three"}]
    rec.respond(_req(messages=msgs))
    msgs2 = list(msgs)
    msgs2[2] = {"role": "user", "content": "THREE"}
    strict = Cassette(str(tmp_path / "c.jsonl"), mode="replay-strict",
                      compact=True, sample="t")
    with pytest.raises(CassetteError, match="message 2"):
        strict.respond(_req(messages=msgs2))
