#!/usr/bin/env python3
"""Cross-language benchmark: Reliary vs ALTBACKEND on a Nix repo.

ALTBACKEND uses tree-sitter which doesn't support Nix (only mainstream languages).
Reliary is grammar-free and works on any text.
"""
import subprocess, json, os, time, re, sys

RELIARY = '/home/user/src/reliary8/target/release/reliary'
ALTBACKEND = '/home/user/.local/bin/altbackend-mcp'
REPO = '/home/user/src/flake'

QUERIES = [
    'version', 'buildPhase', 'mkDerivation', 'stdenv', 'lib', 'config', 'name',
    'description', 'src', 'meta', 'platforms', 'license', 'shellHook',
]

def query_reliary(name, path):
    proc = subprocess.Popen([RELIARY, 'mcp'],
        stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL,
        env={**os.environ, 'NO_RELIARY_WATCHER': '1'})
    try:
        def w(d): proc.stdin.write((json.dumps(d)+'\n').encode()); proc.stdin.flush()
        w({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"x","version":"1"}}})
        proc.stdout.readline()
        w({"jsonrpc":"2.0","method":"notifications/initialized"})
        t0 = time.time()
        w({"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"reliary_find_references_with_source","arguments":{"name":name,"path":path,"format":"grep","limit":10,"threshold":0.05}}})
        resp = proc.stdout.readline().decode()
        elapsed = time.time() - t0
        res = json.loads(resp)
        text = res['result']['content'][0].get('text','')
        lines = [l for l in text.split('\n') if l.strip() and ':' in l and 'Use the' not in l]
        nix_hits = [l for l in lines if '.nix' in l and 'flake.lock' not in l]
        return {'hits': len(lines), 'nix_hits': len(nix_hits), 'time': elapsed}
    finally:
        proc.terminate()
        try: proc.wait(timeout=2)
        except: proc.kill()

def query_altbackend(name, project):
    proc = subprocess.Popen([ALTBACKEND],
        stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL)
    try:
        def w(d): proc.stdin.write((json.dumps(d)+'\n').encode()); proc.stdin.flush()
        w({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"x","version":"1"}}})
        time.sleep(2); proc.stdout.readline()
        w({"jsonrpc":"2.0","method":"notifications/initialized"})
        time.sleep(1)
        t0 = time.time()
        w({"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"search_graph","arguments":{"project":project,"query":name,"limit":10}}})
        resp = proc.stdout.readline().decode()
        elapsed = time.time() - t0
        res = json.loads(resp)
        text = res['result']['content'][0].get('text','')
        d = json.loads(text) if text.startswith('{') else {'results': []}
        nix_hits = [r for r in d.get('results',[]) if '.nix' in r.get('file_path','')]
        return {'hits': d.get('total', 0), 'nix_hits': len(nix_hits), 'time': elapsed}
    finally:
        proc.terminate()
        try: proc.wait(timeout=2)
        except: proc.kill()

def main():
    print(f"=== Cross-language benchmark: {REPO} ===\n")
    print(f"{'Query':<20} {'Reliary hits (.nix)':<25} {'ALTBACKEND hits (.nix)':<20} {'Winner'}")
    print("-" * 90)
    r_total, c_total, r_wins, c_wins, ties = 0, 0, 0, 0, 0
    for q in QUERIES:
        try:
            r = query_reliary(q, REPO)
        except Exception as e:
            r = {'hits': 0, 'nix_hits': 0, 'time': 0, 'error': str(e)}
        try:
            c = query_altbackend(q, 'home-user-src-flake')
        except Exception as e:
            c = {'hits': 0, 'nix_hits': 0, 'time': 0, 'error': str(e)}
        winner = 'Reliary' if r['nix_hits'] > c['nix_hits'] else ('ALTBACKEND' if c['nix_hits'] > r['nix_hits'] else 'Tie')
        if winner == 'Reliary': r_wins += 1
        elif winner == 'ALTBACKEND': c_wins += 1
        else: ties += 1
        r_total += r['nix_hits']
        c_total += c['nix_hits']
        print(f"{q:<20} {str(r['nix_hits']) + ' (' + str(r['hits']) + ')':<25} {str(c['nix_hits']) + ' (' + str(c['hits']) + ')':<20} {winner}")
    print("-" * 90)
    print(f"{'TOTAL':<20} {r_total:<25} {c_total:<20}")
    print(f"\nWins: Reliary={r_wins}, ALTBACKEND={c_wins}, Ties={ties}")
    print(f"Reliary advantage: {r_total - c_total} more .nix hits")
    if c_total == 0 and r_total > 0:
        print(f"\nALTBACKEND returned ZERO .nix hits across {len(QUERIES)} queries.")
        print(f"Reliary found {r_total} .nix hits (grammar-free, works on any text).")

if __name__ == '__main__':
    main()
