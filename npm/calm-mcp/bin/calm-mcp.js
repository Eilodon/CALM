#!/usr/bin/env node
'use strict';

// Thin exec wrapper — the real work is the Rust `calm` binary shipped by one
// of the platform packages below (@eilodon/calm-mcp-<platform>), selected via
// optionalDependencies + npm's os/cpu matching at install time. This file
// only resolves which one landed in node_modules and runs it, forwarding
// argv and stdio untouched (MCP talks JSON-RPC over stdio — nothing here
// may write to stdout).
//
// Uses `spawn` (async) plus explicit signal forwarding, not `spawnSync`:
// `spawnSync(..., {stdio: 'inherit'})` blocks this process until the child
// exits, but does NOT tie the child's lifetime to this process's — if an MCP
// client kills only this Node PID (SIGTERM is `ChildProcess.kill()`'s
// default, and what most editors/process managers send), the `calm` child
// never sees that signal and is orphaned: it keeps running, keeps its DB
// connection and possibly the indexer lock, and is a real contributor to
// unbounded WAL growth (SQLite's auto-checkpoint is starved by any
// connection still holding an old snapshot open). Forwarding the signal here
// lets the same graceful-shutdown path a directly-run `calm` binary gets
// (SIGINT/SIGTERM handling in calm-server, including a WAL checkpoint on the
// way out) actually run instead of leaving an orphan behind.

const { spawn } = require('node:child_process');
const path = require('node:path');
const os = require('node:os');

const PLATFORM_PACKAGES = {
  'linux-x64': '@eilodon/calm-mcp-linux-x64',
  'linux-arm64': '@eilodon/calm-mcp-linux-arm64',
  'darwin-arm64': '@eilodon/calm-mcp-darwin-arm64',
};

function resolveBinary() {
  const pkgName = PLATFORM_PACKAGES[`${process.platform}-${process.arch}`];
  if (!pkgName) return null;
  try {
    const pkgJsonPath = require.resolve(`${pkgName}/package.json`);
    return path.join(path.dirname(pkgJsonPath), 'calm');
  } catch {
    return null;
  }
}

const binPath = resolveBinary();
if (!binPath) {
  const key = `${process.platform}-${process.arch}`;
  process.stderr.write(
    `[calm-mcp] no prebuilt binary for ${key}. Supported today: ${Object.keys(PLATFORM_PACKAGES).join(', ')}.\n` +
      '[calm-mcp] build from source instead: git clone https://github.com/Eilodon/CALM, ' +
      "'then 'cargo build --release --bin calm'.\n"
  );
  process.exit(1);
}

const child = spawn(binPath, process.argv.slice(2), { stdio: 'inherit' });

child.on('error', (err) => {
  process.stderr.write(`[calm-mcp] failed to run ${binPath}: ${err.message}\n`);
  process.exit(1);
});

// Relay every signal that could otherwise kill just this wrapper and orphan
// the child. Registering a handler here also suppresses Node's own default
// disposition for that signal (normally "exit immediately") — intentional:
// we want to wait for the child's `exit` event (below) so its own graceful
// shutdown, WAL checkpoint included, gets a chance to finish before this
// wrapper process disappears too.
let killTimer = null;
for (const sig of ['SIGINT', 'SIGTERM', 'SIGHUP']) {
  process.on(sig, () => {
    child.kill(sig);
    // Safety net: don't hang the MCP client's teardown forever if the child
    // doesn't exit on its own (stuck mid-shutdown, or a signal it ignores).
    // One-shot — a second signal delivery just re-sends, it doesn't reset
    // the clock.
    if (!killTimer) {
      killTimer = setTimeout(() => child.kill('SIGKILL'), 8000);
      killTimer.unref();
    }
  });
}

child.on('exit', (code, signal) => {
  if (killTimer) clearTimeout(killTimer);
  if (signal) {
    process.exit(128 + (os.constants.signals[signal] || 0));
    return;
  }
  process.exit(code === null ? 1 : code);
});
