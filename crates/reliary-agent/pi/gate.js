// reliary gate.js v0.8.0 — THIN Pi adapter.
//
// What this shim does:
//   - Discover the reliary binary.
//   - After edit/write, trigger background FTS5 reindex.
//   - When RELIARY_SIFT_BASH=1, intercept bash tool calls and pipe output through
//     `reliary wrap` (sift compression). Compresses tool output BEFORE the LLM
//     sees it — rtk's proven pattern. No cache bust because the LLM builds
//     reasoning on compressed text from the start.
//   - Log turn count for diagnostics.

const { execFileSync, spawnSync } = require("child_process");
const { existsSync, writeFileSync, unlinkSync } = require("fs");
const { tmpdir } = require("os");
const { join } = require("path");

const GATE_VERSION = "0.8.0";

const SIFT_BASH = process.env.RELIARY_SIFT_BASH !== "0"; // default ON
const GATE_ENABLED = process.env.RELIARY_GATE !== "0";   // default ON
// Pack regeneration on edit: opt-in, default OFF.
// When ON, after edit/write tools fire, regenerates the holographic pack for the file's project.
// Cost: ~5s regen + cache miss on next turn ($0.001). Worth it only if the pack
// has stale data the model is likely to query against.
const PACK_REGEN_ON_EDIT = process.env.RELIARY_PACK_REGEN_ON_EDIT === "1"; // default OFF

let LOG_LEVEL = 3;
try {
  if (process.env.RELIARY_LOG === "error") LOG_LEVEL = 1;
  else if (process.env.RELIARY_LOG === "warn") LOG_LEVEL = 2;
  else if (process.env.RELIARY_LOG === "debug") LOG_LEVEL = 4;
  else if (process.env.RELIARY_LOG === "trace") LOG_LEVEL = 5;
} catch {}

function gateLog(level, msg) {
  const lv = { error: 1, warn: 2, info: 3, debug: 4, trace: 5 }[level] || 3;
  if (lv > LOG_LEVEL) return;
  console.error(`[gate] ${msg}`);
}

// Binary discovery — same locations as before, agent-agnostic.
let RELIARY_BIN = process.env.RELIARY_BIN_PATH || null;
if (!RELIARY_BIN) {
  try {
    const which = execFileSync("which", ["reliary"], { encoding: "utf-8", timeout: 2000 });
    if (which) RELIARY_BIN = which.trim();
  } catch {}
}
if (!RELIARY_BIN) {
  // Fall back to the old binary name during the transition window.
  try {
    const which = execFileSync("which", ["reliary-agent"], { encoding: "utf-8", timeout: 2000 });
    if (which) RELIARY_BIN = which.trim();
  } catch {}
}
if (!RELIARY_BIN) {
  for (const c of ["/usr/local/bin/reliary", "/usr/bin/reliary",
                   "/usr/local/bin/reliary-agent", "/usr/bin/reliary-agent"]) {
    if (existsSync(c)) { RELIARY_BIN = c; break; }
  }
}

gateLog("info", `v${GATE_VERSION} thin shim — reliary: ${RELIARY_BIN ? "found" : "missing"}${RELIARY_BIN ? ` (${RELIARY_BIN.split("/").pop()})` : ""}; pack_regen_on_edit: ${PACK_REGEN_ON_EDIT}`);

let sessionTurns = 0;

// After a file-modifying tool succeeds, refresh the FTS5 index for that file so the
// LLM's next search reflects reality. ~5ms, best-effort, never blocks the agent.
function triggerReindex(filePath) {
  if (!filePath || !RELIARY_BIN) return;
  try {
    execFileSync(RELIARY_BIN, ["reindex-file", filePath], { stdio: "ignore", timeout: 5000 });
  } catch {
    // Best-effort: reindex failure must never break the agent's flow.
  }
}

// Trigger full pack regeneration. HEAVY: 5-10s for large repos, busts the cache for the pack prefix.
// Only call this from handleToolResult where latency doesn't matter and the user just edited code.
// To find project root, walk up from filePath looking for .reliary/index.sqlite.
function triggerPackRegen(filePath) {
  if (!filePath || !RELIARY_BIN || !PACK_REGEN_ON_EDIT) return;
  try {
    const fs = require("fs");
    const path = require("path");
    let dir = path.dirname(path.resolve(filePath));
    let projectRoot = null;
    for (let i = 0; i < 20; i++) {
      if (fs.existsSync(path.join(dir, ".reliary", "index.sqlite"))) {
        projectRoot = dir;
        break;
      }
      const parent = path.dirname(dir);
      if (parent === dir) break;
      dir = parent;
    }
    if (!projectRoot) return;
    execFileSync(RELIARY_BIN, ["pack", projectRoot, "--format", "l2l3", "--strategy", "full"], {
      stdio: "ignore", timeout: 30000,
    });
    gateLog("debug", `pack regenerated for ${projectRoot}`);
  } catch (e) {
    gateLog("debug", `pack regen failed: ${e.message ? e.message.slice(0, 60) : "unknown"}`);
  }
}

// Gate marker — one block per session (PID-based)
const GATE_MARKER = join(tmpdir(), `reliary-gate-${process.pid}`);
function gateMarkerExists() { return existsSync(GATE_MARKER); }
function gateMarkerCreate() {
  try { writeFileSync(GATE_MARKER, "1"); } catch {}
  // Clean up on exit
  process.on("exit", () => { try { unlinkSync(GATE_MARKER); } catch {} });
}

// Hook: tool_call — reindex after writes/edits + optional bash sift + code discovery gate.
function handleToolCall(event) {
  const name = event.toolName;
  const input = event.input || {};
  if (name === "write" || name === "edit") {
    const p = input.path || input.file;
    if (p) gateLog("debug", `post-edit reindex: ${p}`);
  }
  // Bash sift: intercept non-trivial bash commands and pipe through `reliary wrap`.
  // Compressed output replaces the raw result — LLM never sees verbose tool output.
  // Toggle: RELIARY_SIFT_BASH=1 (default OFF for safety).
  if (SIFT_BASH && name === "bash" && RELIARY_BIN) {
    const cmd = input.command || "";
    if (!cmd || cmd.length < 20) return; // skip trivial commands
    // V61: spawnSync (not execFileSync) — execFileSync THROWS on non-zero
    // exit and LOSES stdout (grep -q no-match = exit 1 with useful stdout),
    // and the catch let Pi re-execute the command (double execution of
    // side-effect commands). spawnSync captures stdout/stderr on any exit
    // code, and we always block + return the result so Pi never re-runs.
    const result = spawnSync(RELIARY_BIN, ["wrap", "bash", "-c", cmd], {
      encoding: "utf-8", timeout: 120000, maxBuffer: 10 * 1024 * 1024
    });
    if (result.error) {
      gateLog("debug", `sift spawn error: ${result.error.message ? result.error.message.slice(0, 60) : "error"}`);
      return; // binary missing — let Pi run normally
    }
    if (result.status === 0) {
      gateLog("debug", `sift: ${cmd.slice(0, 40)} → ${result.stdout.length} chars`);
      return { block: true, reason: result.stdout };
    }
    // Non-zero exit: wrap ran the command; return its (possibly useful)
    // stdout/stderr to the LLM. Never let Pi re-execute.
    const out = (result.stdout || result.stderr || "").trim();
    gateLog("debug", `sift exit ${result.status}: ${out.slice(0, 60)}`);
    return { block: true, reason: out || `(exit ${result.status})` };
  }
  // Code discovery gate: block first grep/read per session, redirect to reliary tools.
  // Toggle: RELIARY_GATE=1 (default OFF).
  if (GATE_ENABLED && (name === "grep" || name === "read" || name === "glob")) {
    if (!gateMarkerExists()) {
      gateMarkerCreate();
      return { block: true, reason:
        "BLOCKED: For code intelligence, use reliary MCP tools first:\n" +
        "  - reliary_find_references_with_source(name) — find references to a symbol\n" +
        "  - reliary_search(query) — full-text search\n" +
        "  - reliary_goto_def(name) — jump to definition\n" +
        "  - reliary_callgraph(name) — callers/callees\n" +
        "  - reliary_methods_on(type_name) — methods on a type\n" +
        "If reliary lacks the data, retry this tool."
      };
    }
  }
}

// Hook: tool_result — reindex the touched file once the edit has actually landed.
// Also regenerate pack if enabled (opt-in, slow).
function handleToolResult(event) {
  const name = event.toolName;
  const input = event.input || {};
  if (name === "write" || name === "edit") {
    const f = input.path || input.file;
    triggerReindex(f);
    triggerPackRegen(f);
  }
}

// Hook: before_provider_request — turn counting for diagnostics only. No mutation.
function handleBeforeProviderRequest(event) {
  const payload = event.payload;
  if (!payload || !Array.isArray(payload.messages)) return payload;
  let turnCount = 0;
  for (const m of payload.messages) { if (m.role === "user") turnCount++; }
  if (turnCount > sessionTurns) {
    gateLog("info", `turn ${turnCount}`);
    sessionTurns = turnCount;
  }
  return payload;
}

module.exports = function (pi) {
  pi.on("tool_call", handleToolCall);
  pi.on("tool_result", handleToolResult);
  pi.on("before_provider_request", handleBeforeProviderRequest);
};
