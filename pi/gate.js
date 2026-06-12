const { execFileSync, spawnSync } = require("child_process");
const { existsSync, readFileSync, readdirSync, statSync, unlinkSync } = require("fs");
const { createHash } = require("crypto");
const path = require("path");

const GATE_VERSION = "0.3.0";

// ── Feature config: daemon-backed with local cascade fallback ──
const FEATURE_DEFAULTS = {
  compress: true,
  convWindow: true,
  taskTargets: false,
  readEnrichment: true,
  editMerge: false,
  priorInjection: false,
};
let FEATURES = { ...FEATURE_DEFAULTS };

function featureEnabled(name) {
  return FEATURES[name] !== false;
}

function loadFeatures() {
  // Start with defaults
  const result = { ...FEATURE_DEFAULTS };
  // If daemon available, query its config (authoritative for daemon-backed features)
  if (RELIARY_BIN) {
    const r = daemonCmd("config-features");
    if (r) {
      try { const df = JSON.parse(r); Object.assign(result, df); return result; } catch {}
    }
  }
  // Fallback: local file cascade (same as v0.3.0)
  const home = process.env.HOME || process.env.USERPROFILE || ".";
  for (const p of [
    path.join(home, ".config", "reliary", "config.json"),
    path.join(home, ".reliary", "config.json"),
  ]) { try { const d = JSON.parse(readFileSync(p, "utf-8")); if (d.features) Object.assign(result, d.features); } catch {} }
  try { const d = JSON.parse(readFileSync(".relconf.json", "utf-8")); if (d.features) Object.assign(result, d.features); } catch {}
  try { const d = JSON.parse(readFileSync(".reliary/config.json", "utf-8")); if (d.features) Object.assign(result, d.features); } catch {}
  const env = process.env.RELIARY_FEATURES;
  if (env) { env.split(",").forEach(f => {
    const t = f.trim();
    if (t.startsWith("-")) result[t.slice(1)] = false;
    else if (t.startsWith("+")) result[t.slice(1)] = true;
    else result[t] = true;
  }); }
  FEATURES = result;
}
let lastLogTime = Date.now();

function gateLog(level, msg) {
  const now = Date.now();
  const dt = ((now - lastLogTime) / 1000).toFixed(1);
  lastLogTime = now;
  const sym = { i: "•", ok: "✓", save: "↓", block: "⛔", warn: "⚠" }[level] || "•";
  console.error(`[gate] ${sym} ${msg} (${dt}s)`);
}

// ── Early exit for fast mode: skip ALL daemon discovery, restore inline compression ──
let RELIARY_BIN = null;
let GATE_MODE = "reactive";
let DAEMON_HEALTHY = true;

if (process.env.RELIARY_MODE === "fast") {
  GATE_MODE = "fast";
  console.error(`[gate] ✓ v${GATE_VERSION} — fast mode: inline compression only, zero daemon calls`);
} else {
  // Normal daemon discovery for non-fast modes
  for (const c of [
    "$HOME/src/reliary-agent/target/release/reliary-agent",
    "$HOME/.local/bin/reliary-agent",
    "/usr/local/bin/reliary-agent",
    "/usr/bin/reliary-agent",
  ]) { if (existsSync(c)) { RELIARY_BIN = c; break; } }
  console.error(`[gate] ✓ v${GATE_VERSION} — reliary: ${!!RELIARY_BIN}${RELIARY_BIN ? ` (${RELIARY_BIN.split("/").pop()})` : " none"}`);

  // Daemon health check: TCP ping to verify daemon is running
  let DAEMON_HEALTHY = false;
  if (RELIARY_BIN) {
    try {
      const r = execFileSync("nc", ["-w", "1", "127.0.0.1", "9799"], {
        encoding: "utf-8", input: "ping\n", timeout: 2000,
      });
      DAEMON_HEALTHY = (r || "").trim() === "pong";
    } catch {}
    if (!DAEMON_HEALTHY) {
      gateLog("warn", "daemon binary found but not reachable on :9799 — safety features disabled");
    }
  }

  // Config mode: query daemon or env var
  try {
    if (RELIARY_BIN) {
      const modeCmd = execFileSync(RELIARY_BIN, ["config"], { encoding: "utf-8", timeout: 3000, maxBuffer: 512 });
      const m = (modeCmd || "").match(/gate mode: (\w+)/);
      if (m) GATE_MODE = m[1];
    }
  } catch { /* daemon query failed — use default */ }
  try {
    if (process.env.RELIARY_MODE) GATE_MODE = process.env.RELIARY_MODE;
  } catch {}
}

// ── Safety level (controlled by mode system — fast/reactive/strict, not auto-escalation) ──
let safetyLevel = 0;

// ── Read dedup cache (daemon-backed, persists across Pi restarts) ──
const readCache = {};
// ── Task target files: extracted from user prompt on turn 1 (only if taskTargets enabled) ──
let taskTargetFiles = [];

function daemonCmd(cmd) {
  // Fast mode: no daemon communication
  if (!RELIARY_BIN) return null;
  // Prefer TCP to daemon port (~1ms) over CLI spawn (~15ms)
  try {
    const nc = require("child_process").execFileSync("nc", ["-w", "2", "127.0.0.1", "9799"],
      { encoding: "utf-8", input: cmd + "\n", timeout: 5000, maxBuffer: 16384 }
    );
    if (nc && nc.length > 0) return nc.trim();
  } catch {}
  // Fallback to CLI spawn (slower, works without daemon)
  try {
    const args = cmd.split(" ");
    const r = execFileSync(RELIARY_BIN, args, {
      encoding: "utf-8", timeout: 10000, maxBuffer: 4096,
    });
    return r.trim();
  } catch {}
  return null;
}

// ── Daemon health check: verify binary exists ──

function cacheRead(path, hash, len) {
  return daemonCmd(`cache-read ${path} ${hash} ${len}`);
}

function checkRead(path, hash) {
  return daemonCmd(`check-read ${path} ${hash}`);
}

function extractWorkdir(msgs) {
  for (const m of msgs) {
    if (m.role !== "user") continue;
    const text = extractMessageText(m);
    if (!text) continue;
    let m2 = text.match(/\/(?:[\w./-]+)\/(?:src|tests?|lib|bin)/);
    if (m2) return m2[0].split("/src")[0];
    m2 = text.match(/\/(?:tmp|home|Users)\/[^\s,;)]{3,120}/);
    if (m2) return m2[0].replace(/\/$/,'');
  }
  return null;
}

// ── Extract text content from the various Pi message block shapes ──
function extractMessageText(m) {
  if (!m) return null;
  const content = m.content;
  if (!content) return m.text || null;
  if (typeof content === "string") return content;
  if (Array.isArray(content)) {
    for (const b of content) {
      if (b.type === "text" && b.text) return b.text;
      if (b.type === "thinking" && b.thinking) return b.thinking;
    }
  }
  return null;
}

function extractText(content) {
  if (!content) return "";
  if (typeof content === "string") return content;
  if (Array.isArray(content)) {
    return content.map(b => b.text || b.thinking || "").filter(Boolean).join("\n");
  }
  return "";
}

function isCodeFile(p) {
  const ext = p.split(".").pop();
  return CODE_EXTS.has("." + (ext || ""));
}

// ── Extract function/class signatures for read enrichment ──
function extractSignatures(text) {
  const names = [];
  const re = /^\s*(pub\s+)?(fn|def|class|struct|enum|trait|function|func)\s+(\w+)/;
  for (const line of text.split("\n")) {
    const m = line.match(re);
    if (m) names.push({ name: m[3], line: line.trim() });
  }
  return { names, found: names.length };
}

// ── Build structured summary from raw file content ──
function buildStructuredSummary(content, filePath) {
  const sigs = extractSignatures(content);
  const header = `[${filePath.split("/").pop()}] ${content.split("\n").length}L | ${sigs.found} defs`;
  const defs = sigs.names.slice(0, 6).map(n => `  ${n.line}`).join("\n");
  let enriched = `${header}${defs ? "\n" + defs : ""}`;
  if (enriched.length > 600) enriched = enriched.slice(0, 600);
  return enriched;
}

// ── Dynamic hook guards (unused but kept for compatibility) ──
function isMature(turns) { return sessionTurns >= turns; }

function qualMaxBlocks() {
  if (!isMature(3)) return 0;
  if (!isMature(6)) return 1;
  if (!isMature(10)) return 2;
  return 3;
}

// ── Grammar-free test runner via daemon ──
function runTest(workdir) {
  if (!RELIARY_BIN) return "ERROR: reliary-agent not available";
  try {
    const r = execFileSync(RELIARY_BIN, ["test", workdir], {
      encoding: "utf-8", timeout: 120000, maxBuffer: 32768,
    });
    return r.trim();
  } catch (e) {
    return "ERROR: test execution failed";
  }
}

// ── Hook A: tool_result — read dedup + compression ──
function handleToolResult(event) {
  const name = event.toolName;
  const input = event.input || {};
  const text = extractText(event.content);
  if (!text || text.length < 200) return;
  const pathHint = input.path || input.file || null;

  // Read dedup + enrichment + debug spiral detection
  if (name === "read" && pathHint && !input.offset && !input.limit) {
    // Debug spiral: if same file read 3x without edit, escalate to strict
    if (pathHint in readSpiralCount) {
      readSpiralCount[pathHint]++;
      if (readSpiralCount[pathHint] >= 3) {
        maybeEscalate(`debug spiral: ${pathHint} read ${readSpiralCount[pathHint]}x without edit`, 2, 10);
      }
    } else {
      readSpiralCount[pathHint] = 0; // will be incremented on next read
    }
    // Track reads
    readSpiralCount[pathHint] = (readSpiralCount[pathHint] || 0) + 1;

    try {
      const hash = createHash("sha256").update(text).digest("hex").slice(0, 16);
      if (readCache[pathHint] === hash) {
        gateLog("save", `dedup: ${pathHint} (${text.length}c)`);
        return { content: [{ type: "text", text: `[reliary: ${hash.slice(0,8)}] ${pathHint} — unchanged (${text.length} chars)` }] };
      }
      readCache[pathHint] = hash;
      if (RELIARY_BIN) cacheRead(pathHint, hash.slice(0, 16), text.length);
    } catch {}
  }

  // Read content: build structured summary ONLY if readEnrichment enabled and file not targeted
  if (featureEnabled("readEnrichment") && name === "read" && text.length > 1000) {
    const fp = pathHint || "";
    const isTarget = featureEnabled("taskTargets") && taskTargetFiles.some(t => fp.includes(t));
    if (!isTarget) {
      const enriched = buildStructuredSummary(text, pathHint || name);
      if (enriched && enriched.length < text.length * 0.8) {
        gateLog("save", `read: ${(pathHint || name).split("/").pop()} ${text.length}→${enriched.length}c (${Math.round((1 - enriched.length/text.length)*100)}%)`);
        return { content: [{ type: "text", text: enriched }] };
      }
    } else {
      gateLog("save", `read: ${(pathHint || name).split("/").pop()} — target file, returning full content`);
    }
  }

  // Bash output: zone truncation for large output (preserve errors)
  if (name === "bash") {
    let compressed = text;
    if (compressed.length > 2000) {
      const head = compressed.slice(0, 1000);
      const errorSig = /[Ee]rror|[Ww]arning|FAILED|failed|expected |unexpected|not found/i.test(head);
      if (errorSig) {
        compressed = head.length > 1200 ? head.slice(0, 1200).replace(/\n\s*\n\s*\n.*$/s, "\n") : head;
      }
    }
    if (compressed !== text) {
      return { content: [{ type: "text", text: compressed }] };
    }
  }
}

// ── Hook B: tool_call — handle test/explain, pass read/edit through ──
function handleToolCall(event) {
  const name = event.toolName;
  const input = event.input || {};

  // Test tool: run grammar-free test runner via daemon
  if (name === "test") {
    const workdir = input.workdir || input.path || process.cwd();
    gateLog("ok", `test: ${workdir}`);
    const result = runTest(workdir);
    return { block: true, response: result };
  }

  // Explain tool: get function context
  if (name === "explain") {
    const file = input.file || input.path || "";
    const func = input.function || input.name || "";
    if (!file || !func) {
      return { block: true, response: "ERROR: explain requires 'file' and 'function' parameters" };
    }
    try {
      const content = readFileSync(file, "utf-8");
      const lines = content.split("\n");
      const sigs = extractSignatures(content);
      const target = sigs.names.find(n => n.name.includes(func) || func.includes(n.name));
      const lineNo = target ? lines.indexOf(target.line) + 1 : 0;
      let result = `[${file.split("/").pop()}] L${lineNo || "?"}: ${target?.line || func}\n`;
      if (sigs.found > 0) result += `defs: ${sigs.names.slice(0, 5).map(n => n.name).join(", ")}${sigs.found > 5 ? " (+" + (sigs.found - 5) + ")" : ""}\n`;
      result += `risk: ${daemonCmd(`risk ${file}`) || "unknown"}`;
      gateLog("save", `explain: ${file} → ${func}`);
      return { block: true, response: result };
    } catch (e) {
      return { block: true, response: `ERROR: ${e.message}` };
    }
  }

  // Create tool: write new file with content, run heal on project
  if (name === "create") {
    const file = input.file || input.path || "";
    if (!file) return { block: true, response: "ERROR: create requires 'file' parameter" };
    const content = input.content || "";
    if (!content) return { block: true, response: "ERROR: create requires 'content' parameter" };
    try {
      if (existsSync(file)) return { block: true, response: `ERROR: ${file} already exists (use edit to modify)` };
      writeFileSync(file, content, "utf-8");
      gateLog("save", `create: ${file} (${content.length}c)`);
      return { block: true, response: `Created ${file} (${content.length} chars). Run 'test <workdir>' to verify.` };
    } catch (e) {
      return { block: true, response: `ERROR: ${e.message}` };
    }
  }

  // Bash: pass through — don't block. The LLM uses python3 to bypass any regex.
  // The real fix is taskTargetFiles (don't compress bug-relevant files).
  if (name === "bash") return;

  // Write: block at safetyLevel >= 2, pass through otherwise
  if (name === "write") {
    if (safetyLevel >= 2) {
      gateLog("block", `write blocked (strict ${safetyLevel})`);
      return { block: true, response: "write is disabled in strict mode. Use edit to modify existing files or create to add new files." };
    }
    return; // pass through
  }

  // Grep: block at safetyLevel >= 2, pass through otherwise
  if (name === "grep") {
    if (safetyLevel >= 2) {
      gateLog("block", `grep blocked (strict ${safetyLevel}) — use search instead`);
      return { block: true, response: "grep is disabled in strict mode. Use search (FTS5 index) for faster results." };
    }
    return; // pass through
  }

  // Edit: heal-apply with daemon (reactive safety — veto escalates on hallucination)
  if (name === "edit") {
    const path = input.path;
    if (!path || !isCodeFile(path)) return;
    // Clear read spiral count for this file (edit indicates progress)
    delete readSpiralCount[path];

    // Vet the edit
    const edits = input.edits || [];
    if (edits.length > 0 && edits[0].newText && RELIARY_BIN) {
      // Veto check at safetyLevel >= 1
      if (safetyLevel >= 1) {
        const vetoResult = reliaryVeto(path, edits[0].newText);
        if (vetoResult && vetoResult.startsWith("ERROR")) {
          maybeEscalate(`veto blocked: ${path} — ${vetoResult.substring(7, 50)}`, 1, 3);
          return { block: true, response: vetoResult };
        }
      }
      try {
        const content = readFileSync(path, "utf-8");
        const oldText = edits[0].oldText || "";
        const newText = edits[0].newText || "";
        const modified = content.replace(oldText, newText);
        if (modified === content) {
          return { block: true, response: `ERROR: could not find oldText in ${path}` };
        }
        const tmpFile = `/tmp/gate-heal-${Date.now()}.tmp`;
        writeFileSync(tmpFile, modified, "utf-8");
        const healResult = daemonCmd(`apply-edit ${path} ${tmpFile} ${getRepoRoot() || ""}`);
        try { unlinkSync(tmpFile); } catch {}
        if (healResult && healResult.startsWith("REVERTED")) {
          maybeEscalate(`heal reverted: ${path}`, 1, 3);
          return { block: true, response: `Edit reverted: ${healResult.substring(9)}` };
        }
        return { block: true, response: `Edit applied and verified: ${healResult || "ok"}` };
      } catch (e) {
        return { block: true, response: `ERROR: ${e.message}` };
      }
    }
  }
}

// ── Compression: daemon-backed, inline fallback (grammar-free) ──
function compressReasoning(text) {
  if (!text || text.length < 600) return null;
  if (sessionTurns < 3 && text.length < 1500) return null;
  if (text.includes("```") || text.includes("//") || text.includes("/*")
      || text.includes("src/") || text.includes(".rs:") || text.includes(".py:")
      || text.includes("s/") || text.includes(".md")) return null;

  // Daemon path: call daemon compress endpoint
  if (RELIARY_BIN) {
    try {
      const r = daemonCmd(`compress ${text}`);
      if (r && r !== "no compression") return r;
    } catch {}
  }

  // Inline fallback: v0.3.0 JS compression (fast mode)
  let result = text
    .replace(/\b(Let me (analyze|look|check|review|see|think|consider)\b[^.]*\.)/gi, '')
    .replace(/\b(I (?:can|would|will) need to)[^.]*\./gi, '')
    .replace(/\b(In order to)[^.]*\./gi, '')
    .replace(/\b(First(?:,|ly)? let me)[^.]*\./gi, '')
    .replace(/\b(Based on (?:the|this|my|our))[^.]*\.?/gi, '')
    .replace(/\b(This means that)[^.]*\./gi, '')
    .replace(/\b(The (?:next|final|first) step)[^.]*\./gi, '')
    .replace(/\b(Now I(?: can| will|'ll| need to| should))[^.,;]*[,;.]?/gi, '')
    .replace(/\b(Alright|Okay|So,?|Well,?|Now,?)\s*/gi, '')
    .replace(/\bessentially|basically|simply|actually|obviously|clearly|currently\b/gi, '')
    .replace(/\s{2,}/g, ' ')
    .trim();
  if (result.length < text.length * 0.6) return result;
  return null;
}

function applyReasoningCompression(msgs) {
  let count = 0;
  for (let i = 0; i < msgs.length - 2; i++) {
    const m = msgs[i];
    if (m.role !== "assistant") continue;
    if (!Array.isArray(m.content)) continue;
    for (const block of m.content) {
      if (block.type === "thinking" && block.thinking?.length > 300) {
        const compact = compressReasoning(block.thinking);
        if (compact) { block.thinking = compact; count++; }
      }
      if (block.type === "text" && block.text?.length > 400) {
        const compact = compressReasoning(block.text);
        if (compact) { block.text = compact; count++; }
      }
    }
  }
  return count;
}

// ── Edit merge: combine sequential edits to same file ──
function compressEditCalls(msgs) {
  for (let i = 1; i < msgs.length; i++) {
    const m = msgs[i];
    if (m.role !== "assistant") continue;
    const prev = msgs[i - 1];
    if (prev.role !== "assistant") continue;
    let prevEdits = null, curEdits = null;
    if (Array.isArray(prev.content)) {
      for (const b of prev.content) {
        if (b.type === "text" && b.text) {
          const m2 = b.text.match(/\[(edit|tool_call|apply)\]/);
        }
      }
    }
  }
  return msgs;
}

// ── Conversation window: daemon-backed, inline fallback ──
function applyConversationWindow(msgs) {
  const n = msgs.length;
  if (n < 10) return msgs;

  // Daemon path: call conv-window endpoint with full conversation
  if (RELIARY_BIN) {
    try {
      const json = JSON.stringify(msgs);
      if (json.length > 100 && json.length < 500000) {
        const r = daemonCmd(`conv-window ${json}`);
        if (r) {
          try {
            const parsed = JSON.parse(r);
            if (Array.isArray(parsed)) {
              const dropped = n - parsed.length;
              if (dropped > 0) gateLog("save", `conv-window: dropped ${dropped} verbose tool results (daemon)`);
              return parsed;
            }
          } catch {}
        }
      }
    } catch {}
  }

  // Inline fallback: collapse tool result CONTENT but keep the message frame
  const keepFirst = 2; const keepLast = 6;
  const middle = msgs.slice(keepFirst, n - keepLast);
  if (middle.length < 2) return msgs;
  let compressed = 0;
  for (const m of middle) {
    if (m.role === "tool" || m.role === "toolResult") {
      if (typeof m.content === "string" && m.content.length > 200) {
        m.content = "[tool result — omitted]";
        compressed++;
      }
    }
  }
  if (compressed === 0) return msgs;
  gateLog("save", `conv-window: compressed ${compressed} verbose tool results (inline)`);
  return msgs;
}

// ── Hook C: before_provider_request — IR reasoning compression + conv window + prior injection ──
function handleBeforeProviderRequest(event) {
  const payload = event.payload;
  if (!payload || !Array.isArray(payload.messages)) return;
  let msgs = payload.messages;

  let turnCount = 0;
  for (const m of msgs) { if (m.role === "user") turnCount++; }
  if (turnCount > sessionTurns) {
    gateLog("i", `turn ${turnCount} (prev: ${sessionTurns})`);
    sessionTurns = turnCount;
  }

  // Reactive safety expiry: if expired, drop back to fast mode
  if (safetyLevel > 0 && safetyExpiresAt <= sessionTurns) {
    safetyLevel = 0;
    gateLog("i", "safety expired — back to fast mode");
  }

  // Turn 1: extract task target files (if taskTargets enabled) + inject chronicled prior (if priorInjection enabled)
  if (turnCount === 1) {
    if (featureEnabled("taskTargets")) {
      const firstUser = msgs.find(m => m.role === "user");
      const text = extractMessageText(firstUser);
      if (text) {
        const fileMatches = text.match(/\b[a-zA-Z_][\w.-]*\.(?:rs|py|js|ts|go|java|rb|cpp|c|h)\b/g);
        if (fileMatches) taskTargetFiles = [...new Set(fileMatches)];
      }
    }
    if (featureEnabled("priorInjection") && RELIARY_BIN) {
      const workdir = extractWorkdir(msgs);
      if (workdir) {
        try {
          const r = execFileSync(RELIARY_BIN, ["prior", workdir], {
            encoding: "utf-8", timeout: 5000, maxBuffer: 4096,
          });
          const priorBlock = r.trim();
          if (priorBlock) {
            msgs.splice(1, 0, { role: "system", content: priorBlock });
            gateLog("save", `prior: ${priorBlock.substring(0, 80)}`);
          }
        } catch {}
      }
    }
  }

  // Compress reasoning in all prior assistant messages
  if (featureEnabled("compress")) {
    const compCount = applyReasoningCompression(msgs);
    if (compCount > 0) gateLog("save", `compressed ${compCount} blocks`);
  }

  if (featureEnabled("convWindow")) msgs = applyConversationWindow(msgs);
  if (featureEnabled("editMerge")) msgs = compressEditCalls(msgs);
  return { ...payload, messages: msgs };
}

// ── Export: register hooks ──
// ── Export: register hooks (CommonJS for Pi extension loader) ──
module.exports = function (pi) {
  // Load features from daemon or fallback cascade
  loadFeatures();
  console.error(`[gate] ✓ v${GATE_VERSION} — reliary: ${!!RELIARY_BIN}${RELIARY_BIN ? ` (${RELIARY_BIN.split("/").pop()})` : " none"}`);
  pi.on("tool_result", handleToolResult);
  pi.on("tool_call", handleToolCall);
  pi.on("before_provider_request", handleBeforeProviderRequest);
  process.on("exit", () => {});
}
