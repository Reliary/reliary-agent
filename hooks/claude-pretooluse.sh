#!/bin/bash
# reliary PreToolUse hook for Claude Code
# Rewrites high-volume bash commands (git, cargo, pytest, ls, grep, etc.)
# to pipe through `reliary wrap` for sift compression — the RTK pattern.
#
# Opt-in via RELIARY_SIFT_BASH=1. Default OFF (preserves prior behavior).
#
# Skip rewrite if:
#   - RELIARY_SIFT_BASH != 1
#   - command contains shell chaining (|, >, <, &&, ;, ||, $(, `)
#   - command contains --no-sift
#   - command is a piped-through command (already starts with `reliary wrap`)
#
# Cache safety: identical command → identical compressed output (sift
# pipeline is now deterministic + first-appearance freeze at MCP layer).

if ! command -v jq >/dev/null 2>&1; then
  exit 0
fi

input=$(cat)
tool_name=$(echo "$input" | jq -r '.tool_name // empty')
cmd=$(echo "$input" | jq -r '.tool_input.command // empty')

# Only intercept bash commands
if [ "$tool_name" != "bash" ]; then
  exit 0
fi

# Opt-in toggle. Default ON (RTK parity) — unset or "0" disables.
if [ "${RELIARY_SIFT_BASH:-1}" != "1" ]; then
  exit 0
fi

if [ -z "$cmd" ] || [ ${#cmd} -lt 4 ]; then
  exit 0
fi

# Reject commands with newlines (would break single-quote wrapping).
case "$cmd" in *$'\n'*) exit 0;; esac

# Skip if user already opted out at command level.
case "$cmd" in *"--no-sift"*) exit 0;; esac

# Skip if already wrapped.
case "$cmd" in *"reliary "*) exit 0;; esac

# Skip shell chaining — can't safely re-execute.
case "$cmd" in
  *"|"*) exit 0 ;;
  *">"*) exit 0 ;;
  *"<"*) exit 0 ;;
  *"&&"*) exit 0 ;;
  *";"*) exit 0 ;;
  *"||"*) exit 0 ;;
  *'$('*) exit 0 ;;
  *'`'*) exit 0 ;;
esac

# Find reliary binary (cached via marker file per session).
_CACHE_FILE="/tmp/reliary-bin-path-${PPID:-$$}"
RELIARY_BIN=""
if [ -f "$_CACHE_FILE" ]; then
  RELIARY_BIN=$(cat "$_CACHE_FILE" 2>/dev/null)
fi
if [ -z "$RELIARY_BIN" ]; then
  RELIARY_BIN="${RELIARY_BIN_PATH:-$(which reliary 2>/dev/null || which reliary-agent 2>/dev/null)}"
  if [ -n "$RELIARY_BIN" ]; then
    (set -C; echo "$RELIARY_BIN" > "$_CACHE_FILE") 2>/dev/null || true
  fi
fi
if ! [[ "$RELIARY_BIN" =~ ^[A-Za-z0-9_./-]+$ ]]; then
  exit 0
fi
if [ ! -x "$RELIARY_BIN" ]; then
  exit 0
fi
if [ -z "$RELIARY_BIN" ]; then
  exit 0
fi

# Extract first token (the program name) — strip leading env vars.
# Use awk to skip leading KEY=VAL pairs.
first_word=$(echo "$cmd" | awk '{
  for (i = 1; i <= NF; i++) {
    if ($i ~ /^[A-Z_][A-Z0-9_]*=/) continue
    print $i
    exit
  }
}')

# Decision table: which programs get wrapped?
should_rewrite=0
case "$first_word" in
  # Version control — high-volume output
  git|gh) should_rewrite=1 ;;
  # Build/test runners
  cargo|npm|yarn|pnpm|bun|cargo-binstall|rustc|gcc|clang|make|cmake|go|java|javac|dotnet|kotlinc|swiftc)
    should_rewrite=1 ;;
  # Test frameworks
  pytest|jest|vitest|mocha|playwright|cypress|rake|rspec|phpunit|tox|nose2|behave|pytest3)
    should_rewrite=1 ;;
  # Test frameworks (multi-word: "go test", "cargo test")
  "go test"|"cargo test")
    should_rewrite=1 ;;
  # Linters / formatters
  ruff|eslint|prettier|biome|black|isort|flake8|mypy|pylint|shellcheck|golangci-lint|rubocop|clippy-driver|rustfmt)
    should_rewrite=1 ;;
  # File operations — verbose on large dirs
  ls|tree|find|fd|rg|ag|grep|ack|delta|bat|less|more|head|tail|wc|du|df|stat|file|xxd)
    should_rewrite=1 ;;
  # Container / k8s
  docker|podman|kubectl|kubectx|minikube|helm|docker-compose|nerdctl)
    should_rewrite=1 ;;
  # Cloud / IaC
  aws|gcloud|az|terraform|pulumi|cd|terragrunt)
    should_rewrite=1 ;;
  # Diff tools
  diff|meld|vimdiff)
    should_rewrite=1 ;;
esac

if [ "$should_rewrite" -eq 0 ]; then
  exit 0
fi

escaped_cmd=$(printf '%s' "$cmd" | sed "s/'/'\\\\''/g")
new_cmd="'$RELIARY_BIN' wrap bash -c '$escaped_cmd'"
echo "{\"tool_input\":{\"command\":$(echo "$new_cmd" | jq -Rs .)}}"