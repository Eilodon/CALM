#!/usr/bin/env python3
"""Direct probe: does scip_overlay finish before or after indexing_phase=='ready'?
And does callees(reindex_changed) change once it does? Single process, no shell
FD juggling across separate Bash calls.
"""
import json
import subprocess
import sys
import time

CORPUS = sys.argv[1]
REPO_ROOT = "/home/ybao/B.1/CALM"

proc = subprocess.Popen(
    ["target/release/calm", "serve", "--project-root", CORPUS],
    stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL,
    text=True, bufsize=1, cwd=REPO_ROOT,
)

ids = iter(range(1, 100000))


def call(method, params=None):
    rid = next(ids)
    obj = {"jsonrpc": "2.0", "id": rid, "method": method}
    if params is not None:
        obj["params"] = params
    proc.stdin.write(json.dumps(obj) + "\n")
    proc.stdin.flush()
    line = proc.stdout.readline()
    return json.loads(line)


def call_tool(name, args):
    resp = call("tools/call", {"name": name, "arguments": args})
    content = resp["result"].get("content", [])
    return "".join(c["text"] for c in content if c.get("type") == "text")


call("initialize", {"protocolVersion": "2024-11-05", "capabilities": {}, "clientInfo": {"name": "probe", "version": "0.1"}})
proc.stdin.write(json.dumps({"jsonrpc": "2.0", "method": "notifications/initialized"}) + "\n")
proc.stdin.flush()

t_start = time.time()
last_scip = None
phase_ready_at = None
scip_ready_at = None
for _ in range(90):
    text = call_tool("indexing_status", {})
    status = json.loads(text)
    phase = status.get("indexing_phase")
    scip = status.get("scip_overlay")
    elapsed = round(time.time() - t_start, 1)
    if scip != last_scip or _ % 5 == 0:
        print(f"t={elapsed}s phase={phase} scip_overlay={scip}", file=sys.stderr)
        last_scip = scip
    if phase == "ready" and phase_ready_at is None:
        phase_ready_at = elapsed
    if scip and scip.get("up_to_date") and scip_ready_at is None:
        scip_ready_at = elapsed
    if phase == "ready" and scip and scip.get("up_to_date"):
        break
    time.sleep(1)

print(f"\nindexing_phase reached 'ready' at t={phase_ready_at}s", file=sys.stderr)
print(f"scip_overlay reached up_to_date at t={scip_ready_at}s", file=sys.stderr)


def show_callees(label):
    text = call_tool("callees", {"symbol": "reindex_changed"})
    d = json.loads(text)
    names = sorted(x["symbol"].rsplit("::", 1)[-1] for x in d.get("direct", []))
    print(f"\n=== callees(reindex_changed) — {label} — {len(names)} found ===")
    for x in d.get("direct", []):
        print(" ", x["symbol"], "-", x.get("edge_confidence"))
    return set(names)


first = show_callees(f"t={round(time.time() - t_start, 1)}s, right after ready+overlay-flag-true")
print("\nwaiting 30s more, re-querying to check empirically if the answer changes over time regardless of the status flag ...", file=sys.stderr)
time.sleep(30)
second = show_callees(f"t={round(time.time() - t_start, 1)}s, +30s later")

print(f"\ndiff (second - first): {second - first}")
print(f"diff (first - second): {first - second}")

callers_text = call_tool("callers", {"symbol": "collect_source_files"})
print("\n=== callers(collect_source_files) ===")
d2 = json.loads(callers_text)
for x in d2.get("direct", d2.get("callers", [])):
    print(" ", x.get("symbol"), "-", x.get("edge_confidence"))

proc.kill()
