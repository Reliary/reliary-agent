#!/usr/bin/env python3
"""Stress test 1: TCP concurrent connections via HTTP on :9090."""
import urllib.request, urllib.error, threading, time

PORT = 9090
results = {"ok": 0, "err": 0}
lock = threading.Lock()

def http_get(path, expect):
    try:
        r = urllib.request.urlopen(f"http://127.0.0.1:{PORT}{path}", timeout=10)
        body = r.read().decode().strip()
        with lock:
            if expect in body:
                results["ok"] += 1
            else:
                print(f"  UNEXPECTED: expected '{expect}', got '{body[:60]}'")
                results["err"] += 1
    except Exception as e:
        with lock:
            results["err"] += 1
            if results["err"] <= 5:
                print(f"  ERR: {e}")

# Test 1: burst 50 concurrent HTTP GET /ping
threads = [threading.Thread(target=http_get, args=("/ping", "pong")) for _ in range(50)]
for t in threads: t.start()
for t in threads: t.join()
print(f"  Burst 50 /ping: ok={results['ok']} err={results['err']}")

# Test 2: 30 malformed requests
threads = []
def malformed():
    import socket
    try:
        s = socket.create_connection(("127.0.0.1", PORT), timeout=5)
        s.sendall(b"GET / HTTP/1.0\r\nContent-Length: -1\r\n\r\n\x00\x01\x02")
        s.recv(4096)
        s.close()
        with lock: results["ok"] += 1
    except:
        with lock: results["err"] += 1
for _ in range(30):
    t = threading.Thread(target=malformed)
    threads.append(t)
    t.start()
for t in threads: t.join()
print(f"  Malformed 30: ok={results['ok']} err={results['err']}")
print(f"  Total: ok={results['ok']} err={results['err']}")
