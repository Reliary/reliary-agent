"""Shared helpers for benchmark scripts.

Provides a single source of truth for the working-directory CWD prepending
that prevents the LLM from defensively adding `cd /root` to bash commands.
"""
import os
import subprocess


def cwd_prefix(repo: str) -> str:
    """Return a working-directory prefix to inject into the FIRST user prompt.

    The LLM in a fresh --print session sometimes pre-pends `cd /root` to bash
    commands, breaking the bench. Pi's system prompt includes the CWD, but
    explicit re-instruction in the first user prompt is more reliable.

    Apply only to turn 0 of each session.
    """
    return (
        f"Working directory: {repo}\n"
        f"Do not add `cd` to bash commands — the working directory is already set.\n\n"
    )


def run_turn(pi_bin: str, session_file: str, prompt: str, env: dict,
             repo: str, timeout: int = 600, model: str = "deepseek/deepseek-v4-flash",
             first_turn: bool = False) -> subprocess.CompletedProcess:
    """Run a single Pi turn, prepending CWD instruction to the first turn."""
    if first_turn:
        prompt = cwd_prefix(repo) + prompt
    return subprocess.run(
        [pi_bin, "--model", model,
         "--mode", "json", "--session", session_file, "--print", prompt],
        capture_output=True, text=True, timeout=timeout, env=env, cwd=repo,
    )
