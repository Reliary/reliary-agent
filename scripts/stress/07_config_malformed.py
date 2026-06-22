#!/usr/bin/env python3
"""Stress test 7: Malformed config cascade."""
import json, os, tempfile, subprocess, shutil

RELIARY = os.path.join(os.environ.get("REPO_ROOT", os.path.expanduser("~/src/reliary-agent")), "target", "release", "reliary-agent")
RELIARY_DIR = os.path.expanduser("~/.reliary")
errors = 0

def test_config(name, config_content, expect_startup=True):
    global errors
    cfg_path = os.path.join(RELIARY_DIR, "config.json")
    backup = None
    if os.path.exists(cfg_path):
        with open(cfg_path) as f: backup = f.read()
    with open(cfg_path, "w") as f:
        f.write(config_content)
    r = subprocess.run([RELIARY, "doctor"], capture_output=True, timeout=10)
    out = r.stdout.decode() + r.stderr.decode()
    if expect_startup and "ERROR" in out[:200]:
        print(f"  FAIL {name}: unexpected error: {out[:100]}")
        errors += 1
    elif not expect_startup and "ERROR" not in out[:200]:
        print(f"  UNEXPECTED OK {name}: should have errored")
        errors += 1
    else:
        print(f"  OK {name}")
    if backup:
        with open(cfg_path, "w") as f: f.write(backup)

# Tests
test_config("empty file", "")
test_config("invalid JSON", "{broken")
test_config("wrong features type", '{"features": "not_an_object"}')
test_config("unknown feature", '{"features": {"notAFeature": false}}')
test_config("valid config", '{"mode": "fast", "features": {"compress": true, "convWindow": false}}')

print(f"  Total errors: {errors}")
