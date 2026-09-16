"""Arc 28 Lever 6 — connection info module.

Single source of truth for LLM/MCP connection info.

Constraints (per user requirements):
- Direct DeepSeek (NOT api.reliary.dev, NOT deepinfra).
- Use Pi agent (per harness convention).
- Reliary MCP via stdio subprocess (no local daemon port 9090).

Verified working:
- api.deepseek.com key at $HOME/.local/share/opencode/auth.json
- Reliary binary at $HOME/src/reliary8/target/release/reliary
- Tokio index at /tmp/tokio-corpus/tokio/src/.reliary/index.sqlite
- Hyper index at /tmp/hyper-corpus/.reliary/index.sqlite
- Homonym fixture at $HOME/src/reliary8/bench/fixtures/homonyms.json
"""
import json
import os
import subprocess
import threading
import urllib.error
import urllib.request

try:
    import http.client
    import ssl
    _HAVE_HTTPC = True
except Exception:
    _HAVE_HTTPC = False


# W2: pooled HTTPS connection for the bench client. urllib.urlopen() opens a
# fresh TLS connection per call (~150-300ms handshake per turn); real agent
# clients (Pi/OpenCode) pool connections. This keeps the bench honest and
# removes the handshake from measured wall time. Falls back to urllib on any
# connection failure.
_CONN = None
_CONN_LOCK = threading.Lock()


def _pooled_chat(body: dict, timeout: int):
    global _CONN
    host = "api.deepseek.com"
    path = "/v1/chat/completions"
    payload = json.dumps(body).encode()
    headers = {
        "Content-Type": "application/json",
        "Authorization": f"Bearer {load_deepseek_key()}",
        "Connection": "keep-alive",
    }
    with _CONN_LOCK:
        for attempt in (1, 2):
            try:
                if _CONN is None:
                    _CONN = http.client.HTTPSConnection(
                        host, timeout=timeout,
                        context=ssl.create_default_context())
                _CONN.request("POST", path, body=payload, headers=headers)
                resp = _CONN.getresponse()
                data = resp.read()
                if resp.status >= 400:
                    _close_conn()
                    return {"error": data[:500].decode("utf-8", errors="replace"),
                            "status": resp.status}
                return json.loads(data)
            except Exception as e:
                _close_conn()
                if attempt == 2:
                    return {"error": str(e), "status": -1}
    return {"error": "unreachable", "status": -1}


def _close_conn():
    global _CONN
    try:
        if _CONN is not None:
            _CONN.close()
    except Exception:
        pass
    _CONN = None


# ───── Direct DeepSeek (NOT api.reliary.dev, NOT deepinfra) ─────

DEEPSEEK_BASE_URL = "https://api.deepseek.com/v1"
DEEPSEEK_MODEL = "deepseek-v4-flash"
DEEPSEEK_MODEL_PRO = "deepseek-v4-pro"

_AUTH_FILE = os.path.expanduser("~/.local/share/opencode/auth.json")


def load_deepseek_key() -> str:
    """Load direct DeepSeek API key from opencode auth.json."""
    with open(_AUTH_FILE) as f:
        d = json.load(f)
    return d["deepseek"]["key"]


# Key resolution: DEEPSEEK_API_KEY env var first, then opencode auth.json.
# (The old hardcoded fallback was removed — SECURITY: the key was exposed in
# repo history and revoked. See commit bb4c4e3.)
DEEPSEEK_API_KEY_FALLBACK = (
    os.environ.get("DEEPSEEK_API_KEY")
    or load_deepseek_key()
)


def deepseek_chat(messages, model=DEEPSEEK_MODEL, max_tokens=200,
                   temperature=0.0, timeout=30, disable_thinking=True):
    """Direct call to api.deepseek.com. No proxy, no daemon, no reliary.dev.

    disable_thinking=True sends `thinking: {type: "disabled"}` which makes
    v4-flash/v4-pro skip chain-of-thought and return clean JSON directly.
    Without this, the model burns 500-1000 tokens on reasoning before any
    visible output — fatal for small `max_tokens` budgets.

    V75: when RELIARY_CASSETTE is set, responses come from a record/replay
    cassette (bench/cassette.py). Replay is byte-exact and makes zero API
    calls; strict mode errors on a miss instead of silently going live.
    """
    body = {
        "model": model,
        "messages": messages,
        "max_tokens": max_tokens,
        "temperature": temperature,
        "stream": False,
    }
    if disable_thinking:
        body["thinking"] = {"type": "disabled"}
    # V75: replay/record through the cassette when one is configured.
    try:
        from cassette import active, configure_from_env
        cas = active() or configure_from_env()
    except Exception:
        cas = None
    if cas is not None:
        # logprobs is report-only (does not affect sampling) and lets the
        # cassette annotate near-tie decisions. Never streamed, so cheap.
        body["logprobs"] = True
        body["top_logprobs"] = 3
        return cas.respond(body, timeout=timeout)
    # W2: pooled connection first; urllib fallback keeps behavior identical
    # if http.client is unavailable or the pool errors twice.
    if _HAVE_HTTPC:
        return _pooled_chat(body, timeout)
    req = urllib.request.Request(
        f"{DEEPSEEK_BASE_URL}/chat/completions",
        data=json.dumps(body).encode(),
        headers={
            "Content-Type": "application/json",
            "Authorization": f"Bearer {load_deepseek_key()}",
        },
        method="POST",
    )
    try:
        with urllib.request.urlopen(req, timeout=timeout) as resp:
            return json.loads(resp.read())
    except urllib.error.HTTPError as e:
        return {"error": e.read()[:500].decode("utf-8", errors="replace"),
                "status": e.code}
    except Exception as e:
        return {"error": str(e), "status": -1}


# ───── Pi agent (per harness convention) ─────

PI_BIN = os.path.expanduser("~/.local/bin/pi")
PI_SETTINGS = os.path.expanduser("~/.pi/agent/settings.json")
PI_DISABLE_HEARTBEAT = "1"


def set_pi_packages(pkgs):
    """Mirror harness convention: mutate ~/.pi/agent/settings.json packages field."""
    base = os.path.expanduser("~/.pi/agent")
    with open(PI_SETTINGS) as f:
        d = json.load(f)
    d["packages"] = []
    for p in pkgs:
        if p.startswith("/"):
            d["packages"].append(p)
        else:
            d["packages"].append(os.path.relpath(p, base))
    with open(PI_SETTINGS, "w") as f:
        json.dump(d, f, indent=2)


# ───── Reliary binary + MCP dispatcher (no daemon port) ─────

RELIARY_BIN = "$HOME/src/reliary8/target/release/reliary"
TOKIO_CORPUS = "/tmp/tokio-corpus/tokio/src"
HYPER_CORPUS = "/tmp/hyper-corpus"
HOMONYMS_FIXTURE = "$HOME/src/reliary8/bench/fixtures/homonyms.json"


def mcp_call(tool, args, workdir, timeout=30):
    """Call a reliary MCP tool via stdio subprocess. No port 9090.

    Returns parsed JSON if the response text is JSON, else returns
    {"content": [{"text": <raw>}, {"type": "text"}]} so callers can
    distinguish grep-format strings from JSON objects.
    """
    req = json.dumps({"jsonrpc": "2.0", "id": 1, "method": "tools/call",
                      "params": {"name": tool, "arguments": args}})
    proc = subprocess.Popen([RELIARY_BIN, "mcp"], cwd=workdir,
                            stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                            stderr=subprocess.PIPE)
    try:
        out, _ = proc.communicate(req.encode(), timeout=timeout)
    except subprocess.TimeoutExpired:
        proc.kill()
        return {}
    for line in out.decode().splitlines():
        try:
            r = json.loads(line)
        except Exception:
            continue
        if "result" not in r:
            continue
        content = r["result"].get("content", [])
        if not content:
            return {}
        text = content[0].get("text", "")
        # If text is JSON, parse it; otherwise return raw content wrapper.
        try:
            return json.loads(text)
        except Exception:
            return {"content": content}
    return {}


# ───── Smoke test ─────

if __name__ == "__main__":
    print("=== Arc 28 Lever 6 — Connection Smoke Test ===\n")

    # 1. DeepSeek direct.
    print("[1] Direct DeepSeek call:")
    r = deepseek_chat([{"role": "user", "content": "Reply with the word OK"}], max_tokens=5)
    if "error" in r:
        print(f"    ERROR: {r['error']}")
    else:
        msg = r.get("choices", [{}])[0].get("message", {}).get("content", "(none)")
        usage = r.get("usage", {})
        cached = usage.get("prompt_tokens_details", {}).get("cached_tokens", 0)
        print(f"    OK: '{msg}' | prompt={usage.get('prompt_tokens')}, "
              f"completion={usage.get('completion_tokens')}, "
              f"cached={cached}, "
              f"model={r.get('model')}")

    # 2. Reliary MCP via stdio.
    print("\n[2] Reliary MCP via stdio (reliary_find_references):")
    r = mcp_call("reliary_find_references_type_flow",
                  {"name": "clone", "anchor_file": "sync/watch.rs",
                   "anchor_line": 201, "path": TOKIO_CORPUS, "threshold": 0.1},
                  workdir=TOKIO_CORPUS)
    n_hits = len(r.get("hits", []))
    print(f"    OK: {n_hits} hits, method={r.get('method', '?')}")

    # 3. Reliary query_ast.
    print("\n[3] Reliary MCP query_ast:")
    r = mcp_call("reliary_query_ast",
                  {"pattern": "Call(_, _)", "file": f"{TOKIO_CORPUS}/sync/watch.rs",
                   "max_results": 5},
                  workdir=TOKIO_CORPUS)
    n_matches = len(r.get("matches", []))
    print(f"    OK: total={r.get('total', 0)}, returned={n_matches}")

    # 4. Pi binary check.
    print("\n[4] Pi binary:")
    print(f"    path={PI_BIN}, exists={os.path.exists(PI_BIN)}")
    print(f"    settings={PI_SETTINGS}, exists={os.path.exists(PI_SETTINGS)}")

    # 5. Fixture load.
    print("\n[5] Homonym fixture:")
    with open(HOMONYMS_FIXTURE) as f:
        data = json.load(f)
    n_anchors = len(data.get("anchors", []))
    n_unbench = sum(1 for a in data["anchors"]
                    if a.get("audit_status") == "unbenchable")
    print(f"    {n_anchors} anchors total, {n_unbench} unbenchable")
    print(f"    corpus={data.get('corpus')}")

    print("\n=== Smoke test complete ===")