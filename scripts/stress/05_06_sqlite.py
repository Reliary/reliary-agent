#!/usr/bin/env python3
"""Stress test 5+6: SQLite concurrent read/write + corruption recovery."""
import sqlite3, threading, os, time

DB_PATH = "/tmp/stress_sqlite_test.db"
errors = 0
lock = threading.Lock()

def writer(thread_id):
    global errors
    for _ in range(50):
        try:
            db = sqlite3.connect(DB_PATH, timeout=5)
            db.execute("INSERT INTO chronicle (t, event, file, detail, outcome) VALUES (?, ?, ?, ?, ?)",
                      (int(time.time()), "test", f"file_{thread_id}.rs", "stress", "ok"))
            db.commit()
            db.close()
        except Exception as e:
            with lock: errors += 1; print(f"  WRITE ERROR: {e}")

def reader(thread_id):
    global errors
    for _ in range(50):
        try:
            db = sqlite3.connect(DB_PATH, timeout=5)
            db.execute("SELECT COUNT(*) FROM chronicle WHERE file = ?", (f"file_{thread_id}.rs",))
            db.close()
        except Exception as e:
            with lock: errors += 1; print(f"  READ ERROR: {e}")

# Init DB with WAL
db = sqlite3.connect(DB_PATH)
db.executescript("PRAGMA journal_mode = WAL; PRAGMA synchronous = NORMAL; CREATE TABLE IF NOT EXISTS chronicle (id INTEGER PRIMARY KEY, t INTEGER, event TEXT, file TEXT, detail TEXT, outcome TEXT);")
db.close()

t0 = time.time()
threads = []
for i in range(5):
    t = threading.Thread(target=writer, args=(i,))
    threads.append(t)
    t.start()
for i in range(5):
    t = threading.Thread(target=reader, args=(i,))
    threads.append(t)
    t.start()
for t in threads: t.join()
dt = time.time() - t0
print(f"  Concurrent 5W/5R x50: {dt:.1f}s errors={errors}")

# Corruption recovery test
print("  Testing corruption recovery...")
with open(DB_PATH, "r+b") as f:
    f.seek(100)
    f.write(b'\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00')
try:
    db2 = sqlite3.connect(DB_PATH, timeout=5)
    r = db2.execute("SELECT COUNT(*) FROM chronicle").fetchone()
    print(f"  Corrupted DB query: {'RECOVERED' if r else 'EMPTY'}")
    db2.close()
except Exception as e:
    print(f"  Corruption handling: {e} (expected — DB was corrupted)")

os.remove(DB_PATH)
