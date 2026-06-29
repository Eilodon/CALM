from __future__ import annotations

import sqlite3
import threading
from collections import OrderedDict, deque
from dataclasses import dataclass, field
from pathlib import Path

from .config import Config
from .coverage_reader import CoverageData


@dataclass
class EmbedderState:
    status: str = "disabled"
    error: dict | None = None


@dataclass
class SessionState:
    started_at: str
    tool_calls: int = 0
    explored_symbols: OrderedDict = field(default_factory=OrderedDict)
    explored_files: set[str] = field(default_factory=set)
    already_fetched: deque = field(default_factory=lambda: deque(maxlen=200))
    unique_files_explored: int = 0


@dataclass
class IndexerState:
    phase: str = "scanning"
    files_indexed: int = 0
    files_total: int = 0
    symbols_indexed: int | None = None
    edges_indexed: int | None = None
    last_updated: str = ""
    embedder: EmbedderState | None = None


@dataclass
class ServerContext:
    project_root: Path
    db_path: Path
    config: Config
    write_conn: sqlite3.Connection
    write_lock: threading.Lock
    coverage_data: CoverageData
    codeowners_patterns: list
    indexer_state: IndexerState = field(default_factory=IndexerState)
    session: SessionState | None = None

    def make_read_conn(self) -> sqlite3.Connection:
        conn = sqlite3.connect(str(self.db_path), check_same_thread=False)
        conn.execute("PRAGMA journal_mode=WAL")
        conn.execute("PRAGMA query_only=ON")
        conn.row_factory = sqlite3.Row
        return conn
