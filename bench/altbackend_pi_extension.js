// Arc 28 Lever 6 — Pi extension for altbackend-mcp.
// Wraps the MCP stdio binary as Pi tools. Strips verbose fields to reduce
// context bloat (altbackend returns fp/sp/bt signatures which LLMs don't need).

import { spawn } from "child_process";

const ALTBACKEND_BIN = process.env.ALTBACKEND_BIN || `${process.env.HOME}/.local/bin/codebase-memory-mcp`;
const PROJECT = process.env.ALTBACKEND_PROJECT || "tmp-rel8-corpus";

let proc = null;
let nextId = 1;
const pending = new Map();

function ensureProc() {
  if (proc && !proc.killed) return;
  proc = spawn(ALTBACKEND_BIN, [], { stdio: ["pipe", "pipe", "pipe"] });
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
      reject(new Error("altbackend process closed"));
    pending.clear();
    proc = null;
  });
  proc.on("error", () => {
    proc = null;
  });
  sendRequest("initialize", {
    protocolVersion: "2024-11-05",
    capabilities: {},
    clientInfo: { name: "altbackend-pi-extension", version: "0.1" },
  });
}

function sendRequest(method, params) {
  return new Promise((resolve, reject) => {
    const id = nextId++;
    pending.set(id, { resolve, reject });
    const msg = JSON.stringify({ jsonrpc: "2.0", id, method, params }) + "\n";
    proc.stdin.write(msg);
    setTimeout(() => {
      if (pending.has(id)) {
        pending.delete(id);
        reject(new Error("altbackend timeout"));
      }
    }, 30000);
  });
}

async function callTool(tool, args) {
  ensureProc();
  await new Promise((r) => setTimeout(r, 200));
  return await sendRequest("tools/call", { name: tool, arguments: args });
}

// Strip verbose fields from ALTBACKEND output to reduce LLM context bloat.
function stripAltbackendFields(parsed) {
  const KEEP_FIELDS = new Set([
    "name", "qualified_name", "label", "file_path", "start_line",
    "end_line", "is_exported", "is_test", "complexity", "lines",
    "signature", "in_degree", "out_degree", "return_type", "param_types",
    "path", "callers", "callees", "total", "has_more", "packages",
    "edges", "nodes", "services", "clusters", "hotspots", "rows",
    "summary", "results", "functions", "classes", "interfaces", "enums",
    "errors", "warnings", "fingerprint",
  ]);
  function clean(obj) {
    if (Array.isArray(obj)) return obj.map(clean);
    if (obj && typeof obj === "object") {
      const out = {};
      for (const [k, v] of Object.entries(obj)) {
        if (KEEP_FIELDS.has(k)) out[k] = clean(v);
      }
      return out;
    }
    return obj;
  }
  return clean(parsed);
}

export default function (pi) {
  pi.registerTool({
    name: "altbackend_search_graph",
    description:
      "altbackend-mcp: structured graph search by label/name pattern/file pattern. Use INSTEAD OF grep for code definitions. Returns nodes with qualified_name, file_path, etc.",
    parameters: {
      type: "object",
      properties: {
        query: { type: "string", description: "Natural-language BM25 search" },
        name_pattern: { type: "string", description: "Regex name match" },
        label: { type: "string", description: "Function/Method/Class/etc" },
        file_pattern: { type: "string", description: "File path glob" },
        limit: { type: "integer", description: "Max results, default 20" },
      },
    },
    async execute(_id, args) {
      try {
        const r = await callTool("search_graph",
          { project: PROJECT, limit: 20, ...args });
        const text = r?.content?.[0]?.text || JSON.stringify(r);
        try {
          const parsed = JSON.parse(text);
          const cleaned = stripAltbackendFields(parsed);
          return { content: [{ type: "text", text: JSON.stringify(cleaned) }],
                   details: cleaned };
        } catch {
          return { content: [{ type: "text", text }], details: r };
        }
      } catch (e) {
        return { content: [{ type: "text", text: "ERROR: " + e.message }] };
      }
    },
  });

  pi.registerTool({
    name: "altbackend_trace_path",
    description:
      "altbackend-mcp: trace callers/callees via CALLS edges. Direction: inbound (callers), outbound (callees), both.",
    parameters: {
      type: "object",
      properties: {
        function_name: { type: "string" },
        direction: { type: "string", enum: ["inbound", "outbound", "both"] },
        depth: { type: "integer", default: 2 },
      },
      required: ["function_name"],
    },
    async execute(_id, args) {
      try {
        const r = await callTool("trace_path",
          { project: PROJECT, ...args });
        const text = r?.content?.[0]?.text || JSON.stringify(r);
        try {
          const parsed = JSON.parse(text);
          // Truncate to top-15 callers/callees to keep context small.
          if (parsed.callers) parsed.callers = parsed.callers.slice(0, 15);
          if (parsed.callees) parsed.callees = parsed.callees.slice(0, 15);
          const cleaned = stripAltbackendFields(parsed);
          return { content: [{ type: "text", text: JSON.stringify(cleaned) }],
                   details: cleaned };
        } catch {
          return { content: [{ type: "text", text }], details: r };
        }
      } catch (e) {
        return { content: [{ type: "text", text: "ERROR: " + e.message }] };
      }
    },
  });

  pi.registerTool({
    name: "altbackend_search_code",
    description:
      "altbackend-mcp: graph-augmented code search. Grep + dedupes into containing functions. Modes: compact (signatures), files (just paths).",
    parameters: {
      type: "object",
      properties: {
        pattern: { type: "string", description: "Text pattern" },
        file_pattern: { type: "string", description: "Glob e.g. *.rs" },
        mode: { type: "string", enum: ["compact", "files"] },
        limit: { type: "integer", default: 10 },
      },
      required: ["pattern"],
    },
    async execute(_id, args) {
      try {
        const r = await callTool("search_code",
          { project: PROJECT, mode: "files", limit: 30, ...args });
        const text = r?.content?.[0]?.text || JSON.stringify(r);
        try {
          const parsed = JSON.parse(text);
          const cleaned = stripAltbackendFields(parsed);
          return { content: [{ type: "text", text: JSON.stringify(cleaned) }],
                   details: cleaned };
        } catch {
          return { content: [{ type: "text", text }], details: r };
        }
      } catch (e) {
        return { content: [{ type: "text", text: "ERROR: " + e.message }] };
      }
    },
  });

  pi.registerTool({
    name: "altbackend_get_architecture",
    description:
      "altbackend-mcp: high-level architecture overview — packages, services, dependencies, hotspots, clusters.",
    parameters: {
      type: "object",
      properties: {
        aspects: {
          type: "array",
          items: { type: "string" },
          description: "e.g. ['packages', 'clusters', 'hotspots']",
        },
      },
    },
    async execute(_id, args) {
      try {
        const r = await callTool("get_architecture",
          { project: PROJECT, ...args });
        const text = r?.content?.[0]?.text || JSON.stringify(r);
        try {
          const parsed = JSON.parse(text);
          // Truncate top packages to 10.
          if (parsed.packages) parsed.packages = parsed.packages.slice(0, 10);
          if (parsed.clusters) parsed.clusters = parsed.clusters.slice(0, 10);
          const cleaned = stripAltbackendFields(parsed);
          return { content: [{ type: "text", text: JSON.stringify(cleaned) }],
                   details: cleaned };
        } catch {
          return { content: [{ type: "text", text }], details: r };
        }
      } catch (e) {
        return { content: [{ type: "text", text: "ERROR: " + e.message }] };
      }
    },
  });

  pi.registerTool({
    name: "altbackend_get_code_snippet",
    description:
      "altbackend-mcp: read source code by qualified name. First call altbackend_search_graph to find the qualified_name.",
    parameters: {
      type: "object",
      properties: {
        qualified_name: { type: "string" },
      },
      required: ["qualified_name"],
    },
    async execute(_id, args) {
      try {
        const r = await callTool("get_code_snippet",
          { project: PROJECT, ...args });
        const text = r?.content?.[0]?.text || JSON.stringify(r);
        return { content: [{ type: "text", text: text.slice(0, 2000) }],
                 details: r };
      } catch (e) {
        return { content: [{ type: "text", text: "ERROR: " + e.message }] };
      }
    },
  });
}