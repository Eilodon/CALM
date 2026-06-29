import sqlite3
from collections import defaultdict
import bisect
import fnmatch
from pathlib import Path
import re

def tokenize_identifier(name: str) -> str:
    s = re.sub(r'[_\-]+', ' ', name)
    s = re.sub(r'([a-z0-9])([A-Z])', r'\1 \2', s)
    s = re.sub(r'([A-Z]+)([A-Z][a-z])', r'\1 \2', s)
    return s.lower().strip()

def ensure_gitignore(project_root: Path) -> None:
    if not (project_root / ".git").is_dir():
        return

    gitignore = project_root / ".gitignore"
    entries = [".codeindex/index.db-wal", ".codeindex/index.db-shm"]
    existing = gitignore.read_text(encoding="utf-8") if gitignore.exists() else ""
    existing_patterns = [line.strip() for line in existing.splitlines() if line.strip() and not line.startswith("#")]

    def _is_covered(entry: str, patterns: list[str]) -> bool:
        """Check if entry is already covered by existing patterns.
        Understands Git semantics: directory patterns (trailing /) cover all files within."""
        for p in patterns:
            if fnmatch.fnmatch(entry, p):
                return True
            # Git directory pattern: "dir/" covers all paths starting with "dir/"
            if p.endswith("/") and entry.startswith(p):
                return True
            # Git directory pattern without trailing slash but with slash inside
            if "/" in p and not p.endswith("/") and fnmatch.fnmatch(entry, p + "*"):
                return True
        return False

    to_add = [e for e in entries if not _is_covered(e, existing_patterns)]

    if to_add:
        with gitignore.open("a", encoding="utf-8") as f:
            f.write("\n# Code Intelligence MCP — WAL files (auto-generated)\n")
            f.write("\n".join(to_add) + "\n")
        print(f"[codeindex] Added to .gitignore: {', '.join(to_add)}")

def compute_coreness(conn: sqlite3.Connection) -> dict[str, int]:
    """
    Returns: {qualified_name → coreness_value}
    """
    adj: dict[str, set[str]] = defaultdict(set)
    for (from_sym, to_sym) in conn.execute(
        "SELECT from_symbol, to_symbol FROM call_edges"
    ):
        if from_sym == to_sym:   # skip self-calls (recursive functions); self-loops inflate degree
            continue
        adj[from_sym].add(to_sym)
        adj[to_sym].add(from_sym)

    if not adj:
        return {}

    degree = {node: len(neighbors) for node, neighbors in adj.items()}
    max_deg = max(degree.values())

    buckets: list[set[str]] = [set() for _ in range(max_deg + 1)]
    for node, d in degree.items():
        buckets[d].add(node)

    coreness: dict[str, int] = {}
    remaining_count = len(degree)
    k_ptr = 0

    while remaining_count > 0:
        while k_ptr <= max_deg and not buckets[k_ptr]:
            k_ptr += 1
        if k_ptr > max_deg:
            break

        to_peel = buckets[k_ptr]
        while to_peel:
            v = to_peel.pop()
            coreness[v] = k_ptr
            remaining_count -= 1
            for u in adj[v]:
                if u in coreness:
                    continue
                du = degree[u]
                if du <= k_ptr:
                    continue
                buckets[du].discard(u)
                degree[u] = du - 1
                if degree[u] <= k_ptr:
                    buckets[k_ptr].add(u)
                else:
                    buckets[degree[u]].add(u)

    updates = [(v, sym) for sym, v in coreness.items()]
    conn.execute("UPDATE symbols SET coreness = 0")   # explicit baseline for ALL symbols (including isolated nodes not in adj)
    conn.executemany(
        "UPDATE symbols SET coreness = ? WHERE qualified_name = ?",
        updates
    )

    return coreness

class DummyConfigHubThreshold:
    min_callers: int = 5
    min_callers_bridge: int = 2
    top_pct: float = 5.0
    coreness_pct: float = 75.0

class DummyConfig:
    hub_threshold = DummyConfigHubThreshold()

def update_is_hub_flags(conn: sqlite3.Connection, config) -> None:
    rows = conn.execute(
        "SELECT qualified_name, caller_count, coreness "
        "FROM symbols WHERE caller_count >= 1 OR coreness > 0"
    ).fetchall()

    conn.execute("UPDATE symbols SET is_hub = 0")

    if not rows:
        return

    caller_rows = [r for r in rows if r[1] >= 1]
    sorted_counts = sorted(r[1] for r in caller_rows)
    total = len(sorted_counts) if sorted_counts else 1

    def percentile_rank(caller_count: int) -> float:
        if not sorted_counts:
            return 0.0
        return bisect.bisect_right(sorted_counts, caller_count) / total

    all_coreness = [r[2] for r in rows if r[2] > 0]
    if not all_coreness:
        # No coreness data → bridge-hub path disabled; float('inf') ensures
        # coreness >= p75_coreness is always False, preventing unit confusion
        # between coreness values and caller counts (min_callers).
        p75_coreness: float = float('inf')
    else:
        all_coreness.sort()
        pct = config.hub_threshold.coreness_pct
        idx = max(0, int(len(all_coreness) * pct / 100) - 1)
        p75_coreness = max(all_coreness[idx], config.hub_threshold.min_callers_bridge)

    top_threshold = 1.0 - config.hub_threshold.top_pct / 100.0

    updates = []
    for qname, caller_count, coreness in rows:
        caller_pct = percentile_rank(caller_count)
        is_hub = (
            (caller_count >= config.hub_threshold.min_callers and caller_pct >= top_threshold)
            or
            (caller_count >= config.hub_threshold.min_callers_bridge and coreness >= p75_coreness)
        )
        updates.append((is_hub, qname))

    conn.executemany(
        "UPDATE symbols SET is_hub = ? WHERE qualified_name = ?",
        updates
    )

def init_schema(conn: sqlite3.Connection) -> None:
    """Initialize SQLite FTS5 dual-column search schema."""
    # content-backed FTS5: index stored in fts_exact, raw data lives in symbols.
    # FTS5 does NOT duplicate name/docstring text — fetches via content_rowid=id.
    # Caller must INSERT/DELETE into fts_exact manually when symbols change
    # (or via triggers); FTS5 does not auto-sync.
    conn.execute('''
        CREATE VIRTUAL TABLE IF NOT EXISTS fts_exact USING fts5(
            name,
            docstring,
            content='symbols',
            content_rowid='id',
            tokenize='unicode61'
        )
    ''')
    conn.execute('''
        CREATE VIRTUAL TABLE IF NOT EXISTS fts_tokens USING fts5(
            name_tokens,
            content='symbols',
            content_rowid='id',
            tokenize='unicode61'
        )
    ''')
    conn.commit()
    print("[codeindex] Schema initialized")

def migrate_to_v2_6(conn: sqlite3.Connection) -> None:
    existing_cols = [row[1] for row in conn.execute("PRAGMA table_info(symbols)")]
    if "coreness" not in existing_cols:
        conn.execute("ALTER TABLE symbols ADD COLUMN coreness INTEGER DEFAULT 0")

    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_call_edges_to ON call_edges(to_symbol)"
    )
    conn.commit()
    print("[codeindex] Migration v2.6 complete")
