from __future__ import annotations

from dataclasses import dataclass, field


@dataclass
class Symbol:
    name: str
    qualified_name: str
    kind: str
    line_start: int
    line_end: int
    signature: str
    docstring: str
    name_tokens: str
    is_entry_point: bool = False


@dataclass
class CallSite:
    callee_name: str
    line: int
    in_symbol: str


@dataclass
class ImportEdge:
    module_name: str
    resolved_path: str | None
    symbols_used: list[str] = field(default_factory=list)


@dataclass
class ParseResult:
    path: str
    language: str
    file_hash: str
    symbols: list[Symbol] = field(default_factory=list)
    call_sites: list[CallSite] = field(default_factory=list)
    imports: list[ImportEdge] = field(default_factory=list)
