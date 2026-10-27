#!/usr/bin/env python3
"""Known-item retrieval benchmark: symbol name -> the file that defines it.

Ground truth comes from universal-ctags, never from the tool. Questions are
definition names that occur in exactly one source file, so there is exactly one
right answer and the metric is not gameable by keyword stuffing.

Measures:
  recall@k  -- is the gold file in the top k
  mrr       -- 1/rank of the gold file
  bytes     -- mean answer size (cost proxy)

Deliberately has no LLM in the loop: deterministic, free, re-runnable.
"""
import argparse, json, os, subprocess, sys, collections

BIN = os.environ.get("RELIARY_BIN", "reliary")

def load_gt(tagfile, corpus, exts):
    """symbol -> file, keeping only symbols with ONE unambiguous gold path.

    Gold paths are resolved against the FILESYSTEM, never the index: grading
    the index against itself would be circular. An entry is dropped when its
    path does not exist on disk or when its basename is not unique in the
    corpus (a bare `__init__.py` gold matches any `__init__.py`, so a scorer
    with an `endswith` fallback makes those queries unfailable — that inflated
    `requests` to 0.992 against a 54%-broken key).
    """
    root = os.path.abspath(corpus)
    per = collections.defaultdict(set)
    for line in open(tagfile, errors="ignore"):
        if line.startswith("!_"): continue
        p = line.rstrip("\n").split("\t")
        if len(p) < 4: continue
        if p[3].split(":")[0] not in ("function", "func", "class", "method",
                                      "interface", "struct", "typedef", "trait",
                                      "implementation", "module", "type"): continue
        num = "".join(c for c in p[2] if c.isdigit())
        if not num: continue
        f = p[1]
        base = os.path.basename(corpus)
        i = f.find("/" + base + "/")
        if i >= 0: f = f[i + len(base) + 2:]
        while f.startswith("./"): f = f[2:]
        if not f.endswith(tuple(exts.split(","))): continue
        if f.startswith(("docs/", "test", "tests/", "bench")): continue
        per[p[0]].add(f)

    # basename multiplicity across the whole corpus, so "unique" is measured
    # against real siblings, not against the sample.
    basenames = collections.Counter()
    for dirpath, _dirs, files in os.walk(root):
        for fn in files:
            if fn.endswith(tuple(exts.split(","))):
                basenames[fn] += 1

    kept, dropped = {}, collections.Counter()
    for sym, fs in per.items():
        if len(fs) != 1:
            dropped["defined in >1 file"] += 1
            continue
        f = next(iter(fs))
        if not os.path.exists(os.path.join(root, f)):
            dropped["gold path does not exist"] += 1
            continue
        if basenames[os.path.basename(f)] > 1:
            dropped["basename not unique"] += 1
            continue
        kept[sym] = f
    return kept, dropped

def call_all(corpus, queries, limit):
    """One MCP process, one JSON-RPC call per query."""
    lines = [json.dumps({"jsonrpc":"2.0","id":1,"method":"initialize","params":{
        "protocolVersion":"2024-11-05","capabilities":{},
        "clientInfo":{"name":"bench","version":"1"}}}),
        json.dumps({"jsonrpc":"2.0","method":"notifications/initialized"})]
    for i, q in enumerate(queries):
        lines.append(json.dumps({"jsonrpc":"2.0","id":i+2,"method":"tools/call",
            "params":{"name":"reliary_search","arguments":{
                "query":q,"path":".","limit":limit}}}))
    out = subprocess.run([BIN,"mcp"], cwd=corpus, input="\n".join(lines),
                         capture_output=True, text=True)
    res = {}
    for line in out.stdout.splitlines():
        try: o = json.loads(line)
        except Exception: continue
        i = o.get("id")
        if not isinstance(i, int) or i < 2: continue
        try: text = o["result"]["content"][0]["text"]
        except Exception: text = ""
        res[queries[i-2]] = text
    return res

def paths_in(text):
    """Extract file paths from the search answer, in rank order.

    The answer is a JSON array of {"file": ...}; fall back to token scanning
    when it is prose. Paths are normalized to repo-relative so they compare
    against the ctags ground truth.
    """
    out, seen = [], set()
    # A search answer may lead with a definition hint line
    # ("X is defined at f:12"), added so a model that picks `search` for a
    # symbol question still sees the definition. The file list follows it.
    lines = text.split("\n")
    hint = None
    if lines and " is defined at " in lines[0]:
        # "X is defined at path:LINE" is rank 1; the file list follows without
        # repeating it. Parse the path out so scoring still sees it.
        tail = lines[0].rsplit(" is defined at ", 1)[1]
        if ":" in tail:
            hint = tail.rsplit(":", 1)[0]
        lines = lines[1:]
    body = "\n".join(lines)
    try:
        arr = json.loads(body.split("\n")[0])
        for o in arr:
            f = o.get("file") if isinstance(o, dict) else None
            if f and f not in seen:
                seen.add(f); out.append(f)
    except Exception:
        for tok in body.split():
            t = tok.strip("`'\"[],:;")
            if "/" in t:
                if t not in seen:
                    seen.add(t); out.append(t)
    if hint:
        out = [hint] + [p for p in out if p != hint]
    return out

def normalize(path, corpus):
    """repo-relative, no leading `./`.

    `str.lstrip("./")` is a CHARACTER SET, not a prefix: it turns
    `/home/...` into `home/...` and silently breaks every comparison. Use
    explicit prefix removal.
    """
    p = path
    root = os.path.abspath(corpus)
    if p.startswith(root + os.sep):
        p = p[len(root) + 1:]
    while p.startswith("./"):
        p = p[2:]
    if p.startswith("/"):
        p = p[1:]
    return p

def score(res, gt, k, corpus=""):
    hit = mrr = 0.0; r1 = 0; gold_hits = 0; bytes_ = 0
    for q, gold in gt.items():
        text = res.get(q, "")
        bytes_ += len(text)
        ps = [normalize(x, corpus) for x in paths_in(text)]
        gold = normalize(gold, corpus)
        rank = None
        for n, p in enumerate(ps, 1):
            if p == gold:
                rank = n; break
        if rank is not None:
            gold_hits += 1
            mrr += 1.0 / rank
            if rank == 1: r1 += 1
            if rank <= k: hit += 1
    n = max(len(gt), 1)
    return {"n": len(gt), "recall@k": hit/n, "mrr": mrr/n, "recall@1": r1/n,
            "mean_bytes": bytes_/n}

def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--corpus", required=True)
    ap.add_argument("--tags", required=True)
    ap.add_argument("--exts", default=".rs")
    ap.add_argument("--limit", type=int, default=8)
    ap.add_argument("--max", type=int, default=120)
    ap.add_argument("--seed", type=int, default=7)
    ap.add_argument("--out")
    a = ap.parse_args()

    gt_all, dropped = load_gt(a.tags, a.corpus, a.exts)
    # G0.1: every retained gold must resolve. This is an assertion, not a rate.
    missing = [s for s, f in gt_all.items()
               if not os.path.exists(os.path.join(os.path.abspath(a.corpus), f))]
    assert not missing, f"gold paths do not resolve: {missing[:3]}"
    syms = sorted(gt_all)
    # deterministic sample, no dependence on run order
    import random
    rnd = random.Random(a.seed)
    rnd.shuffle(syms)
    syms = syms[: a.max]
    gt = {s: gt_all[s] for s in syms}

    res = call_all(os.path.abspath(a.corpus), syms, a.limit)
    m = score(res, gt, a.limit, a.corpus)
    print(f"corpus={a.corpus} valid_gt={len(gt_all)} queried={m['n']} limit={a.limit}")
    if dropped:
        print("  dropped golds: " + ", ".join(f"{k}={v}" for k, v in dropped.most_common()))
    print(f"  recall@{a.limit}={m['recall@k']:.3f}  recall@1={m['recall@1']:.3f} "
          f" mrr={m['mrr']:.3f}  mean_bytes={m['mean_bytes']:.0f}")
    if a.out:
        json.dump({"summary": m, "queries": {s: {"gold": gt[s], "answer": res.get(s,"")} for s in syms}},
                  open(a.out, "w"), indent=1)
        print(f"  wrote {a.out}")
    return 0

if __name__ == "__main__":
    sys.exit(main())
