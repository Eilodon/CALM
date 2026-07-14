// Real cross-SDK MCP interop check for CALM's `calm serve` (2026-07-14
// upgrade item). Ported from the pattern in modelcontextprotocol/rust-sdk's
// own `crates/rmcp/tests/test_with_js.rs` (verified against that real
// source before porting) — that repo spawns a Node MCP server/client to
// prove rmcp's own Rust client/server interoperates with an independent
// SDK implementation. CALM's CI never had the equivalent for CALM's own
// server half: every existing test (Rust unit/integration tests, the bash
// hook suite) drives `calm serve` with rmcp's own client code — the same
// SDK, same author, on both sides of the wire. A protocol-shape bug that
// rmcp's client tolerates or a wire-format assumption CALM shares with
// rmcp but the spec doesn't actually require would never surface. This
// script is the other half: the official TypeScript MCP SDK
// (@modelcontextprotocol/sdk, a genuinely independent implementation)
// drives a real `calm serve` child process over real stdio.
//
// Usage: node client.mjs <path-to-calm-binary> <project-root-to-index>
//
// Exit 0 and print nothing but progress on success; exit 1 with a message
// identifying exactly which check failed otherwise — this is the CI gate
// itself, not just a smoke test a human reads.

import { Client } from "@modelcontextprotocol/sdk/client/index.js";
import { StdioClientTransport } from "@modelcontextprotocol/sdk/client/stdio.js";

const [, , calmBinary, projectRoot] = process.argv;
if (!calmBinary || !projectRoot) {
  console.error("usage: node client.mjs <path-to-calm-binary> <project-root>");
  process.exit(1);
}

function fail(message) {
  console.error(`FAIL: ${message}`);
  process.exit(1);
}

// `--preset orient` (not `full`) is deliberate: it doubles this interop
// check as an end-to-end verification that CALM's tool-filtering (the
// toolset/preset registry) actually reaches a real, independent client's
// `tools/list` response — not just CALM's own Rust-side unit tests of the
// same router (`filtered_tool_list_matches_preset_tools_for_named_presets`
// in tools.rs), which never cross a real process/wire boundary.
const transport = new StdioClientTransport({
  command: calmBinary,
  args: ["serve", "--project-root", projectRoot, "--preset", "orient"],
  stderr: "pipe",
});

const client = new Client(
  { name: "calm-js-interop-check", version: "1.0.0" },
  { capabilities: {} },
);

let stderrOutput = "";

try {
  await client.connect(transport);
  if (transport.stderr) {
    transport.stderr.on("data", (chunk) => {
      stderrOutput += chunk.toString();
    });
  }

  const { tools } = await client.listTools();
  if (!Array.isArray(tools) || tools.length === 0) {
    fail(`tools/list returned no tools: ${JSON.stringify(tools)}`);
  }

  const names = tools.map((t) => t.name);
  if (!names.includes("repo_overview")) {
    fail(`expected "orient" preset to include repo_overview, got: ${names.join(", ")}`);
  }
  // "edit" toolset tools are NOT in the "orient" preset (see
  // calm-server/src/tools/common.rs::preset_tools) — a real client seeing
  // them here would mean preset filtering isn't actually reaching the
  // wire, only CALM's own in-process test harness.
  if (names.includes("edit_symbol")) {
    fail(`"orient" preset must not expose edit_symbol, got: ${names.join(", ")}`);
  }

  for (const tool of tools) {
    if (!tool.inputSchema || tool.inputSchema.type !== "object") {
      fail(`tool ${tool.name} has no valid inputSchema: ${JSON.stringify(tool.inputSchema)}`);
    }
  }

  const result = await client.callTool({ name: "repo_overview", arguments: {} });
  if (result.isError) {
    fail(`repo_overview call returned isError=true: ${JSON.stringify(result)}`);
  }
  if (!Array.isArray(result.content) || result.content.length === 0) {
    fail(`repo_overview call returned no content: ${JSON.stringify(result)}`);
  }

  console.log(`OK: ${tools.length} tools listed via "orient" preset, repo_overview call succeeded.`);
} catch (err) {
  fail(`${err?.stack ?? err}${stderrOutput ? `\n--- calm serve stderr ---\n${stderrOutput}` : ""}`);
} finally {
  await client.close().catch(() => {});
}

process.exit(0);
