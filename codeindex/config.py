from __future__ import annotations

import json
from pathlib import Path

from pydantic import BaseModel, Field


class HubThresholdConfig(BaseModel):
    top_pct: float = 5.0
    min_callers: int = 5
    min_callers_bridge: int = 2
    coreness_pct: float = 75.0


class CallGraphConfig(BaseModel):
    resolver: str = "conservative"
    confidence_tracking: bool = True


class SemanticSearchConfig(BaseModel):
    enabled: bool = False
    model: str = "BAAI/bge-base-en-v1.5"
    dimensions: int = 768
    index_on_startup: bool = False


class SearchConfig(BaseModel):
    text_chunk_context_lines: int = 10
    text_max_chunk_lines: int = 50
    rrf_k: int = 20


class PathConfig(BaseModel):
    default_max_hops: int = 8
    max_allowed_hops: int = 20
    timeout_ms: int = 5000


class DepthConfig(BaseModel):
    max_depth_cap: int = 4
    transitive_timeout_ms: int = 3000


class HotspotsConfig(BaseModel):
    default_top_n: int = 10
    default_since: str = "6 months ago"
    default_min_churn: int = 2
    risk_critical_threshold: float = 0.75
    risk_high_threshold: float = 0.50
    risk_medium_threshold: float = 0.25


class Config(BaseModel):
    preset: str = "full"
    languages: list[str] = [
        "python", "typescript", "javascript", "java", "rust", "go",
    ]
    ignore: list[str] = [
        "node_modules", ".git", "__pycache__", "*.min.js", "dist", "build", ".venv",
    ]
    entry_points: list[str] = []
    hub_threshold: HubThresholdConfig = Field(default_factory=HubThresholdConfig)
    call_graph: CallGraphConfig = Field(default_factory=CallGraphConfig)
    semantic_search: SemanticSearchConfig = Field(default_factory=SemanticSearchConfig)
    search: SearchConfig = Field(default_factory=SearchConfig)
    path: PathConfig = Field(default_factory=PathConfig)
    callers: DepthConfig = Field(default_factory=DepthConfig)
    callees: DepthConfig = Field(default_factory=DepthConfig)
    hotspots: HotspotsConfig = Field(default_factory=HotspotsConfig)


def load_config(project_root: Path) -> Config:
    """Load config.json or .codeindex/config.json. Falls back to defaults."""
    for candidate in [
        project_root / "config.json",
        project_root / ".codeindex" / "config.json",
    ]:
        if candidate.exists():
            return Config.model_validate(json.loads(candidate.read_text()))
    return Config()
