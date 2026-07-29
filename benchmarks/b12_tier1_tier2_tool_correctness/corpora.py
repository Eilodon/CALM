"""Corpus registry for B12 -- 6 Tier-0 languages, real external OSS repos.

5 of 6 reuse the already-pinned local clones under `benchmarks/resolution/corpus/`
(shared, READ-ONLY source -- this benchmark never mutates them directly since
`edit_lines`/`edit_symbol` tests write real files; it works on a throwaway `git
clone --local` copy per run instead, torn down afterward).

Rust needs its own fresh clone OUTSIDE the CALM repo tree: cargo auto-discovers
an ancestor `[workspace]` if a crate is nested inside CALM's own cargo workspace,
which silently breaks rust-analyzer (hit and worked around in the 2026-07-28
6-language accuracy benchmark -- see memory
`calm-tier0-6lang-accuracy-benchmark-2026-07-28`).
"""
from __future__ import annotations

import shutil
import subprocess
from dataclasses import dataclass
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parents[2]
RESOLUTION_CORPUS = REPO_ROOT / "benchmarks" / "resolution" / "corpus"
# Deliberately a sibling of REPO_ROOT, not inside it -- see module docstring.
EXTERNAL_CORPUS_ROOT = REPO_ROOT.parent / "calm-bench-corpora"
FD_URL = "https://github.com/sharkdp/fd.git"

WORK_ROOT = Path(__file__).resolve().parent / ".work"


@dataclass(frozen=True)
class Corpus:
    lang: str  # CALM's symbols.language string
    label: str  # human name for reports (upstream project name)
    source: Path  # read-only pinned checkout, never mutated by this benchmark


def _run(cmd: list[str], **kw) -> subprocess.CompletedProcess:
    return subprocess.run(cmd, check=True, capture_output=True, text=True, **kw)


def ensure_fd_clone() -> Path:
    target = EXTERNAL_CORPUS_ROOT / "fd"
    if not target.exists():
        EXTERNAL_CORPUS_ROOT.mkdir(parents=True, exist_ok=True)
        _run(["git", "clone", "--quiet", FD_URL, str(target)])
    return target


def _corpora() -> dict[str, Corpus]:
    return {
        "python": Corpus("python", "flask (pallets/flask)", RESOLUTION_CORPUS / "python"),
        "go": Corpus("go", "gin (gin-gonic/gin)", RESOLUTION_CORPUS / "go"),
        "java": Corpus("java", "spring-petclinic (spring-projects)", RESOLUTION_CORPUS / "java"),
        "javascript": Corpus("javascript", "express (expressjs/express)", RESOLUTION_CORPUS / "js"),
        "typescript": Corpus("typescript", "zod (colinhacks/zod)", RESOLUTION_CORPUS / "typescript"),
        "rust": Corpus("rust", "fd (sharkdp/fd)", ensure_fd_clone()),
    }


CORPORA_LANGS: tuple[str, ...] = ("python", "rust", "go", "javascript", "typescript", "java")

CORPORA: dict[str, Corpus] = {}  # populated lazily by prepare_worktree() to avoid a clone at import time


def get_corpus(lang: str) -> Corpus:
    if not CORPORA:
        CORPORA.update(_corpora())
    return CORPORA[lang]


def prepare_worktree(lang: str) -> Path:
    """Fresh, isolated, mutation-safe local clone of `lang`'s pinned corpus --
    starts with NO `.calm/` dir (a local clone only carries git-tracked
    content), which is exactly the point: the very first tool call in a run
    against this directory really does simulate a brand-new external CALM
    install on a repo it has never seen, not a warm re-index."""
    corpus = get_corpus(lang)
    dest = WORK_ROOT / lang
    if dest.exists():
        shutil.rmtree(dest)
    WORK_ROOT.mkdir(parents=True, exist_ok=True)
    _run(["git", "clone", "--quiet", str(corpus.source), str(dest)])
    return dest


def cleanup_worktree(lang: str) -> None:
    dest = WORK_ROOT / lang
    if dest.exists():
        shutil.rmtree(dest, ignore_errors=True)


def pinned_commit(lang: str) -> str:
    corpus = get_corpus(lang)
    return _run(["git", "-C", str(corpus.source), "rev-parse", "HEAD"]).stdout.strip()
