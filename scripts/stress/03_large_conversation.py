#!/usr/bin/env python3
"""Stress test 3: Large conversation through proxy."""
import json, urllib.request, time

PORT = 9090

def build_conversation(turns):
    msgs = [{"role": "user", "content": "Let me analyze this code carefully. I think we need to check the validate_config function first."}]
    for i in range(turns):
        msgs.append({"role": "assistant", "content": "Let me look at this bug. I can see the issue is in the threshold comparison. I will fix it by changing the operator. Based on my analysis, the problem is in the config validation logic. I think we should check edge cases first." * 10})
        msgs.append({"role": "user", "content": f"ok turn {i+1}"})
    return msgs

for turn_count in [10, 25, 50, 100]:
    msgs = build_conversation(turn_count)
    body = json.dumps({"model": "test", "messages": msgs})
    t0 = time.time()
    req = urllib.request.Request(f"http://localhost:{PORT}/v1/chat/completions",
        data=body.encode(),
        headers={"Content-Type": "application/json", "Authorization": "Bearer test-key"})
    try:
        urllib.request.urlopen(req, timeout=30)
        dt = time.time() - t0
        original_len = len(body)
        print(f"  {turn_count}turns: {dt:.2f}s (skipped — no route for test-key)")
    except Exception as e:
        dt = time.time() - t0
        print(f"  {turn_count}turns: {dt:.2f}s (expected 403: {str(e)[:30]})")
