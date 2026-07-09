#!/usr/bin/env bash
# Wrapper script to redirect calm logs to file, keeping only JSON-RPC on stdout
exec /home/ybao/B.1/CALM/target/release/calm serve --project-root /home/ybao/B.1/CALM 2>/tmp/calm-mcp.log
