#!/usr/bin/env python3
"""Stress test 4: Heal-apply continuous edit cycle."""
import subprocess, tempfile, os, time

RELIARY = "/home/john/src/reliary-agent/target/release/reliary-agent"
TEST_FILE = "/tmp/stress_heal_test.rs"

# Create a simple test file and cargo project
os.makedirs("/tmp/stress_heal_project/src", exist_ok=True)
with open("/tmp/stress_heal_project/Cargo.toml", "w") as f:
    f.write('[package]\nname = "stress"\nversion = "0.1.0"\nedition = "2021"\n')
with open("/tmp/stress_heal_project/src/lib.rs", "w") as f:
    f.write("pub fn add(a: i32, b: i32) -> i32 { a + b }\n#[test]\nfn test_add() { assert_eq!(add(2, 2), 4); }\n")

# Pre-build the project
subprocess.run(["cargo", "test"], cwd="/tmp/stress_heal_project", capture_output=True)

success = 0
reverted = 0
errors = 0
t0 = time.time()

for i in range(100):
    # Write a known-good edit
    content = f"pub fn add(a: i32, b: i32) -> i32 {{ a + b }}\n#[test]\nfn test_add() {{ assert_eq!(add({i}, {i}), {i*2}); }}\n"
    with open(TEST_FILE, "w") as f:
        f.write(content)
    
    r = subprocess.run([RELIARY, "apply-edit", "/tmp/stress_heal_project/src/lib.rs", TEST_FILE, "/tmp/stress_heal_project"],
                      capture_output=True, timeout=30)
    out = r.stdout.decode()
    if "OK" in out:
        success += 1
    elif "REVERTED" in out:
        reverted += 1
    else:
        errors += 1
        if errors <= 3:
            print(f"  FAIL cycle {i}: {out[:60]}")

dt = time.time() - t0
print(f"  100 cycles: {dt:.1f}s avg={dt/100:.2f}s/cycle")
print(f"  success={success} reverted={reverted} errors={errors}")

# Cleanup
subprocess.run(["rm", "-rf", "/tmp/stress_heal_project", TEST_FILE])
