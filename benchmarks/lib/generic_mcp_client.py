"""Generic MCP stdio client — same JSON-RPC protocol `mcp_client.MCPClient` speaks,
but parameterized over an arbitrary spawn command instead of hardcoding `calm serve`.

Used by B9 to talk to competitor MCP servers (CodeGraph, Semble) with the exact
same request/response accounting as the `calm`-specific client, so tool-call and
token counts are comparable across tools.
"""

from __future__ import annotations

import itertools
import json
import os
import subprocess


class MCPError(RuntimeError):
    pass


class GenericMCPClient:
    def __init__(self, cmd: list[str], cwd: str, env: dict | None = None):
        self._ids = itertools.count(1)
        full_env = {**os.environ, **(env or {})}
        self.proc = subprocess.Popen(
            cmd,
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=subprocess.DEVNULL,
            text=True,
            bufsize=1,
            cwd=cwd,
            env=full_env,
        )
        self._initialize()

    def _send(self, obj: dict) -> None:
        assert self.proc.stdin is not None
        self.proc.stdin.write(json.dumps(obj) + "\n")
        self.proc.stdin.flush()

    def _recv(self) -> dict:
        assert self.proc.stdout is not None
        line = self.proc.stdout.readline()
        if not line:
            raise MCPError("server closed stdout unexpectedly (crashed?)")
        return json.loads(line)

    def _initialize(self) -> None:
        rid = next(self._ids)
        self._send({
            "jsonrpc": "2.0", "id": rid, "method": "initialize",
            "params": {
                "protocolVersion": "2024-11-05",
                "capabilities": {},
                "clientInfo": {"name": "b9-bench", "version": "0.1"},
            },
        })
        resp = self._recv()
        if resp.get("id") != rid or "result" not in resp:
            raise MCPError(f"initialize failed: {resp}")
        self._send({"jsonrpc": "2.0", "method": "notifications/initialized"})

    def call_tool(self, name: str, arguments: dict) -> str:
        rid = next(self._ids)
        self._send({
            "jsonrpc": "2.0", "id": rid, "method": "tools/call",
            "params": {"name": name, "arguments": arguments},
        })
        resp = self._recv()
        if resp.get("id") != rid:
            raise MCPError(f"id mismatch calling {name}: {resp}")
        if "error" in resp:
            raise MCPError(f"{name}({arguments}) -> {resp['error']}")
        content = resp["result"].get("content", [])
        return "".join(c["text"] for c in content if c.get("type") == "text")

    def close(self) -> None:
        assert self.proc.stdin is not None
        self.proc.stdin.close()
        try:
            self.proc.wait(timeout=10)
        except subprocess.TimeoutExpired:
            self.proc.kill()
