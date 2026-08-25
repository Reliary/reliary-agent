// reliary PreToolUse plugin for OpenCode
// Rewrites HIGH-VOLUME bash commands (git, cargo, pytest, ls, grep, etc.)
// to pipe through `reliary wrap` for sift compression — the RTK pattern.
//
// Default ON (RTK parity). Disable with RELIARY_SIFT_BASH=0.
//
// Skip rewrite if:
//   - RELIARY_SIFT_BASH == 0
//   - command contains shell chaining (|, >, <, &&, ;, ||, $(, `)
//   - command contains --no-sift
//   - command is a piped-through command (already starts with `reliary wrap`)
//
// Cache safety: identical command → identical compressed output (sift
// pipeline is now deterministic + first-appearance freeze at MCP layer).

const fs = require("fs");

// C4: validate RELIARY_BIN against a strict safe-path charset before splicing it
// into the rewritten bash command. Prevents RCE if RELIARY_BIN_PATH or which output
// contains shell metacharacters.
const SAFE_BIN_RE = /^[A-Za-z0-9_./-]+$/;

function isValidBin(bin) {
  return typeof bin === "string" && SAFE_BIN_RE.test(bin);
}

function isValidBinWithDiagnostic(bin) {
  if (typeof bin !== "string") return false;
  if (!SAFE_BIN_RE.test(bin)) {
    console.error("[reliary-sift] rejected binary path (contains unsafe chars):", bin);
    return false;
  }
  return true;
}

// H13: walk PATH directly instead of spawning 'which' subprocess.
function findReliary() {
  if (process.env.RELIARY_BIN_PATH) {
    const p = process.env.RELIARY_BIN_PATH.trim();
    if (!isValidBinWithDiagnostic(p)) return null;
    try { fs.accessSync(p, fs.constants.X_OK); return p; } catch { return null; }
  }
  const pathDirs = (process.env.PATH || "").split(":");
  for (const name of ["reliary", "reliary-agent"]) {
    for (const dir of pathDirs) {
      const candidate = dir ? dir + "/" + name : name;
      if (!isValidBin(candidate)) continue;
      try { fs.accessSync(candidate, fs.constants.X_OK); return candidate; } catch { }
    }
  }
  return null;
}

// Programs that benefit from sift compression (RTK parity list).
const REWRITE_PROGRAMS = new Set([
  // Version control
  "git", "gh",
  // Build/test
  "cargo", "npm", "yarn", "pnpm", "bun", "cargo-binstall", "rustc", "gcc", "clang",
  "make", "cmake", "go", "java", "javac", "dotnet", "kotlinc", "swiftc",
  // Test frameworks
  "pytest", "pytest3", "jest", "vitest", "mocha", "playwright", "cypress",
  "rake", "rspec", "phpunit", "tox", "nose2", "behave",
  // Linters / formatters
  "ruff", "eslint", "prettier", "biome", "black", "isort", "flake8", "mypy",
  "pylint", "shellcheck", "golangci-lint", "rubocop", "clippy-driver", "rustfmt",
  // File ops
  "ls", "tree", "find", "fd", "rg", "ag", "grep", "ack", "delta", "bat",
  "less", "more", "head", "tail", "wc", "du", "df", "stat", "file", "xxd",
  // Container / k8s
  "docker", "podman", "kubectl", "kubectx", "minikube", "helm",
  "docker-compose", "nerdctl",
  // Cloud / IaC
  "aws", "gcloud", "az", "terraform", "pulumi", "terragrunt",
  // Diff
  "diff", "meld", "vimdiff",
]);

// Strip leading KEY=VAL env vars to find the first program token.
function firstToken(cmd) {
  const tokens = cmd.split(/\s+/);
  for (const t of tokens) {
    if (!/^[A-Z_][A-Z0-9_]*=/.test(t)) return t;
  }
  return "";
}

module.exports = function (opencode) {
  const SIFT_BASH = process.env.RELIARY_SIFT_BASH !== "0"; // V14: default ON (RTK parity)
  const RELIARY_BIN = findReliary();

  if (!RELIARY_BIN) {
    console.error("[reliary-sift] disabled: binary not found or unsafe path");
    return;
  }

  opencode.on("tool.execute.before", (event) => {
    if (!SIFT_BASH) return;
    if (event.tool !== "bash") return;
    const cmd = event.input?.command || "";
    if (!cmd || cmd.length < 4) return;
    if (cmd.includes("\n")) return;
    // Skip if user opted out at command level
    if (cmd.includes("--no-sift")) return;
    // Skip if already wrapped
    if (cmd.includes("reliary ")) return;
    // Skip shell chaining — can't safely re-execute
    if (/[|<>&;]|`|\$\(/.test(cmd)) return;

    const first = firstToken(cmd);
    if (!REWRITE_PROGRAMS.has(first)) return;

    const escaped_cmd = cmd.includes("'") ? cmd.replace(/'/g, "'\\''") : cmd;
    event.input.command = `${RELIARY_BIN} wrap bash -c '${escaped_cmd}'`;
  });
};