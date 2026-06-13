#!/usr/bin/env python3
"""Stress test 9: Mutex lock contention on shared caches."""
import threading, socket, time

HOST = "127.0.0.1"
PORT = 9090
errors = 0
lock = threading.Lock()

def hammer_cache(thread_id):
    global errors
    for _ in range(100):
        try:
            s = socket.create_connection((HOST, PORT), timeout=5)
            # Mix read and write operations to hit risk_cache + read_cache
            s.sendall(f"risk /nonexistent/file_{thread_id}.rs\n".encode())
            s.recv(4096)
            s.close()
            s = socket.create_connection((HOST, PORT), timeout=5)
            s.sendall(f"cache-read /nonexistent/file_{thread_id}.rs abc123 100\n".encode())
            s.recv(4096)
            s.close()
        except Exception as e:
            with lock:
                errors += 1
                if errors <= 5:
                    print(f"  ERROR thread {thread_id}: {e}")

t0 = time.time()
threads = [threading.Thread(target=hammer_cache, args=(i,)) for i in range(20)]
for t in threads: t.start()
for t in threads: t.join()
dt = time.time() - t0
print(f"  20 threads x100 ops: {dt:.1f}s errors={errors}")
