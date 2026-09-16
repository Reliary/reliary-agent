#!/usr/bin/env python3
"""V75 — deterministic transcripts for a stochastic LLM (bench-only).

The provider sampler cannot be forced deterministic from the client (verified
against the live DeepSeek API: `seed` is ignored, `response_format=json_object`
is unenforced, `json_schema` is unavailable). What CAN be forced is the
transcript: record every model response keyed by the exact sampling-affecting
request, then replay it byte-for-byte.

Design constraints:
- Bench-only. Never imported by the shipped binary.
- Key includes the corpus's persisted `meta.index_gen`, because tool output
  embeds the index stamp `[idx:xxxxxxxx]`; a reindex must invalidate entries.
- No key material is ever stored (the Authorization header is not part of the
  request body, and a test asserts the file is clean).
- `replay-strict` NEVER falls back to a live call. A miss is an error, so a
  comparison run cannot silently mix recordings with improvisation.

Usage:
    RELIARY_CASSETTE=bench/cassettes/tokio \
    RELIARY_CASSETTE_MODE=replay-strict \
    python3 bench/long_session_bench.py --conditions A,C --seeds 42

Inspect flip-risk (near-tie decisions):
    python3 bench/cassette.py summarize bench/cassettes/tokio
"""
import hashlib
import json
import os
import sqlite3
import sys
import threading
import time

CASSETTE_VERSION = 1

# Sampling-affecting request fields, in fixed order, that form the cache key.
# Anything that can change the sampled token stream belongs here.
KEY_FIELDS = (
    "model",
    "messages",
    "max_tokens",
    "temperature",
    "thinking",
    "response_format",
    "disable_thinking",
    "logprobs",
    "top_logprobs",
)


class CassetteError(RuntimeError):
    """A strict-mode replay miss. Carries a diagnostic of what changed."""


def _canonical(obj) -> str:
    """Canonical JSON: sorted keys, tight separators, UTF-8 preserved."""
    return json.dumps(obj, sort_keys=True, separators=(",", ":"), ensure_ascii=False)


def index_gen_for(workdir: str) -> int:
    """Read the corpus's persisted reindex generation (0 if absent).

    Read-only; never creates or migrates the index. Mirrors the M4 stamp the
    MCP server appends to tool output (`schema::index_gen`).
    """
    path = os.path.join(workdir, ".reliary", "index.sqlite")
    if not os.path.exists(path):
        return 0
    try:
        con = sqlite3.connect(f"file:{path}?mode=ro", uri=True)
        try:
            row = con.execute(
                "SELECT value FROM meta WHERE key='index_gen'").fetchone()
            return int(row[0]) if row else 0
        finally:
            con.close()
    except Exception:
        return 0


def request_key(request: dict, index_gen: int) -> str:
    """SHA-256 over the canonical sampling-affecting request + environment.

    `index_gen` is mixed in as a top-level key (not into the request body) so
    the diagnostic can name it when a miss is caused by a reindex.
    """
    subset = {k: request.get(k) for k in KEY_FIELDS if k in request}
    payload = _canonical({
        "cassette_version": CASSETTE_VERSION,
        "request": subset,
        "index_gen": index_gen,
    })
    return hashlib.sha256(payload.encode("utf-8")).hexdigest()


def _missing_diagnostic(request: dict, entries: dict, index_gen: int) -> str:
    """Explain a strict miss by finding which key component differs."""
    import copy
    for stored_key, entry in entries.items():
        probe = copy.deepcopy(entry.get("request", {}))
        diffs = []
        for k in KEY_FIELDS:
            if probe.get(k) != request.get(k):
                if k == "messages":
                    a = probe.get(k) or []
                    b = request.get(k) or []
                    if len(a) != len(b):
                        diffs.append(f"messages length {len(a)} != {len(b)}")
                    else:
                        for i, (ma, mb) in enumerate(zip(a, b)):
                            if ma != mb:
                                diffs.append(
                                    f"messages[{i}] differs "
                                    f"(role {ma.get('role')} vs {mb.get('role')}, "
                                    f"content len {len(str(ma.get('content')))} "
                                    f"vs {len(str(mb.get('content')))})")
                                break
                else:
                    diffs.append(f"{k}: {probe.get(k)!r} -> {request.get(k)!r}")
        if diffs:
            return (f"closest stored entry differs: " + "; ".join(diffs[:4]))
        if entry.get("index_gen") != index_gen:
            return (f"same request but index_gen {entry.get('index_gen')} != "
                    f"{index_gen} (corpus was reindexed)")
    if entries:
        return (f"{len(entries)} entries present, none match the request "
                "(messages or sampling params changed)")
    return "cassette is empty"


class Cassette:
    """Append-only record/replay store for model responses."""

    def __init__(self, path: str, mode: str = "auto", index_gen: int = 0,
                 live=None):
        if mode not in ("record", "replay-strict", "auto"):
            raise ValueError(f"unknown cassette mode: {mode!r}")
        self.path = path
        self.mode = mode
        self.index_gen = index_gen
        self._live = live
        self._lock = threading.Lock()
        self.entries = {}
        self.stats = {"hits": 0, "misses": 0, "recorded": 0}
        self._load()

    # ── persistence ────────────────────────────────────────────────
    def _load(self):
        if not os.path.exists(self.path):
            return
        with open(self.path, encoding="utf-8") as f:
            for line in f:
                line = line.strip()
                if not line:
                    continue
                try:
                    e = json.loads(line)
                except Exception:
                    continue
                if e.get("cassette_version") != CASSETTE_VERSION:
                    continue
                self.entries[e["key"]] = e

    def _append(self, entry: dict):
        os.makedirs(os.path.dirname(os.path.abspath(self.path)), exist_ok=True)
        with open(self.path, "a", encoding="utf-8") as f:
            f.write(json.dumps(entry, ensure_ascii=False) + "\n")

    # ── core ───────────────────────────────────────────────────────
    def respond(self, request: dict, timeout: int = 30) -> dict:
        """Return the response for `request`, replaying or recording.

        The live transport (`self._live`) has the signature
        `(request_body, timeout) -> response_dict` — the default is
        `llm_conn._pooled_chat`, which matches.
        """
        key = request_key(request, self.index_gen)
        with self._lock:
            entry = self.entries.get(key)
            if entry is not None:
                self.stats["hits"] += 1
                return dict(entry["response"])
            if self.mode == "replay-strict":
                self.stats["misses"] += 1
                raise CassetteError(
                    f"[cassette] strict replay miss (key {key[:12]}…): "
                    f"{_missing_diagnostic(request, self.entries, self.index_gen)}")
            if self._live is None:
                raise CassetteError(
                    "[cassette] no live transport configured; cannot record")
            self.stats["misses"] += 1
            response = self._live(request, timeout)
            if isinstance(response, dict) and response.get("error"):
                # Never persist transport errors — they are not model decisions.
                return response
            record = {
                "cassette_version": CASSETTE_VERSION,
                "key": key,
                "index_gen": self.index_gen,
                "request": {k: request.get(k) for k in KEY_FIELDS if k in request},
                "response": response,
                "system_fingerprint": (response.get("system_fingerprint")
                                       if isinstance(response, dict) else None),
                "recorded_at": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
            }
            fp = flip_risk(request, response)
            if fp is not None:
                record["flip_risk"] = fp
            self.entries[key] = record
            self._append(record)
            self.stats["recorded"] += 1
            return response


def flip_risk(request: dict, response: dict):
    """Worst top1−top2 logprob margin across the response, or None.

    Report-only (the request asks for logprobs in cassette mode; this does not
    affect sampling). A small margin marks a near-tie decision where the
    sampler could reasonably have gone the other way.
    """
    try:
        lp = response["choices"][0].get("logprobs") or {}
        content = lp.get("content") or []
        margins = []
        for tok in content:
            alts = tok.get("top_logprobs") or []
            if len(alts) >= 2:
                margins.append(alts[0]["logprob"] - alts[1]["logprob"])
        if not margins:
            return None
        return {"worst_margin": round(min(margins), 4),
                "first_margin": round(margins[0], 4),
                "tokens": len(margins)}
    except Exception:
        return None


# ── process-global wiring used by llm_conn ──────────────────────────

_ACTIVE = None


def active() -> "Cassette | None":
    return _ACTIVE


def configure_from_env(default_index_gen: int = 0, live_override=None):
    """(Re)configure the process-global cassette from the environment.

    RELIARY_CASSETTE       — cassette directory or file path ("" disables)
    RELIARY_CASSETTE_MODE  — record | replay-strict | auto (default: auto)
    RELIARY_CASSETTE_INDEX_GEN — override index generation (else read from the
                                 bench corpus, defaulting to the tokio corpus)
    RELIARY_CORPUS         — corpus root whose index_gen is used for the key

    `live_override` injects a transport (tests); otherwise the pooled DeepSeek
    client is used. The transport bound per call by `deepseek_chat` still wins,
    so the caller's timeout is honored.
    """
    global _ACTIVE
    target = os.environ.get("RELIARY_CASSETTE", "").strip()
    if not target:
        _ACTIVE = None
        return None
    path = target if target.endswith(".jsonl") else os.path.join(target, "cassette.jsonl")
    mode = os.environ.get("RELIARY_CASSETTE_MODE", "auto")
    from llm_conn import _pooled_chat  # late import: avoids a cycle at import time
    gen = os.environ.get("RELIARY_CASSETTE_INDEX_GEN")
    if gen:
        index_gen = int(gen)
    elif default_index_gen:
        index_gen = default_index_gen
    else:
        corpus = os.environ.get("RELIARY_CORPUS")
        if not corpus:
            import llm_conn
            corpus = llm_conn.TOKIO_CORPUS
        index_gen = index_gen_for(corpus)
    _ACTIVE = Cassette(path, mode=mode, index_gen=index_gen,
                       live=live_override or _pooled_chat)
    return _ACTIVE


def install_env_wiring(default_index_gen: int = 0, live=None):
    """Test hook: same as configure_from_env with an injected transport."""
    return configure_from_env(default_index_gen=default_index_gen,
                              live_override=live)


# ── CLI: summarize ──────────────────────────────────────────────────

def _cmd_summarize(argv):
    if not argv:
        print("usage: cassette.py summarize <dir-or-file> [--threshold 1.0]",
              file=sys.stderr)
        return 2
    path = argv[0]
    if not path.endswith(".jsonl"):
        path = os.path.join(path, "cassette.jsonl")
    threshold = 1.0
    if "--threshold" in argv:
        threshold = float(argv[argv.index("--threshold") + 1])
    total = hits = risky = 0
    worst = []
    with open(path, encoding="utf-8") as f:
        for line in f:
            line = line.strip()
            if not line:
                continue
            e = json.loads(line)
            total += 1
            fr = e.get("flip_risk")
            if fr:
                if fr["worst_margin"] < threshold:
                    risky += 1
                    worst.append((fr["worst_margin"], e["key"][:12]))
    worst.sort()
    print(f"cassette: {path}")
    print(f"  entries: {total}")
    print(f"  near-tie decisions (margin < {threshold}): {risky}")
    for m, k in worst[:10]:
        print(f"    margin {m:+.4f}  key {k}…")
    return 0


if __name__ == "__main__":
    if len(sys.argv) > 1 and sys.argv[1] == "summarize":
        sys.exit(_cmd_summarize(sys.argv[2:]))
    print(__doc__)
    sys.exit(0)
