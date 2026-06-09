import { execFileSync, spawnSync } from "child_process";
import { existsSync, readFileSync, writeFileSync, readdirSync, statSync, unlinkSync } from "fs";
import { createHash } from "crypto";

const GATE_VERSION = "2.1.0";

// ── Binary discovery ──
const RELIARY_CANDIDATES = [
  (() => { try { return new URL(".", import.meta.url).pathname.replace(/pi\/.+$/, "bin/reliary-agent"); } catch { return null; } })(),
  "/usr/local/bin/reliary-agent",
  "/usr/bin/reliary-agent",
  `${process.env.HOME}/.local/bin/reliary-agent`,
  `${process.env.HOME}/.cargo/bin/reliary-agent`,
];
let RELIARY_BIN = RELIARY_CANDIDATES.find(c => c && existsSync(c));

console.error(`[gate] ✓ v${GATE_VERSION} — reliary: ${!!RELIARY_BIN}${RELIARY_BIN ? ` (${RELIARY_BIN.split("/").pop()})` : " none"}`);

// ── State ──
let sessionTurns = 1;
let gateRunId = Date.now().toString(36) + Math.random().toString(36).slice(2, 5);
let blockedCount = 0;
const sentinel = "<!-- gate-intercepted -->";

// ── Daemon command helper ──
function gateCmd(cmd) {
  if (!RELIARY_BIN) return null;
  try {
    const r = execFileSync(RELIARY_BIN, cmd.split(" "), {
      encoding: "utf-8", timeout: 3000, maxBuffer: 4096,
    });
    return r.trim();
  } catch { return null; }
}

// ── Quality gate log ──
function gateLog(level, msg) {
  console.error(`[gate] ${level}: ${gateRunId} ${msg}`);
}

// ── IR reasoning compression via reliary ──
function reliaryCompress(text) {
  if (!RELIARY_BIN || text.length < 200) return null;
  try {
    const r = spawnSync(RELIARY_BIN, ["-f", "compact", "compress", text.substring(0, 1000)], {
      encoding: "utf-8", timeout: 3000, maxBuffer: 2048,
    });
    if (r.status !== 0 || !r.stdout) return null;
    const b = r.stdout.trim();
    const compressedLen = b.length;
    return (b && compressedLen < text.length * 0.85) ? b : null;
  } catch { return null; }
}

function reliaryGentleCompress(text) {
  if (!RELIARY_BIN || text.length < 200) return null;
  try {
    const r = spawnSync(RELIARY_BIN, ["-f", "compact", "compress", "--gentle", text.substring(0, 1000)], {
      encoding: "utf-8", timeout: 3000, maxBuffer: 2048,
    });
    if (r.status !== 0 || !r.stdout) return null;
    const b = r.stdout.trim();
    const compressedLen = b.length;
    return (b && compressedLen < text.length * 0.85) ? b : null;
  } catch { return null; }
}

function reliaryRisk(filePath) {
  if (!RELIARY_BIN) return null;
  try {
    const r = execFileSync(RELIARY_BIN, ["-f", "json", "risk", filePath], {
      encoding: "utf-8", timeout: 3000, maxBuffer: 2048,
    });
    if (r.status !== 0) return null;
    const parsed = JSON.parse(r.stdout);
    const risk = parsed?.risk || parsed?.result?.risk || parsed?.level || "";
    const reason = parsed?.reason || parsed?.result?.reason || "";
    if (risk === "LOW" || risk === "low") return null;
    return { risk, reason: reason || `risk level: ${risk}` };
  } catch { return null; }
}

function reliaryCacheRead(path, hash, size) {
  if (!RELIARY_BIN) return;
  try { execFileSync(RELIARY_BIN, ["cache-read", path, hash, String(size)], {
    encoding: "utf-8", timeout: 2000, maxBuffer: 256,
  }); } catch {}
}

// ── Hook A: tool_result — compress read/bash if appropriate ──
function handleToolResult(event) {
  if (event.isError) return;
  const msg = event.input;
  const name = msg?.toolName || event.toolName;

  if (name === "read") {
    const pathHint = msg?.path || msg?.input?.path || "";
    if (!pathHint) return;
    const text = extractMessageText(event);
    if (!text || text.length < 500) return;

    // Dedup cache: if same file with same hash, return marker
    const hash = createHash("sha256").update(text).digest("hex").slice(0, 16);
    const cacheDir = `${process.env.HOME}/.cache/gate-read`;
    const cacheFile = `${cacheDir}/${hash.slice(0, 8)}`;
    try {
      readdirSync(cacheDir);
    } catch {
      try { execFileSync("mkdir", ["-p", cacheDir]); } catch {}
    }
    const cached = existsSync(cacheFile) ? readFileSync(cacheFile, "utf-8") : "";
    const cachedHash = cached ? cached.split("|")[0] : "";
    const priorLen = cached ? parseInt(cached.split("|")[1] || "0") : 0;

    if (cachedHash && priorLen > 0 && priorLen === text.length) {
      gateLog("save", `dedup: ${pathHint} (hash match, ${priorLen}c)`);
      event.content = [{ type: "text", text: `[reliary: ${hash.slice(0,8)}] ${pathHint} — unchanged (${priorLen} chars)` }];
      return;
    }

    // Cache this content
    try { writeFileSync(cacheFile, `readFile`); } catch {}

    // Compress large exploratory reads
    if (text.length > 2000) {
      const lines = text.split("\n");
      const head = lines.slice(0, 60);
      const tail = lines.slice(-20);
      const compressed = head.join("\n") + `\n[... ${lines.length - 80} lines omitted ...]\n` + tail.join("\n");
      gateLog("save", `zone: ${pathHint} ${text.length}→${compressed.length}c (${Math.round((1 - compressed.length / text.length) * 100)}%)`);
      event.content = [{ type: "text", text: compressed }];
      return;
    }
  }

  // Bash large output compression via sift-like zone truncation
  if (name === "bash") {
    const text = extractMessageText(event);
    if (!text || text.length < 1000) return;

    const lines = text.split("\n");
    // Collapse repeated blank lines
    const collapsed = [];
    let blankRun = 0;
    for (const line of lines) {
      if (line.trim() === "") { blankRun++; if (blankRun > 2) continue; }
      else { blankRun = 0; }
      collapsed.push(line);
    }
    const collapsedText = collapsed.join("\n");
    const compressed = reliaryCompress(collapsedText);

    if (compressed && compressed.length < collapsedText.length * 0.85) {
      gateLog("save", `bash: ${collapsedText.length}→${compressed.length}c (${Math.round((1 - compressed.length / collapsedText.length) * 100)}%)`);
      event.content = [{ type: "text", text: compressed }];
    }
  }
}

// ── Hook B: tool_call — reliary safety gate ──
function handleToolCall(event) {
  const toolName = event.toolName || event.name;
  const input = event.input || event.arguments || event;
  
  // ── Self-healing edits: intercept edit tool calls ──
  if (toolName === "edit" && RELIARY_BIN) {
    const args = input.edits ? input : input.arguments || input;
    const file = args.file || args.path || "";
    const editsArr = args.edits || [];
    const workdir = args.cwd || process.cwd();
    if (file && editsArr.length > 0 && editsArr[0].oldText && editsArr[0].newText) {
      const oldText = editsArr[0].oldText;
      const newText = editsArr[0].newText;
      
      // Read original file, apply old→new replacement, write full content to tmp
      try {
        const original = readFileSync(file, "utf-8");
        const modified = original.replace(oldText, newText);
        if (modified === original) {
          gateLog("warn", `heal-edit: no match for oldText in ${file}`);
          return; // fall through
        }
        const tmpFile = `/tmp/reliary-edit-${Date.now()}.tmp`;
        writeFileSync(tmpFile, modified, "utf-8");
        const result = gateCmd(`apply-edit ${file} ${tmpFile} ${workdir}`);
        try { unlinkSync(tmpFile); } catch {}
        
        if (result) {
          gateLog("heal", `${file}: ${result.substring(0, 60)}`);
          return { block: true, response: result };
        }
      } catch (e) {
        gateLog("warn", `heal-edit error: ${e.message}`);
      }
    }
  }
  
  // ── Reliary safety gate: warn on risky reads (edits already handled above) ──
  if (toolName === "read") {
    const path = input.path || event.path || "";
    if (!path || path.startsWith("/tmp") || path.startsWith("/dev")) return;

    const result = reliaryRisk(path);
    if (!result) return;

    blockedCount++;
    const rf = blockedCount >= 3 ? " (circuit breaker active)" : "";
    const reason = `Reliary risk: ${path} — ${result.reason}.${rf}`;
    gateLog("block", `#${blockedCount}: ${reason}`);
    return { block: true, response: reason };
  }
}

// ── Reasoning compression via reliary (inline heuristics + daemon) ──
function compressReasoning(text) {
  if (!text || text.length < 600) return null;
  if (sessionTurns < 3) return null;
  if (text.includes("```") || text.includes("//") || text.includes("/*")
      || text.includes("src/") || text.includes(".rs:") || text.includes(".py:")) return null;
  return reliaryGentleCompress(text);
}

// ── Hook C: before_provider_request — conv window + edit compression + IR reasoning compression ──
function handleBeforeProviderRequest(event) {
  const payload = event.payload;
  if (!payload || !Array.isArray(payload.messages)) return;

  let msgs = payload.messages;

  // Turn counting
  const userCount = msgs.filter(m => m.role === "user").length;
  sessionTurns = userCount;

  // Incremental compression: compress assistant messages before last 2 turns
  for (let i = 0; i < msgs.length - 2; i++) {
    const m = msgs[i];
    if (m.role !== "assistant") continue;
    if (!Array.isArray(m.content)) continue;
    for (const block of m.content) {
      if (block.type === "thinking" && block.thinking?.length > 300) {
        const compressed = compressReasoning(block.thinking);
        if (compressed) {
          gateLog("save", `thinking ${block.thinking.length}→${compressed.length}c`);
          block.thinking = compressed;
        }
      }
      if (block.type === "text" && block.text?.length > 400) {
        const compressed = compressReasoning(block.text);
        if (compressed) {
          gateLog("save", `text ${block.text.length}→${compressed.length}c`);
          block.text = compressed;
        }
      }
    }
  }

  // Conv window: collapse old turns at 6+
  msgs = applyConversationWindow(msgs);

  // Edit merge: combine sequential edits to same file
  msgs = compressEditCalls(msgs);

  return { ...payload, messages: msgs };
}

function applyConversationWindow(msgs) {
  const n = msgs.length;
  if (n < 10) return msgs; // need 5+ turns (10 messages) to collapse

  const keepFirst = 2; // keep first user msg + system response
  const keepLast = 6;  // keep last 3 turn pairs
  const middle = msgs.slice(keepFirst, n - keepLast);
  if (middle.length < 2) return msgs;

  const summary = middle
    .filter(m => m.role === "assistant")
    .map(m => {
      let text = "";
      if (Array.isArray(m.content)) {
        for (const b of m.content) {
          if (b.type === "text") text += b.text;
          else if (b.type === "thinking") text += b.thinking;
          else if (b.type === "toolCall") text += `[${b.name || "tool"}]`;
        }
      }
      const compressed = compressReasoning(text);
      return compressed || text.slice(0, 100);
    })
    .filter(Boolean)
    .join(" | ");

  if (!summary) return msgs;
  gateLog("save", `conv-window: collapsed ${middle.length} msgs`);
  return [...msgs.slice(0, keepFirst), { role: "system", content: `[collapsed ${middle.length} prior msgs: ${summary.slice(0, 300)}]` }, ...msgs.slice(n - keepLast)];
}

function compressEditCalls(msgs) {
  const n = msgs.length;
  const assistantIdx = [];
  for (let i = 0; i < n; i++) {
    if (msgs[i].role === "assistant") assistantIdx.push(i);
  }
  if (assistantIdx.length < 2) return msgs;

  // Check if last 2 assistant messages both have edits on the same file
  const last = msgs[assistantIdx[assistantIdx.length - 1]];
  const prev = msgs[assistantIdx[assistantIdx.length - 2]];
  const lastEdits = extractEditCalls(last);
  const prevEdits = extractEditCalls(prev);
  if (lastEdits.size === 0 || prevEdits.size === 0) return msgs;
  if (![...lastEdits].some(file => prevEdits.has(file))) return msgs;

  // Merge: remove previous tool results+assistant if same file edited
  gateLog("save", "edit-merge: combining sequential edits");
  const keep = [...assistantIdx.slice(0, -2), assistantIdx[assistantIdx.length - 1]];
  return keep.map(i => msgs[i]);
}

function extractEditCalls(msg) {
  const files = new Set();
  if (!Array.isArray(msg.content)) return files;
  for (const b of msg.content) {
    if (b.type === "toolCall" && b.name === "edit" && b.arguments?.file) {
      files.add(b.arguments.file);
    }
  }
  return files;
}

function extractMessageText(event) {
  const c = event.content;
  if (!c) return null;
  if (typeof c === "string") return c;
  if (Array.isArray(c)) return c.map(b => b.text || b.thinking || "").join("\n").trim() || null;
  return null;
}

// ── Extension export ──
export default function (pi) {
  if (pi.on) {
    pi.on("tool_result", (event) => {
      try { handleToolResult(event); } catch (e) { console.error("[gate] tool_result error:", e.message); }
    });
    pi.on("tool_call", (event) => {
      try { return handleToolCall(event); } catch (e) { console.error("[gate] tool_call error:", e.message); }
    });
    pi.on("before_provider_request", (event) => {
      try { return handleBeforeProviderRequest(event); } catch (e) { console.error("[gate] before_provider_request error:", e.message); }
    });
  }
}
