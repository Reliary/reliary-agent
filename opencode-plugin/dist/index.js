// src/regen.ts
import { spawn } from "child_process";
import { existsSync } from "fs";
import { dirname, resolve, join } from "path";
var DEFAULT_FILE_PATTERN = /\.(rs|py|ts|tsx|go|c|h|cpp|hpp|js|jsx|java|rb|swift)$/i;
function discoverReliary() {
  if (process.env.RELIARY_BIN && process.env.RELIARY_BIN.trim()) {
    return process.env.RELIARY_BIN.trim();
  }
  const pathDirs = (process.env.PATH || "").split(":");
  for (const name of ["reliary", "reliary-agent"]) {
    for (const dir of pathDirs) {
      const candidate = dir ? dir + "/" + name : name;
      if (existsSync(candidate)) return candidate;
    }
  }
  return null;
}
function findProjectRoot(filePath) {
  if (!filePath) return null;
  let dir = dirname(resolve(filePath));
  for (let i = 0; i < 20; i++) {
    if (existsSync(join(dir, ".reliary", "index.sqlite"))) return dir;
    const parent = dirname(dir);
    if (parent === dir) break;
    dir = parent;
  }
  return null;
}
var pendingReindex = null;
var pendingFiles = /* @__PURE__ */ new Set();
var DEBOUNCE_MS = 500;
function flushReindex(opts) {
  if (pendingFiles.size === 0) return;
  const files = Array.from(pendingFiles);
  pendingFiles.clear();
  pendingReindex = null;
  for (const f of files) {
    triggerReindex(f, opts);
  }
}
function triggerReindex(filePath, opts = {}) {
  const bin = opts.bin ?? discoverReliary();
  if (!bin) return false;
  if (!findProjectRoot(filePath)) return false;
  try {
    const child = spawn(bin, ["reindex-file", "--", filePath], {
      stdio: "ignore",
      detached: true
    });
    child.on("error", (err) => {
      console.error(`[reliary] reindex spawn failed for ${filePath}: ${err.message}`);
    });
    child.unref();
    return true;
  } catch (err) {
    console.error(`[reliary] reindex sync error for ${filePath}: ${err}`);
    return false;
  }
}
function triggerPackRegen(filePath, opts = {}) {
  if (process.env.RELIARY_PACK_REGEN_ON_EDIT !== "1") return false;
  const bin = opts.bin ?? discoverReliary();
  if (!bin) return false;
  const projectRoot = findProjectRoot(filePath);
  if (!projectRoot) return false;
  try {
    const child = spawn(
      bin,
      ["pack", projectRoot, "--format", "l2l3", "--strategy", "full"],
      { stdio: "ignore", detached: true }
    );
    child.on("error", (err) => {
      console.error(`[reliary] pack-regen spawn failed for ${filePath}: ${err.message}`);
    });
    child.unref();
    return true;
  } catch (err) {
    console.error(`[reliary] pack-regen sync error for ${filePath}: ${err}`);
    return false;
  }
}
function onFileEdit(filePath, opts = {}) {
  if (!filePath || typeof filePath !== "string") return { reindexed: false, regenerated: false };
  const pattern = opts.filePattern ?? DEFAULT_FILE_PATTERN;
  if (!pattern.test(filePath)) return { reindexed: false, regenerated: false };
  const bin = opts.bin ?? discoverReliary();
  if (!bin) return { reindexed: false, regenerated: false };
  const resolvedOpts = { ...opts, bin };
  pendingFiles.add(filePath);
  if (pendingReindex) clearTimeout(pendingReindex);
  pendingReindex = setTimeout(() => flushReindex(resolvedOpts), DEBOUNCE_MS);
  const regenerated = triggerPackRegen(filePath, resolvedOpts);
  return { reindexed: true, regenerated };
}
function dispose() {
  if (pendingReindex) {
    clearTimeout(pendingReindex);
    pendingReindex = null;
  }
  if (pendingFiles.size > 0) {
    const files = Array.from(pendingFiles);
    pendingFiles.clear();
    for (const f of files) {
      try {
        const bin = discoverReliary();
        if (!bin) continue;
        spawn(bin, ["reindex-file", "--", f], { stdio: "ignore", detached: true }).unref();
      } catch {
      }
    }
  }
}

// src/index.ts
var WRITE_TOOLS = ["write", "edit", "create", "replace", "patch", "str_replace_edit"];
var ReliaryOpencodePlugin = async () => {
  process.on("exit", dispose);
  const hooks = {
    "tool.execute.after": async (input, _output) => {
      const tool = (input.tool ?? "").toLowerCase();
      if (!WRITE_TOOLS.includes(tool)) return;
      const args = input.args ?? {};
      const candidate = args.file ?? args.path ?? args.filePath;
      if (typeof candidate !== "string") return;
      try {
        onFileEdit(candidate);
      } catch (e) {
        console.error("[reliary-opencode] onFileEdit failed:", e);
      }
    }
  };
  return hooks;
};
var PluginModule = {
  id: "reliary-opencode",
  server: ReliaryOpencodePlugin
};
var index_default = ReliaryOpencodePlugin;
export {
  PluginModule,
  index_default as default
};
// reliary-opencode-plugin built by tsup
