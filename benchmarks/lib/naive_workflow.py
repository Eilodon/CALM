"""Shared naive-workflow simulation, reused by B4 (token count) and B6 (tool-call count).

Given a task's `naive` spec from tasks.yaml, reconstructs the text an agent
without call-graph tools would have to read, plus how many discrete tool
invocations (grep/cat) it took to get there.
"""

from __future__ import annotations

import subprocess
from pathlib import Path


def naive_text_and_calls(repo_root: Path, spec: dict) -> tuple[str, int]:
    kind = spec["type"]

    if kind == "cat":
        text = (repo_root / spec["path"]).read_text()
        return text, 1  # 1 file read

    if kind == "grep":
        text = _grep(repo_root, spec["pattern"], spec["globs"])
        return text, 1  # 1 grep call, no follow-up reads

    if kind == "grep_then_cat_matches":
        matched_files = _grep_files(repo_root, spec["pattern"], spec["globs"])
        text = "\n".join((repo_root / f).read_text() for f in matched_files)
        calls = 1 + len(matched_files)  # 1 grep -l + one read per matched file
        return text, calls

    raise ValueError(f"unknown naive.type: {kind}")


def naive_text(repo_root: Path, spec: dict) -> str:
    text, _ = naive_text_and_calls(repo_root, spec)
    return text


def naive_grep_ranked_files(repo_root: Path, pattern: str, globs: list[str]) -> list[str]:
    """Files matching `pattern` (`grep -l`), in the order grep -l returns them
    — i.e. no relevance ranking at all, just file-scan order. Used by B3 as
    the "naive" baseline ranking to compare `ci search`'s real ranking against.
    """
    return _grep_files(repo_root, pattern, globs)


def _grep(repo_root: Path, pattern: str, globs: list[str]) -> str:
    files = _glob_files(repo_root, globs)
    result = subprocess.run(
        ["grep", "-n", pattern, *files],
        cwd=repo_root, capture_output=True, text=True,
    )
    return result.stdout


def _grep_files(repo_root: Path, pattern: str, globs: list[str]) -> list[str]:
    files = _glob_files(repo_root, globs)
    result = subprocess.run(
        ["grep", "-l", pattern, *files],
        cwd=repo_root, capture_output=True, text=True,
    )
    return [line for line in result.stdout.splitlines() if line]


def _glob_files(repo_root: Path, globs: list[str]) -> list[str]:
    files: list[str] = []
    for pattern in globs:
        files.extend(str(p.relative_to(repo_root)) for p in repo_root.glob(pattern))
    return files
