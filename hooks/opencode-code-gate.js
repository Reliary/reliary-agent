// reliary code discovery gate for OpenCode
// Blocks the first grep/glob/read per session, redirects to reliary MCP tools.
// Subsequent calls pass through. Nudge, not force.
//
// Install: add to ~/.config/opencode/plugins/ and reference in opencode.json plugins array
// Toggle: RELIARY_GATE=0 to disable (default ON)

const { mkdtempSync, rmdirSync } = require("fs");
const { tmpdir } = require("os");
const { join } = require("path");

// C5: use mkdtempSync for atomic create (O_CREAT|O_EXCL) — eliminates TOCTOU
// race and cross-session PID collision. Unique per-invocation suffix prevents
// other opencode plugin instances from colliding.
let GATE_MARKER = null;
let gateTriggered = false;

function gateMarkerTryCreate() {
  if (gateTriggered) return false;
  try {
    GATE_MARKER = mkdtempSync(join(tmpdir(), `reliary-gate-${process.pid}-`));
    gateTriggered = true;
    process.on("exit", () => {
      try { rmdirSync(GATE_MARKER); } catch {}
    });
    return true;
  } catch {
    return false;
  }
}

const REDIRECT_MSG = "BLOCKED: For code intelligence, use reliary MCP tools first:\n" +
  "  - reliary_find_references_with_source(name) — find references to a symbol\n" +
  "  - reliary_search(query) — full-text search\n" +
  "  - reliary_goto_def(name) — jump to definition\n" +
  "  - reliary_callgraph(name) — callers/callees\n" +
  "  - reliary_methods_on(type_name) — methods on a type\n" +
  "If reliary lacks the data, retry this tool.";

module.exports = function (opencode) {
  const GATE_ENABLED = process.env.RELIARY_GATE !== "0"; // default ON
  if (!GATE_ENABLED) return;

  opencode.on("tool.execute.before", (event) => {
    const tool = event.tool;
    // M9: case-insensitive tool matching for SDK version portability
    if (!tool) return;
    const t = tool.toLowerCase();
    if (t !== "grep" && t !== "read" && t !== "glob") return;
    // C5: mkdtempSync returns Ok only on atomic create. If gate already
    // exists for this session, it returns false (no race window).
    if (gateTriggered) return;
    if (!gateMarkerTryCreate()) return; // already gated this session
    // Return a blocked result — LLM sees the redirect message
    event.result = REDIRECT_MSG;
    event.skip = true;
  });
};