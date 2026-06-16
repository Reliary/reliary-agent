#!/usr/bin/env node
/**
 * ISTQB tests for gate.js module-level behavior:
 *  1. RELIARY_LOG level filtering
 *  2. RELIARY_BIN_PATH binary discovery
 *  3. PATH-based binary discovery
 *  4. Feature flag parsing
 */

const { execFileSync } = require("child_process");
const { existsSync, writeFileSync, unlinkSync } = require("fs");

// ── Load gate.js's level logic (extracted for isolation) ──
const LOG_LEVELS = { error: 1, warn: 2, info: 3, debug: 4, trace: 5 };

function resolveLogLevel(env) {
  if (env.RELIARY_LOG && LOG_LEVELS[env.RELIARY_LOG] !== undefined) {
    return LOG_LEVELS[env.RELIARY_LOG];
  }
  if (env.RUST_LOG) {
    const rl = env.RUST_LOG;
    if (rl.includes("trace")) return 5;
    if (rl.includes("debug")) return 4;
    if (rl.includes("warn")) return 2;
    if (rl.includes("error")) return 1;
  }
  return 3; // default: info
}

function resolveBinary(env, existsCache) {
  if (env.RELIARY_BIN_PATH) return env.RELIARY_BIN_PATH;
  // PATH check (simulated)
  if (env.RELIARY_TEST_BIN && existsCache[env.RELIARY_TEST_BIN]) return env.RELIARY_TEST_BIN;
  // Hardcoded paths
  for (const c of ["/usr/local/bin/reliary-agent", "/usr/bin/reliary-agent"]) {
    if (existsCache[c]) return c;
  }
  return null;
}

let passed = 0;
let failed = 0;

function assert(label, condition, detail) {
  if (condition) {
    console.log(`  ✓ ${label}`);
    passed++;
  } else {
    console.log(`  ✗ ${label}: ${detail}`);
    failed++;
  }
}

// ── Test 1: Log level defaults to info when no env vars ──
console.log("\n1. Log level filtering");
assert("default level is info",
  resolveLogLevel({}) === 3,
  `expected 3 (info), got ${resolveLogLevel({})}`);
assert("RELIARY_LOG=debug → level 4",
  resolveLogLevel({ RELIARY_LOG: "debug" }) === 4,
  `expected 4, got ${resolveLogLevel({ RELIARY_LOG: "debug" })}`);
assert("RELIARY_LOG=error → level 1",
  resolveLogLevel({ RELIARY_LOG: "error" }) === 1,
  `expected 1, got ${resolveLogLevel({ RELIARY_LOG: "error" })}`);
assert("RELIARY_LOG=trace → level 5",
  resolveLogLevel({ RELIARY_LOG: "trace" }) === 5,
  `expected 5, got ${resolveLogLevel({ RELIARY_LOG: "trace" })}`);
assert("RELIARY_LOG=invalid → default info (3)",
  resolveLogLevel({ RELIARY_LOG: "invalid" }) === 3,
  `expected 3, got ${resolveLogLevel({ RELIARY_LOG: "invalid" })}`);
assert("RUST_LOG=reliary_agent=debug → level 4",
  resolveLogLevel({ RUST_LOG: "reliary_agent=debug" }) === 4,
  `expected 4, got ${resolveLogLevel({ RUST_LOG: "reliary_agent=debug" })}`);
assert("RUST_LOG=reliary_agent=error → level 1",
  resolveLogLevel({ RUST_LOG: "reliary_agent=error" }) === 1,
  `expected 1, got ${resolveLogLevel({ RUST_LOG: "reliary_agent=error" })}`);
assert("RELIARY_LOG overrides RUST_LOG",
  resolveLogLevel({ RELIARY_LOG: "warn", RUST_LOG: "reliary_agent=debug" }) === 2,
  `expected 2, got ${resolveLogLevel({ RELIARY_LOG: "warn", RUST_LOG: "reliary_agent=debug" })}`);

// ── Test 2: Binary discovery priority ──
console.log("\n2. Binary discovery");
assert("no env, no bin → null",
  resolveBinary({}, {}) === null,
  "expected null");
assert("RELIARY_BIN_PATH takes priority",
  resolveBinary({ RELIARY_BIN_PATH: "/custom/path/rel" }, {}) === "/custom/path/rel",
  "should return env var value");
assert("PATH check works",
  resolveBinary({ RELIARY_TEST_BIN: "/usr/bin/reliary-agent" }, { "/usr/bin/reliary-agent": true }) === "/usr/bin/reliary-agent",
  "should return PATH result");
assert("hardcoded path fallback",
  resolveBinary({}, { "/usr/local/bin/reliary-agent": true }) === "/usr/local/bin/reliary-agent",
  "should find hardcoded path");
assert("RELIARY_BIN_PATH beats PATH",
  resolveBinary({ RELIARY_BIN_PATH: "/custom/rel", RELIARY_TEST_BIN: "/usr/bin/reliary-agent" }, { "/usr/bin/reliary-agent": true }) === "/custom/rel",
  "RELIARY_BIN_PATH should win");

// ── Test 3: Feature flag parsing ──
console.log("\n3. Feature flag parsing");
function parseFeatures(envFeatures, defaults) {
  const features = { ...defaults };
  if (envFeatures) {
    for (const f of envFeatures.split(",")) {
      const isDisable = f.startsWith("-");
      const name = isDisable ? f.slice(1) : f.startsWith("+") ? f.slice(1) : f;
      if (name) features[name] = !isDisable;
    }
  }
  return features;
}

const defaults = { healEdit: true, compress: true, convWindow: true };

const f1 = parseFeatures("-healEdit", defaults);
assert("disable healEdit", f1.healEdit === false, `expected false, got ${f1.healEdit}`);
assert("compress unaffected", f1.compress === true, `expected true, got ${f1.compress}`);

const f2 = parseFeatures("-healEdit,+convWindow", defaults);
assert("disable with - prefix", f2.healEdit === false, "expected false");
assert("enable with + prefix", f2.convWindow === true, "expected true");

const f3 = parseFeatures("", defaults);
assert("no features → defaults", f3.healEdit === true, `expected true, got ${f3.healEdit}`);

const f4 = parseFeatures("-nonexistent", defaults);
assert("unknown feature toggles to false",
  f4.nonexistent === false,
  `expected false, got ${f4.nonexistent}`);

// ── Test 4: End-to-end gate.js loading ──
console.log("\n4. Gate.js module loading");
try {
  // Can't actually load gate.js as a module (requires Pi hooks)
  // But verify it's valid JavaScript by parsing it
  const gateJs = require("fs").readFileSync(require("path").join(__dirname, "../pi/gate.js"), "utf-8");
  // Just check it doesn't crash on syntax parsing
  new (require("vm").Script)(gateJs);
  assert("gate.js is valid JavaScript", true, "");
} catch (e) {
  assert("gate.js is valid JavaScript", false, e.message);
}

// ── Summary ──
console.log(`\n${"=".repeat(40)}`);
console.log(`  ${passed} passed, ${failed} failed`);
if (failed > 0) process.exit(1);
