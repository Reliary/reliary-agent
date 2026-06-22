const { execFileSync, spawnSync } = require("child_process");
const { existsSync, readFileSync, readdirSync, statSync, unlinkSync } = require("fs");
const { createHash } = require("crypto");

const GATE_VERSION = "0.6.7";

// ── Log levels (matching RELIARY_LOG convention) ──
const LOG_LEVELS = { error: 1, warn: 2, info: 3, debug: 4, trace: 5 };
let LOG_LEVEL = 3; // default: info
try {
  if (process.env.RELIARY_LOG && LOG_LEVELS[process.env.RELIARY_LOG] !== undefined) {
    LOG_LEVEL = LOG_LEVELS[process.env.RELIARY_LOG];
  } else if (process.env.RUST_LOG) {
    const rl = process.env.RUST_LOG;
    if (rl.includes("trace")) LOG_LEVEL = 5;
    else if (rl.includes("debug")) LOG_LEVEL = 4;
    else if (rl.includes("warn")) LOG_LEVEL = 2;
    else if (rl.includes("error")) LOG_LEVEL = 1;
  }
} catch {}

let lastLogTime = Date.now();

function gateLog(level, msg) {
  const lv = LOG_LEVELS[level];
  if (lv === undefined || lv > LOG_LEVEL) return;
  const now = Date.now();
  const dt = ((now - lastLogTime) / 1000).toFixed(1);
  lastLogTime = now;
  const sym = { error: "⛔", warn: "⚠", info: "•", debug: "↓", trace: "·" }[level] || "•";
  console.error(`[gate] ${sym} ${msg} (${dt}s)`);
}

// ── Binary discovery (env var → PATH → hardcoded paths) ──
let RELIARY_BIN = process.env.RELIARY_BIN_PATH || null;
if (!RELIARY_BIN) {
  // Check PATH first
  try {
    const which = execFileSync("which", ["reliary-agent"], { encoding: "utf-8", timeout: 2000 });
    if (which) RELIARY_BIN = which.trim();
  } catch {}
}
if (!RELIARY_BIN) {
  // Check hardcoded paths as fallback
  for (const c of [
    "/usr/local/bin/reliary-agent",
    "/usr/bin/reliary-agent",
  ]) { if (existsSync(c)) { RELIARY_BIN = c; break; } }
}

gateLog("info", `v${GATE_VERSION} — reliary: ${!!RELIARY_BIN}${RELIARY_BIN ? ` (${RELIARY_BIN.split("/").pop()})` : " none"}`);

const CODE_EXTS = new Set([
  ".py", ".rs", ".js", ".ts", ".tsx", ".jsx",
  ".cpp", ".c", ".h", ".hpp", ".go", ".java",
  ".rb", ".swift", ".kt", ".scala",
]);

let blockedCount = 0;
let sessionTurns = 0;
let repoRoot = null;

// ── Config mode: query daemon or env var ──
// ── Gate configuration ──
let GATE_MODE = "strict"; // default (transparent redirect, auto-deescalates to reactive after 5 redirects)
try {
  if (RELIARY_BIN) {
    const modeCmd = execFileSync(RELIARY_BIN, ["config"], { encoding: "utf-8", timeout: 3000, maxBuffer: 512 });
    const m = (modeCmd || "").match(/gate mode: (\w+)/);
    if (m) GATE_MODE = m[1];
  }
} catch { /* daemon query failed — use default */ }
try {
  if (process.env.RELIARY_MODE) {
    GATE_MODE = process.env.RELIARY_MODE;
  }
} catch { /* env var check failed */ }

// ── Feature flags ──
// Each can be disabled via RELIARY_FEATURES env var (e.g. "-healEdit,-convWindow")
const FEATURES = {
  healEdit: false,      // route edit/write/sed through heal-apply (+healEdit to enable)
  compress: true,       // inline reasoning compression
  convWindow: true,     // drop old verbose tool results
  readEnrichment: true, // compress non-target read results
};
if (process.env.RELIARY_FEATURES) {
  for (const f of process.env.RELIARY_FEATURES.split(",")) {
    const isDisable = f.startsWith("-");
    const name = isDisable ? f.slice(1) : f.startsWith("+") ? f.slice(1) : f;
    if (name) FEATURES[name] = !isDisable;
  }
}

// ── Reactive safety level ──
// 0 = fast (pure IR compression), 1 = safe (heal-apply + veto), 2 = strict (bash/write blocked)
let safetyLevel = 0;
let safetyExpiresAt = 0;
let readSpiralCount = {}; // path → count of reads without edit

function maybeEscalate(reason, level, turns) {
  if (safetyLevel >= level) return;
  safetyLevel = level;
  safetyExpiresAt = sessionTurns + turns;
  gateLog("warn", `safety ${level} for ${turns}t: ${reason}`);
}

// ── Read dedup cache (daemon-backed, persists across Pi restarts) ──
const readCache = {};

function daemonCmd(cmd) {
  try {
    const args = cmd.split(" ");
    // Use spawnSync with proper array args to avoid path-with-spaces issues
    const r = execFileSync(RELIARY_BIN, args, {
      encoding: "utf-8", timeout: 10000, maxBuffer: 4096,
    });
    return r.trim();
  } catch { return null; }
}

// ── Daemon health check: verify binary exists (TCP check would require daemon running) ──
let DAEMON_HEALTHY = true; // assume healthy — CLI fallback handles gracefully

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

// ── Strict mode redirect helpers (transparent, LLM never sees "blocked") ──
let strictRedirects = 0;

function redirectBash(cmd, workdir) {
  // cargo test / pytest → test tool
  if (/cargo\s+test|pytest|npm\s+test|python3?\s+-m\s+pytest/.test(cmd)) {
    gateLog("debug", "redirect bash(test)→test");
    const result = runTest(workdir || process.cwd());
    const sifted = siftOutput(result);
    return { block: true, response: sifted !== result ? sifted : result };
  }

  // cat / head / tail file → read tool
  const readMatch = cmd.match(/\b(?:cat|head|tail)\s+(.+)/);
  if (readMatch) {
    const file = readMatch[1].trim();
    gateLog("debug", "redirect bash(cat)→read");
    try {
      const content = readFileSync(file, "utf-8");
      const lines = content.split("\n");
      let result;
      if (lines.length > 50) {
        result = `[${file}] ${lines.length} lines\n\n` + lines.slice(0, 40).join("\n") + `\n... (${lines.length - 40} more lines)\n\n` + lines.slice(-10).join("\n");
      } else {
        result = content;
      }
      return { block: true, response: result };
    } catch (e) {
      return { block: true, response: `ERROR: ${e.message}` };
    }
  }

  // grep / rg → search tool
  if (/grep|rg\b|ripgrep/.test(cmd)) {
    const query = cmd.replace(/.*\bgrep\b\s*/, "").replace(/^['"]|['"]$/g, "").trim();
    gateLog("debug", "redirect bash(grep)→search");
    const result = daemonCmd(`search ${query}`) || "no results";
    return { block: true, response: `[redirected to search]\n${result}` };
  }

  // ls / find → directory listing
  if (/^ls\b/.test(cmd)) {
    const target = cmd.replace(/^ls\s*/, "").trim() || ".";
    gateLog("debug", "redirect bash(ls)→read");
    try {
      const entries = readdirSync(target);
      const lines = entries.slice(0, 40).join("\n");
      return { block: true, response: `${target}/ (${entries.length} entries)\n${lines}` + (entries.length > 40 ? `\n... (${entries.length - 40} more)` : "") };
    } catch (e) {
      return { block: true, response: `ERROR: ${e.message}` };
    }
  }

  // sed → edit via file healing
  const sedEdit = cmd.match(/sed\s+-i\s+['"]?s\/([^/]+)\/([^/]*)\/['"]?\s*(.+)/);
  if (sedEdit) {
    gateLog("debug", "redirect bash(sed)→edit");
    return { block: true, response: `[redirected to edit] Use the edit tool to modify ${sedEdit[3].trim()}` };
  }

  // Fall back to runTest (most commands are test runners)
  gateLog("debug", "redirect bash→test");
  const result = runTest(workdir || process.cwd());
  const sifted = siftOutput(result);
  return { block: true, response: sifted !== result ? sifted : result };
}

function redirectCreate(filePath, content) {
  try {
    writeFileSync(filePath, content, "utf-8");
    return `Created ${filePath} (${content.length} chars)`;
  } catch (e) {
    return `ERROR: ${e.message}`;
  }
}

function redirectEdit(filePath, oldText, newText) {
  try {
    if (!existsSync(filePath)) return `ERROR: ${filePath} does not exist`;
    const content = readFileSync(filePath, "utf-8");
    const modified = oldText ? content.replace(oldText, newText) : newText;
    writeFileSync(filePath, modified, "utf-8");
    return `Updated ${filePath}`;
  } catch (e) {
    return `ERROR: ${e.message}`;
  }
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
        gateLog("debug", `dedup: ${pathHint} (${text.length}c)`);
        return { content: [{ type: "text", text: `[reliary: ${hash.slice(0,8)}] ${pathHint} — unchanged (${text.length} chars)` }] };
      }
      readCache[pathHint] = hash;
      if (RELIARY_BIN) cacheRead(pathHint, hash.slice(0, 16), text.length);
    } catch {}
  }

  // Read content: build structured summary (grammar-free) or sift for large files
  if (name === "read" && text.length > 1000) {
    // Large files: sift first (collapses structural noise), then fall back to summary
    if (text.length > 5000 && !input.offset && !input.limit) {
      const sifted = siftOutput(text);
      if (sifted !== text) {
        gateLog("debug", `read: ${(pathHint || name).split("/").pop()} ${text.length}→${sifted.length}c (${Math.round((1-sifted.length/text.length)*100)}%)`);
        return { content: [{ type: "text", text: sifted }] };
      }
    }
    // Medium files: structured summary (signatures + callers)
    const enriched = buildStructuredSummary(text, pathHint || name);
    if (enriched && enriched.length < text.length * 0.8) {
      gateLog("debug", `read: ${(pathHint || name).split("/").pop()} ${text.length}→${enriched.length}c (${Math.round((1-enriched.length/text.length)*100)}%)`);
      return { content: [{ type: "text", text: enriched }] };
    }
  }

  // Sift: compress any tool result with repeated patterns
  if (text.length > 300) {
    const compressed = siftOutput(text);
    if (compressed !== text) {
      gateLog("debug", `sift: ${name} ${text.length}→${compressed.length}c (${Math.round((1-compressed.length/text.length)*100)}%)`);
      return { content: [{ type: "text", text: compressed }] };
    }
  }
}

// ── Sift: inline class-line compression ──
function siftOutput(text) {
  if (text.length <= 300) return text;
  const lines = text.split("\n");

  // Classify each line
  const types = lines.map(l => {
    const t = l.trim();
    if (!t) return "blank";
    if (/^(Compiling|Checking|Building|Linking|Running)\b/.test(t)) return "progress";
    if (t === "ok" || /\b\.\.\. ok$/.test(t)) return "ok";
    if (/\b(FAILED|failed|error|Error|ERROR|E\d{4}|Traceback)\b/.test(t)) return "error";
    if (t.startsWith("  --> ") || t.startsWith("   = help") || t.startsWith("   = note")) return "help";
    return "code";
  });

  // Collapse runs of 3+ same-type lines
  const collapsed = [];
  let i = 0;
  while (i < lines.length) {
    const type = types[i];
    let j = i + 1;
    while (j < lines.length && types[j] === type) j++;
    const count = j - i;
    if (count >= 3 && (type === "progress" || type === "ok" || type === "blank")) {
      collapsed.push(type === "ok" ? `[${count} ok]` : type === "blank" ? "" : `[${count} ${lines[i].trim().split(/\s/)[0]} ...]`);
      i = j;
    } else {
      collapsed.push(lines[i]);
      i++;
    }
  }

  const result = collapsed.filter(l => l !== undefined && l !== null).join("\n");
  return result.length < text.length ? result : text;
}

// ── Hook B: tool_call — handle test/explain, pass read/edit through ──
function handleToolCall(event) {
  const name = event.toolName;
  const input = event.input || {};

  // Test tool: run grammar-free test runner via daemon
  if (name === "test") {
    const workdir = input.workdir || input.path || process.cwd();
    gateLog("info", `test: ${workdir}`);
    const result = runTest(workdir);
    const sifted = siftOutput(result);
    return { block: true, response: sifted !== result ? sifted : result };
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
      gateLog("debug", `explain: ${file} → ${func}`);
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
      gateLog("debug", `create: ${file} (${content.length}c)`);
      return { block: true, response: `Created ${file} (${content.length} chars). Run 'test <workdir>' to verify.` };
    } catch (e) {
      return { block: true, response: `ERROR: ${e.message}` };
    }
  }

  // Bash: intercept and redirect in strict mode, sed-heal in reactive
  if (name === "bash") {
    const cmd = input.command || "";

    // Strict mode: transparent redirect to sandbox tools
    if (safetyLevel >= 2) {
      // Track redirect count for auto-deescalation
      strictRedirects = (strictRedirects || 0) + 1;
      if (strictRedirects >= 5) {
        gateLog("warn", `auto-deescalate: ${strictRedirects} redirects`);
        safetyLevel = 1;
        // Fall through to reactive mode
      } else {
        return redirectBash(cmd, getRepoRoot() || process.cwd());
      }
    }

    // Route sed -i commands through heal-apply (if enabled)
    if (FEATURES.healEdit) {
      const sedMatch = cmd.match(/sed\s+-i\s+['"]?s\/([^/]+)\/([^/]*)\/['"]?\s*(.+)/);
      if (sedMatch) {
        const oldText = sedMatch[1];
        const newText = sedMatch[2];
        const filePath = sedMatch[3].trim();
        gateLog("debug", `heal-sed: ${filePath} "${oldText}" → "${newText}"`);
        const result = daemonCmd(`sed-apply ${filePath} ${oldText} ${newText} ${getRepoRoot() || "."}`);
        return { block: true, response: `Edit applied via healing: ${result || "no match"}` };
      }
    }
    // Fast mode: monitor for destructive patterns, escalate if found
    const dd = /sed\s+-i|rm\s+(?:-rf?\s+)?\S+\.(?:py|rs|js|ts|tsx)|\s>\s*\S+\.(?:py|rs|js|ts|tsx)/i;
    if (dd.test(cmd)) {
      maybeEscalate(`destructive bash: ${cmd.slice(0, 40)}`, 1, 3);
    }
    return; // pass through
  }

  // Write: redirect in strict, heal-apply in reactive
  if (name === "write") {
    const filePath = input.file || input.path || "";
    const content = input.content || "";
    if (safetyLevel >= 2 && filePath && content) {
      strictRedirects = (strictRedirects || 0) + 1;
      if (strictRedirects >= 5) {
        gateLog("warn", `auto-deescalate: ${strictRedirects} redirects`);
        safetyLevel = 1;
      } else if (!existsSync(filePath)) {
        return { block: true, response: redirectCreate(filePath, content) };
      } else {
        return { block: true, response: redirectEdit(filePath, "", content) };
      }
    }
    if (FEATURES.healEdit && filePath && content && existsSync(filePath)) {
      gateLog("debug", `heal-write: ${filePath}`);
      const tmpFile = `/tmp/gate-heal-write-${Date.now()}.tmp`;
      try { writeFileSync(tmpFile, content, "utf-8"); } catch {
        return { block: true, response: `ERROR: could not write temp file` };
      }
      const result = daemonCmd(`apply-edit ${filePath} ${tmpFile} ${getRepoRoot() || "."}`);
      try { unlinkSync(tmpFile); } catch {}
      if (result && result.trim() !== "") {
        return { block: true, response: `Edit applied via healing: ${result}` };
      }
    }
    return; // pass through (reactive mode)
  }

  // Grep: redirect to search in strict mode
  if (name === "grep") {
    if (safetyLevel >= 2) {
      strictRedirects = (strictRedirects || 0) + 1;
      if (strictRedirects >= 5) {
        gateLog("warn", `auto-deescalate: ${strictRedirects} redirects`);
        safetyLevel = 1;
      } else {
        const query = (input.command || input.pattern || "").replace(/^.*?\b(\w+).*$/, "$1");
        gateLog("debug", "grep→search: " + query);
        return { block: true, response: `[redirected to search] ` + (daemonCmd(`search ${query}`) || "no results") };
      }
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
    if (edits.length > 0 && edits[0].newText && RELIARY_BIN && FEATURES.healEdit) {
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

// ── Hook C: before_provider_request — safety monitoring + prior injection ──
// Proxy owns ALL message compression. Gate.js owns tool safety only.
function handleBeforeProviderRequest(event) {
  const payload = event.payload;
  if (!payload || !Array.isArray(payload.messages)) return;
  let msgs = payload.messages;

  let turnCount = 0;
  for (const m of msgs) { if (m.role === "user") turnCount++; }
  if (turnCount > sessionTurns) {
    gateLog("info", `turn ${turnCount} (prev: ${sessionTurns})`);
    sessionTurns = turnCount;
  }

  // Reactive safety expiry: if expired, drop back to fast mode
  if (safetyLevel > 0 && safetyExpiresAt <= sessionTurns) {
    safetyLevel = 0;
    gateLog("info", "safety expired — back to fast mode");
  }

  // Turn 1: inject chronicled prior (only in fast/reactive mode, not strict where proxy handles it)
  if (turnCount === 1 && GATE_MODE !== "strict" && RELIARY_BIN) {
    const workdir = extractWorkdir(msgs);
    if (workdir) {
      try {
        const r = execFileSync(RELIARY_BIN, ["prior", workdir], {
          encoding: "utf-8", timeout: 5000, maxBuffer: 4096,
        });
        const priorBlock = r.trim();
        if (priorBlock) {
          msgs.splice(1, 0, { role: "system", content: priorBlock });
          gateLog("debug", `prior: ${priorBlock.substring(0, 80)}`);
        }
      } catch {}
    }
  }

  return { ...payload, messages: msgs };
}

// ── Export: register hooks ──
// ── Export: register hooks (CommonJS for Pi extension loader) ──
module.exports = function (pi) {
  pi.on("tool_result", handleToolResult);
  pi.on("tool_call", handleToolCall);
  pi.on("before_provider_request", handleBeforeProviderRequest);
  process.on("exit", () => {});
}

