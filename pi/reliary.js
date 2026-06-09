// reliary.js — minimal Pi Agent TCP shim for reliary-agent daemon
// Forwards events to daemon on :9799. Falls through gracefully if daemon unavailable.

import { connect } from "net";

const DAEMON_PORT = 9799;
let conn = null;

function send(cmd) {
  return new Promise((resolve) => {
    if (!conn) {
      const s = connect(DAEMON_PORT, "127.0.0.1", () => {
        s.write(cmd + "\n");
        s.once("data", (d) => { resolve(d.toString().trim()); s.end(); });
      });
      s.setTimeout(2000);
      s.on("error", () => resolve(null));
      s.on("timeout", () => { s.destroy(); resolve(null); });
    }
  });
}

export default function (pi) {
  process.stderr.write("[reliary] pi shim loaded\n");
  // Thin pass-through: all logic lives in the daemon.
  // This shim just forwards events and compresses results.
  pi.on("tool_result", async (event) => {
    // Daemon queries for context enrichment happen here
    // Currently a no-op until daemon supports tool_result processing
  });
}
