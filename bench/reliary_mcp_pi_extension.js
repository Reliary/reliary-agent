// Arc 43 Phase 1 — reliary MCP extension for Pi.
// Persistent stdio MCP client: spawns reliary ONCE per Pi session, reuses the
// process for all tool calls. This mirrors how Pi loads MCP servers normally
// and removes the per-call 7s subprocess spawn overhead.

import { spawn } from "child_process";

const RELIARY_BIN = process.env.RELIARY_BIN || "/home/user/src/reliary8/target/release/reliary";
const WORKDIR = process.env.RELIARY_WORKDIR || process.cwd();

let proc = null;
let nextId = 1;
let initPromise = null;
const pending = new Map();

function ensureProc() {
  if (proc && !proc.killed) return initPromise;

  proc = spawn(RELIARY_BIN, ["mcp"], {
    cwd: WORKDIR,
    stdio: ["pipe", "pipe", "pipe"],
  });

  let buf = "";
  proc.stdout.on("data", (d) => {
    buf += d.toString();
    let idx;
    while ((idx = buf.indexOf("\n")) >= 0) {
      const line = buf.slice(0, idx).trim();
      buf = buf.slice(idx + 1);
      if (!line) continue;
      try {
        const r = JSON.parse(line);
        if (r.id != null && pending.has(r.id)) {
          const { resolve, reject } = pending.get(r.id);
          pending.delete(r.id);
          if (r.error) reject(new Error(JSON.stringify(r.error)));
          else resolve(r.result);
        }
      } catch (e) {}
    }
  });

  proc.on("close", () => {
    for (const { reject } of pending.values())
      reject(new Error("reliary process closed"));
    pending.clear();
    proc = null;
    initPromise = null;
  });

  proc.on("error", () => {
    proc = null;
    initPromise = null;
  });

  // MCP requires an initialize handshake before tools/list or tools/call.
  initPromise = sendRequest("initialize", {
    protocolVersion: "2024-11-05",
    capabilities: {},
    clientInfo: { name: "reliary-pi-extension", version: "0.2.0" },
  }).then(() => sendRequest("notifications/initialized", {}));

  return initPromise;
}

function sendRequest(method, params) {
  // JSON-RPC notifications have no id and expect no response.
  // Send them fire-and-forget so they don't block on the 90s timeout.
  const isNotification = method === "notifications/initialized" ||
                          method.startsWith("notifications/");
  if (isNotification) {
    if (!proc || proc.killed) return Promise.resolve(null);
    try {
      proc.stdin.write(JSON.stringify({ jsonrpc: "2.0", method, params }) + "\n");
    } catch (e) {}
    return Promise.resolve(null);
  }
  return new Promise((resolve, reject) => {
    if (!proc || proc.killed) {
      reject(new Error("reliary process not running"));
      return;
    }
    const id = nextId++;
    pending.set(id, { resolve, reject });
    const msg = JSON.stringify({ jsonrpc: "2.0", id, method, params }) + "\n";
    try {
      proc.stdin.write(msg);
    } catch (e) {
      pending.delete(id);
      reject(e);
      return;
    }
    setTimeout(() => {
      if (pending.has(id)) {
        pending.delete(id);
        reject(new Error("reliary timeout"));
      }
    }, 90000);
  });
}

// Warm up: make a fast query after initialize so the first real query doesn't hit
// the 30s timeout from cold start.
let _warmedUp = false;
async function warmUp() {
  if (_warmedUp) return;
  try {
    await ensureProc();
    await sendRequest("tools/call", {
      name: "reliary_find_references_with_source",
      arguments: { name: "main", path: ".", format: "grep", limit: 1, anchor_file: "", anchor_line: 0 }
    });
    _warmedUp = true;
  } catch (e) {
    // warmup failure is non-fatal — first real call will still work
    _warmedUp = true;
  }
}

async function callTool(tool, args) {
  // Don't await warmUp — it runs in the background. Just wait for the
  // initialize handshake to complete (fast, ~7ms). Then proceed.
  if (initPromise) { try { await initPromise; } catch (e) {} }
  // If proc not alive (warmup finished but errored), ensure it now.
  await ensureProc();
  // Normalize the corpus path. The LLM sometimes passes:
  //   - "." (good)
  //   - "/tmp/tokio-corpus/tokio/src" (absolute corpus workdir) — map to "."
  //   - "/tmp/tokio-corpus/tokio/src/io/util/chain.rs" (a FILE, not a dir)
  //     → strip to "/tmp/tokio-corpus/tokio/src" or "."
  //   - "/home/user/..." (Pi's CWD or our binary) — not in corpus
  const wd = process.env.RELIARY_WORKDIR || ".";
  const wdPrefix = wd.replace(/\/$/, "");
  if (!args.path) {
    args.path = ".";
  } else if (wdPrefix && args.path.startsWith(wdPrefix)) {
    // Path is within (or equal to) the corpus workdir. Trim to relative.
    args.path = args.path.slice(wdPrefix.length).replace(/^\//, "") || ".";
  } else if (args.path.startsWith("/home/user") || args.path.startsWith("/tmp/")) {
    // Out-of-corpus absolute path. Use corpus workdir.
    args.path = ".";
  }
  return await sendRequest("tools/call", { name: tool, arguments: args });
}

function extractText(result) {
  if (!result) return "";
  if (result.content && Array.isArray(result.content) && result.content.length > 0) {
    return result.content[0].text || "";
  }
  return typeof result === "string" ? result : JSON.stringify(result);
}

export default function (pi) {
  // Arc 43 Phase 1: warm up the MCP process on extension load so the first tool
  // call doesn't exceed Pi's ~30s tool-execution timeout. Cold SQLite + lazy
  // table JIT takes ~28s — right at Pi's timeout edge.
  // Strategy: fire-and-forget warmup. If it's still running when the first
  // tool call arrives, the 90s timeout in sendRequest handles it.

  // PRIMARY find-references tool — auto-anchor, grep format.
  pi.registerTool({
    name: "reliary_find_references",
    description:
      "Find all references to a symbol. Grammar-free, type-aware. Pass format='grep' for plain file:line: code lines (LLM-friendly). anchor_file is optional — pass empty string when you don't know where the symbol is defined.",
    parameters: {
      type: "object",
      properties: {
        name: { type: "string", description: "Symbol name" },
        anchor_file: { type: "string", description: "Optional. Empty string = auto-discover." },
        anchor_line: { type: "integer", description: "Optional. 0 = auto-discover." },
        threshold: { type: "number", description: "Min similarity 0-1, default 0.3" },
        format: { type: "string", enum: ["json", "grep"], description: "Output format" },
        limit: { type: "integer", description: "Max hits to return" },
        path: { type: "string", description: "Repo root, default '.'" },
      },
      required: ["name"],
    },
    async execute(_id, args) {
      try {
        process.stderr.write("[ext] callTool find_references " + args.name + "\n");
        const r = await callTool("reliary_find_references_with_source", {
          name: args.name,
          anchor_file: args.anchor_file || "",
          anchor_line: args.anchor_line || 0,
          threshold: args.threshold ?? 0.3,
          format: args.format || "grep",
          limit: args.limit ?? 30,
          path: args.path || ".",
        });
        const text = extractText(r);
        return { content: [{ type: "text", text: text || "(no references)" }], details: r };
      } catch (e) {
        return { content: [{ type: "text", text: "ERROR: " + e.message }] };
      }
    },
  });

  // Callgraph — grammar-free body extraction with multi-hop expansion.
  pi.registerTool({
    name: "reliary_callgraph",
    description:
      "Show callers and callees of a function. Grammar-free body extraction — extracts callees directly from function bodies, no AST. Returns source preview, callees with definition sites, and callers. Pass name only — anchor is auto-discovered.",
    parameters: {
      type: "object",
      properties: {
        name: { type: "string", description: "Function name (e.g. 'block_on', 'Runtime::block_on')" },
        path: { type: "string", description: "Repo root, default '.'" },
      },
      required: ["name"],
    },
    async execute(_id, args) {
      try {
        const r = await callTool("reliary_callgraph_v2", {
          name: args.name,
          path: args.path || ".",
        });
        const text = extractText(r);
        return { content: [{ type: "text", text }], details: r };
      } catch (e) {
        return { content: [{ type: "text", text: "ERROR: " + e.message }] };
      }
    },
  });

  // Methods-on-type — grammar-free impl-block enumeration.
  pi.registerTool({
    name: "reliary_methods_on",
    description:
      "Enumerate all methods declared on a type. Grammar-free — scans all source files for `impl Type` and `impl Trait for Type` blocks. Returns method names with file:line:source.",
    parameters: {
      type: "object",
      properties: {
        name: { type: "string", description: "Type name (e.g. 'BufWriter', 'SemaphorePermit')" },
      },
      required: ["name"],
    },
    async execute(_id, args) {
      try {
        const r = await callTool("reliary_methods_on", { name: args.name });
        const text = extractText(r);
        return { content: [{ type: "text", text }], details: r };
      } catch (e) {
        return { content: [{ type: "text", text: "ERROR: " + e.message }] };
      }
    },
  });

  // BM25 search.
  pi.registerTool({
    name: "reliary_search",
    description:
      "BM25 full-text search across the indexed codebase. Use when you don't know the exact symbol name.",
    parameters: {
      type: "object",
      properties: {
        query: { type: "string" },
        path: { type: "string" },
      },
      required: ["query"],
    },
    async execute(_id, args) {
      try {
        const r = await callTool("reliary_search", {
          query: args.query,
          path: args.path || ".",
        });
        const text = extractText(r);
        return { content: [{ type: "text", text }], details: r };
      } catch (e) {
        return { content: [{ type: "text", text: "ERROR: " + e.message }] };
      }
    },
  });

  // Goto definition — uses find_references auto-anchor, returns def lines first.
  pi.registerTool({
    name: "reliary_goto_def",
    description:
      "Jump to the definition site of a symbol. Returns file:line for the most likely definition.",
    parameters: {
      type: "object",
      properties: {
        name: { type: "string" },
      },
      required: ["name"],
    },
    async execute(_id, args) {
      try {
        const r = await callTool("reliary_find_references_with_source", {
          name: args.name,
          anchor_file: "",
          anchor_line: 0,
          format: "grep",
          limit: 5,
          path: ".",
        });
        const text = extractText(r);
        const needle = "fn " + args.name + "(";
        const defs = text.split("\n").filter((l) => l.includes(needle));
        return {
          content: [{ type: "text", text: defs.length ? defs.join("\n") : text.slice(0, 500) }],
          details: r,
        };
      } catch (e) {
        return { content: [{ type: "text", text: "ERROR: " + e.message }] };
      }
    },
  });

  // V53: describe — explain a symbol: purpose, signature, location, callers, methods.
  // Gives the model context about what a symbol does before editing it.
  pi.registerTool({
    name: "reliary_describe",
    description:
      "Explain a symbol: purpose, signature, location, callers, and methods. Use this BEFORE editing — gives the model context about what a symbol does and where it's used. Set methods=true to list methods on a type. Set dead_only=true with path to find dead code in a module.",
    parameters: {
      type: "object",
      properties: {
        name: { type: "string", description: "Symbol to describe (e.g. 'block_on', 'Sleep', 'BraceNode')" },
        file: { type: "string", description: "File path to describe (alternative to name)" },
        path: { type: "string", description: "Working directory, default '.'" },
        methods: { type: "boolean", default: false, description: "List methods on a type" },
        dead_only: { type: "boolean", default: false, description: "Find dead code in a module (set path)" },
        limit: { type: "integer", default: 30 },
      },
    },
    async execute(_id, args) {
      try {
        if (args.dead_only) {
          const r = await callTool("reliary_dead_symbols", {
            path: args.path || ".",
            limit: args.limit || 30,
            functions_only: true,
          });
          const text = extractText(r);
          return {
            content: [{ type: "text", text: text.slice(0, 4000) }],
            details: r,
          };
        }
        const callArgs = { name: args.name, path: args.path || ".", methods: !!args.methods };
        if (args.file) callArgs.file = args.file;
        const r = await callTool("reliary_describe", callArgs);
        const text = extractText(r);
        return {
          content: [{ type: "text", text: text.slice(0, 5000) }],
          details: r,
        };
      } catch (e) {
        return { content: [{ type: "text", text: "ERROR: " + e.message }] };
      }
    },
  });
}