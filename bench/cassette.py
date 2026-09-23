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
import gzip
import hashlib
import io
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


def request_key(request: dict, index_gen: int, sample: str | None = None) -> str:
    """SHA-256 over the canonical sampling-affecting request + environment.

    `index_gen` is mixed in as a top-level key (not into the request body) so
    the diagnostic can name it when a miss is caused by a reindex.

    `sample` names which independent sample of this input is being taken (the
    bench uses `cond-seed`). It is required because the harness builds the
    same first request for every seed — the seed only labels which stochastic
    draw the run represents. Without it, every seed would replay the first
    seed's decisions and the measured variance would be a lie.
    """
    subset = {k: request.get(k) for k in KEY_FIELDS if k in request}
    payload = _canonical({
        "cassette_version": CASSETTE_VERSION,
        "request": subset,
        "index_gen": index_gen,
        "sample": sample,
    })
    return hashlib.sha256(payload.encode("utf-8")).hexdigest()


def _missing_diagnostic(request: dict, entries: dict, index_gen: int) -> str:
    """Explain a strict miss by finding the closest stored entry and diffing it.

    Picks the entry with the longest shared message-prefix (not the first
    entry with any difference), then reports where the two diverge. This
    matters because a session has hundreds of stored turns and the first
    mismatch found by iteration is usually unrelated to the actual one.
    """
    req_msgs = request.get("messages") or []
    req_roles = [(m.get("role") if isinstance(m, dict) else "?") for m in req_msgs]
    req_hashes = [_msg_hash(m) for m in req_msgs]

    def roles_of(entry) -> list:
        probe = entry.get("request", {})
        if "roles" in probe:
            return list(probe["roles"] or [])
        return [(m.get("role") if isinstance(m, dict) else "?")
                for m in (probe.get("messages") or [])]

    def prefix_len(entry) -> int:
        """Number of leading messages that match (content when stored, else role)."""
        probe = entry.get("request", {})
        stored = probe.get("messages")
        if stored:
            n = 0
            for a, b in zip(stored, req_msgs):
                if a != b:
                    break
                n += 1
            return n
        stored_roles = roles_of(entry)
        n = 0
        for a, b in zip(stored_roles, req_roles):
            if a != b:
                break
            n += 1
        return n

    best, best_n = None, -1
    for entry in entries.values():
        n = prefix_len(entry)
        if n > best_n:
            best, best_n = entry, n
    if best is None:
        return "cassette is empty"

    probe = best.get("request", {})
    stored_msgs = probe.get("messages") or []
    stored_hashes = probe.get("msg_hashes")

    # Compact tape with per-message hashes: name the exact divergent turn.
    if stored_hashes is not None and not stored_msgs:
        for i, (ha, hb) in enumerate(zip(stored_hashes, req_hashes)):
            if ha != hb:
                role = req_roles[i] if i < len(req_roles) else "?"
                preview = ""
                if i < len(req_msgs) and isinstance(req_msgs[i], dict):
                    preview = str(req_msgs[i].get("content"))[:120]
                prev = str(req_msgs[i - 1].get("content"))[-160:] if i > 0 else ""
                return (f"message {i} (role={role}) differs from the recorded "
                        f"conversation.\n"
                        f"      previous message tail: {prev!r}\n"
                        f"      wanted content: {preview!r}")
        if len(stored_hashes) != len(req_hashes):
            return (f"message count differs: recorded {len(stored_hashes)}, "
                    f"replaying {len(req_hashes)}")
        for k in ("model", "max_tokens", "temperature"):
            if probe.get(k) != request.get(k):
                return f"{k}: {probe.get(k)!r} -> {request.get(k)!r}"
        if best.get("index_gen") != index_gen:
            return (f"same request but index_gen {best.get('index_gen')} != "
                    f"{index_gen} (corpus was reindexed)")
        return "messages and parameters match but the key differs (sample label?)"

    # Full (non-compact) tape with message bodies: diff at the divergence point.
    if stored_msgs:
        if best_n == len(stored_msgs) and best_n == len(req_msgs):
            for k in KEY_FIELDS:
                if k == "messages":
                    continue
                if probe.get(k) != request.get(k):
                    return f"{k}: {probe.get(k)!r} -> {request.get(k)!r}"
            if best.get("index_gen") != index_gen:
                return (f"same request but index_gen {best.get('index_gen')} != "
                        f"{index_gen} (corpus was reindexed)")
            return "messages and parameters match but the key differs (sample label?)"
        if best_n < len(stored_msgs):
            msg = stored_msgs[best_n]
            cur = req_msgs[best_n] if best_n < len(req_msgs) else None
            if cur is None:
                return (f"request has {len(req_msgs)} messages, closest entry has "
                        f"{len(stored_msgs)}; diverges at index {best_n} "
                        f"(request ended early)")
            a, b = str(msg.get("content"))[:200], str(cur.get("content"))[:200]
            return (f"diverges at message {best_n} "
                    f"(role {msg.get('role')} vs {cur.get('role')}):\n"
                    f"      stored: {a!r}\n"
                    f"      wanted: {b!r}")

    # Compact tape: only the role sequence is stored. Report how far the
    # closest entry's role sequence matches and what came next, which names
    # the turn at which the conversation diverged.
    stored_roles = roles_of(best)
    if best_n == len(stored_roles) and best_n == len(req_roles):
        for k in ("model", "max_tokens", "temperature"):
            if probe.get(k) != request.get(k):
                return f"{k}: {probe.get(k)!r} -> {request.get(k)!r}"
        if best.get("index_gen") != index_gen:
            return (f"same request but index_gen {best.get('index_gen')} != "
                    f"{index_gen} (corpus was reindexed)")
        return ("message content differs (compact tape stores roles only; "
                "the conversation diverged or a prompt changed)")
    nxt_stored = stored_roles[best_n] if best_n < len(stored_roles) else "<end>"
    nxt_req = req_roles[best_n] if best_n < len(req_roles) else "<end>"
    return (f"role sequences diverge at message {best_n}: stored {nxt_stored!r} "
            f"vs wanted {nxt_req!r} (request has {len(req_roles)} messages, "
            f"closest entry has {len(stored_roles)})")


class Cassette:
    """Append-only record/replay store for model responses."""

    def __init__(self, path: str, mode: str = "auto", index_gen: int = 0,
                 live=None, compact: bool = False, sample: str | None = None):
        if mode not in ("record", "replay-strict", "auto"):
            raise ValueError(f"unknown cassette mode: {mode!r}")
        self.path = path
        self.mode = mode
        self.index_gen = index_gen
        self.compact = compact
        self.sample = sample
        self._live = live
        self._lock = threading.Lock()
        self.entries = {}
        self.stats = {"hits": 0, "misses": 0, "recorded": 0}
        self._load()

    def set_sample(self, sample: str):
        """Name the current run (e.g. 'A-42') before recording or replaying.

        The harness derives each run's seed from this label. It must be set
        for every bench run: the bench builds the same first request for all
        seeds, so without a sample label every seed would share one recorded
        draw and the measured variance would be an artifact.
        """
        if not sample:
            raise ValueError("sample label must be non-empty")
        self.sample = sample

    # ── persistence ────────────────────────────────────────────────
    def _load(self):
        if not os.path.exists(self.path):
            return
        opener = gzip.open if self.path.endswith(".gz") else open
        with opener(self.path, "rt", encoding="utf-8") as f:
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
        line = json.dumps(entry, ensure_ascii=False) + "\n"
        if self.path.endswith(".gz"):
            # Deterministic gzip: mtime=0 AND filename="" (gzip embeds the
            # input file's name in its header when given one, which made
            # otherwise-identical tapes differ). Buffered through BytesIO,
            # then appended as a gzip member — concatenated members are valid
            # gzip and decompress to the full stream.
            buf = io.BytesIO()
            with gzip.GzipFile(filename="", fileobj=buf, mode="wb", mtime=0) as gz:
                gz.write(line.encode("utf-8"))
            with open(self.path, "ab") as raw:
                raw.write(buf.getvalue())
        else:
            with open(self.path, "a", encoding="utf-8") as f:
                f.write(line)

    # ── core ───────────────────────────────────────────────────────
    def respond(self, request: dict, timeout: int = 30) -> dict:
        """Return the response for `request`, replaying or recording.

        The live transport (`self._live`) has the signature
        `(request_body, timeout) -> response_dict` — the default is
        `llm_conn._pooled_chat`, which matches.
        """
        if self.mode != "record" and not self.sample:
            raise CassetteError(
                "[cassette] no sample label set. Call set_sample('A-42') "
                "before running; the sample names which stochastic draw the "
                "run represents and must match between record and replay.")
        key = request_key(request, self.index_gen, self.sample)
        with self._lock:
            entry = self.entries.get(key)
            if os.environ.get("RELIARY_CASSETTE_TRACE") == "1":
                n_msgs = len(request.get("messages") or [])
                sys.stderr.write(
                    f"[cassette-trace] sample={self.sample} msgs={n_msgs} "
                    f"key={key[:10]} {'HIT' if entry else 'MISS'}\n")
            # `record` always goes live (fresh tape, last write wins on load);
            # `auto` replays when present; `replay-strict` replays or raises.
            if entry is not None and self.mode != "record":
                self.stats["hits"] += 1
                return dict(entry["response"])
            if self.mode == "replay-strict":
                self.stats["misses"] += 1
                if os.environ.get("RELIARY_CASSETTE_DUMP"):
                    import pickle
                    with open(os.environ["RELIARY_CASSETTE_DUMP"], "wb") as f:
                        pickle.dump({"request": request, "sample": self.sample,
                                     "index_gen": self.index_gen,
                                     "key": key}, f)
                detail = _missing_diagnostic(request, self.entries, self.index_gen)
                if os.environ.get("RELIARY_CASSETTE_DEBUG") == "1":
                    detail = diagnose(request, self.entries, self.index_gen,
                                      self.sample)
                raise CassetteError(
                    f"[cassette] strict replay miss (key {key[:12]}…): {detail}")
            if self._live is None:
                raise CassetteError(
                    "[cassette] no live transport configured; cannot record")
            self.stats["misses"] += 1
            response = self._live(request, timeout)
            if isinstance(response, dict) and response.get("error"):
                # Never persist transport errors — they are not model decisions.
                return response
            fp = flip_risk(request, response)
            record = {
                "cassette_version": CASSETTE_VERSION,
                "key": key,
                "index_gen": self.index_gen,
                "sample": self.sample,
                "response": _strip_logprobs(response) if self.compact else response,
                "system_fingerprint": (response.get("system_fingerprint")
                                       if isinstance(response, dict) else None),
                "recorded_at": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
            }
            if self.compact:
                # Committed tapes drop the full message list: it grows to
                # 100K+ tokens per turn and would balloon the artifact. Store
                # per-message hashes instead of just roles so a replay miss
                # can name the exact turn whose content differs.
                record["request"] = {
                    "roles": [(m.get("role") if isinstance(m, dict) else "?")
                              for m in (request.get("messages") or [])],
                    "msg_hashes": [_msg_hash(m) for m in (request.get("messages") or [])],
                    "model": request.get("model"),
                    "max_tokens": request.get("max_tokens"),
                    "temperature": request.get("temperature"),
                }
            else:
                record["request"] = {k: request.get(k) for k in KEY_FIELDS
                                     if k in request}
            if fp is not None:
                record["flip_risk"] = fp
            self.entries[key] = record
            self._append(record)
            self.stats["recorded"] += 1
            return response


def _msg_hash(m) -> str:
    """Short hash of one message's role+content, for compact-tape diagnostics."""
    if not isinstance(m, dict):
        return ""
    blob = _canonical({"role": m.get("role"), "content": m.get("content")})
    return hashlib.sha256(blob.encode("utf-8")).hexdigest()[:16]


def diagnose(request: dict, entries: dict, index_gen: int, sample: str | None,
             max_report: int = 3) -> str:
    """Locate where a replay request diverges from the closest recorded turn.

    Compares the live message list against the stored entry with the longest
    shared prefix (content when stored, per-message hash when compact) and
    reports the first differing index with both sides' content. This is the
    only reliable locator: prefix-walking the live request would test message
    counts that were never sent (the conversation grows by two messages per
    turn, so odd-length prefixes have no recorded key).
    """
    msgs = request.get("messages") or []
    roles = [(m.get("role") if isinstance(m, dict) else "?") for m in msgs]
    hashes = [_msg_hash(m) for m in msgs]

    def stored_seq(entry):
        probe = entry.get("request", {})
        if probe.get("messages"):
            return probe["messages"], None
        return None, probe.get("msg_hashes")

    best, best_n, best_seq = None, -1, None
    for entry in entries.values():
        # Prefer entries from the same sample: without the filter, the closest
        # match can come from another seed's run, whose conversation diverges
        # for legitimate reasons (different explore-vs-answer decisions) and
        # would misreport where this run's divergence is.
        if sample is not None and entry.get("sample") not in (None, sample):
            continue
        bodies, hs = stored_seq(entry)
        seq = bodies if bodies is not None else hs
        if seq is None:
            continue
        n = 0
        for a, b in zip(seq, bodies if bodies is not None else hashes):
            if a != b:
                break
            n += 1
        if n > best_n:
            best, best_n, best_seq = entry, n, (bodies, hs)
    if best is None:
        return "no comparable entries"

    bodies, hs = best_seq
    if best_n >= len(msgs) and best_n >= (len(bodies) if bodies is not None else len(hs)):
        # Every message matched; the difference is a top-level key field.
        # Compare ALL of them, not a subset — a missing vs present field
        # changes the key just as much as a different value.
        probe = best.get("request", {})
        diffs = []
        if bodies is not None:
            for k in KEY_FIELDS:
                if k == "messages":
                    continue
                if probe.get(k) != request.get(k):
                    diffs.append(f"{k}: {probe.get(k)!r} -> {request.get(k)!r}")
        else:
            for k in ("model", "max_tokens", "temperature"):
                if probe.get(k) != request.get(k):
                    diffs.append(f"{k}: {probe.get(k)!r} -> {request.get(k)!r}")
            for k in ("thinking", "response_format", "logprobs", "top_logprobs"):
                stored_has = k in probe
                live_has = k in request
                if stored_has != live_has:
                    diffs.append(f"{k}: stored present={stored_has}, "
                                 f"live present={live_has}")
        if diffs:
            return "all messages match; key fields differ: " + "; ".join(diffs)
        if best.get("index_gen") != index_gen:
            return (f"all messages match but index_gen {best.get('index_gen')} != "
                    f"{index_gen}")
        return f"all messages and fields match (sample: {sample!r})"

    i = best_n
    role = roles[i] if i < len(roles) else "?"
    wanted = str(msgs[i].get("content")) if i < len(msgs) and isinstance(msgs[i], dict) else ""
    if bodies is not None and i < len(bodies):
        stored = str(bodies[i].get("content")) if isinstance(bodies[i], dict) else ""
        return (f"message {i} (role={role}) differs from the recorded turn.\n"
                f"      stored: {stored[:300]!r}\n"
                f"      wanted: {wanted[:300]!r}")
    prev = str(msgs[i - 1].get("content"))[-200:] if i > 0 else ""
    return (f"message {i} (role={role}) differs (compact tape stores hashes only).\n"
            f"      previous message tail: {prev!r}\n"
            f"      wanted content: {wanted[:300]!r}")


def _debug_tail(request: dict) -> str:
    """Dump the trailing messages so a divergence can be located exactly."""
    msgs = request.get("messages") or []
    tail = msgs[-4:]
    parts = []
    for m in tail:
        if not isinstance(m, dict):
            continue
        c = str(m.get("content"))
        parts.append(f"{m.get('role')}({len(c)}): {c[-160:]!r}")
    return "\n  tail: " + "\n        ".join(parts)


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


def _strip_logprobs(response: dict) -> dict:
    """Drop per-token logprobs from a stored response (compact tapes).

    Logprobs are ~20x the size of the tokens they annotate. The flip-risk
    summary captured at record time is what matters; the raw arrays are not
    needed for replay, so compact tapes omit them.
    """
    if not isinstance(response, dict):
        return response
    out = dict(response)
    choices = []
    for ch in response.get("choices") or []:
        ch2 = dict(ch)
        ch2.pop("logprobs", None)
        choices.append(ch2)
    out["choices"] = choices
    return out


# ── process-global wiring used by llm_conn ──────────────────────────

_ACTIVE = None


def active() -> "Cassette | None":
    return _ACTIVE


def _resolve_path(target: str) -> str:
    """Expand a cassette directory or explicit file path.

    A bare directory resolves to `<dir>/cassette.jsonl.gz` (gzip is the default
    for committed tapes: ~10x smaller, deterministic mtime=0). An explicit
    path ending in `.jsonl` or `.jsonl.gz` is used as-is.
    """
    if target.endswith(".jsonl") or target.endswith(".jsonl.gz"):
        return target
    return os.path.join(target, "cassette.jsonl.gz")


def _open_text(path: str):
    """Open a cassette file for reading, transparently handling gzip."""
    if path.endswith(".gz"):
        return gzip.open(path, "rt", encoding="utf-8")
    return open(path, encoding="utf-8")


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
    path = _resolve_path(target)
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
                       live=live_override or _pooled_chat,
                       compact=os.environ.get("RELIARY_CASSETTE_COMPACT") == "1")
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
    path = _resolve_path(argv[0])
    threshold = 1.0
    if "--threshold" in argv:
        threshold = float(argv[argv.index("--threshold") + 1])
    total = risky = 0
    worst = []
    with _open_text(path) as f:
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
