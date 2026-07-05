#!/usr/bin/env node
'use strict';

// Thin exec wrapper — the real work is the Rust `calm` binary shipped by one
// of the platform packages below (@eilodon/calm-mcp-<platform>), selected via
// optionalDependencies + npm's os/cpu matching at install time. This file
// only resolves which one landed in node_modules and execs it, forwarding
// argv and stdio untouched (MCP talks JSON-RPC over stdio — nothing here
// may write to stdout).

const { spawnSync } = require('node:child_process');
const path = require('node:path');

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

const result = spawnSync(binPath, process.argv.slice(2), { stdio: 'inherit' });
if (result.error) {
  process.stderr.write(`[calm-mcp] failed to run ${binPath}: ${result.error.message}\n`);
  process.exit(1);
}
process.exit(result.status === null ? 1 : result.status);
