#!/usr/bin/env python3
"""Run all stress tests in sequence."""
import subprocess, time, sys, os

SCRIPTS = [
    ("01 TCP concurrent", "01_tcp_concurrent.py", True),
    ("03 Large conversation", "03_large_conversation.py", False),
    ("04 Heal-apply cycle", "04_heal_cycle.py", False),
    ("05-06 SQLite", "05_06_sqlite.py", False),
    ("07 Config malformed", "07_config_malformed.py", True),
    ("09 Mutex contention", "09_mutex_contention.py", True),
]

for name, script, needs_daemon in SCRIPTS:
    # Start daemon for tests that need it
    if needs_daemon:
        subprocess.run(["pkill", "-f", "reliary-agent"], capture_output=True)
        subprocess.Popen([os.path.join(os.environ.get("REPO_ROOT", os.path.expanduser("~/src/reliary-agent")), "target", "release", "reliary-agent"), "serve"],
                        stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        import time as _t; _t.sleep(3)

    print(f"\n=== {name} ===")
    t0 = time.time()
    r = subprocess.run([sys.executable, os.path.join(os.path.dirname(__file__), script)],
                      capture_output=True, timeout=300)
    dt = time.time() - t0
    out = r.stdout.decode()
    print(out[:1000])
    if r.returncode != 0:
        print(f"  EXIT CODE: {r.returncode}")
    print(f"  ({dt:.0f}s)")

print("\n=== All stress tests complete ===")
