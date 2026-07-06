"""Minimal MCP stdio client for the `calm` server.

Spawns `calm serve` once, keeps the process alive for the whole benchmark run,
and exposes `call_tool()` returning the raw text content an agent would
actually receive (this is exactly what gets tokenized for the ratio).

Reusable across B1-B5 benchmarks, not just B4.
"""

from __future__ import annotations

import itertools
import json
import subprocess
import time
from pathlib import Path


class MCPError(RuntimeError):
    pass


class MCPClient:
    def __init__(self, project_root: str, repo_root: str | None = None):
        self.project_root = project_root
        self._ids = itertools.count(1)
        self.proc = subprocess.Popen(
            [
                "cargo", "run", "--quiet", "--release", "-p", "calm-cli",
                "--", "serve", "--project-root", project_root,
            ],
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=subprocess.DEVNULL,
            text=True,
            bufsize=1,
            cwd=repo_root,
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
            raise MCPError("ci server closed stdout unexpectedly (crashed?)")
        return json.loads(line)

    def _initialize(self) -> None:
        rid = next(self._ids)
        self._send({
            "jsonrpc": "2.0", "id": rid, "method": "initialize",
            "params": {
                "protocolVersion": "2024-11-05",
                "capabilities": {},
                "clientInfo": {"name": "b4-bench", "version": "0.1"},
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

    def wait_until_indexed(self, timeout: float = 60.0, poll_interval: float = 1.0) -> None:
        deadline = time.time() + timeout
        last_phase = None
        while time.time() < deadline:
            raw = self.call_tool("indexing_status", {})
            status = json.loads(raw)
            last_phase = status.get("indexing_phase")
            if last_phase == "ready":
                return
            time.sleep(poll_interval)
        raise MCPError(f"index not ready after {timeout}s (last phase={last_phase})")

    def close(self) -> None:
        assert self.proc.stdin is not None
        self.proc.stdin.close()
        try:
            self.proc.wait(timeout=10)
        except subprocess.TimeoutExpired:
            self.proc.kill()


def repo_root_from_here() -> Path:
    # benchmarks/b4_token_efficiency/mcp_client.py -> repo root is 2 levels up
    return Path(__file__).resolve().parents[2]
