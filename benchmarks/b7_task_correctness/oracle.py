"""B7 oracle -- reuses B12's ground_truth for independent callsite ground
truth and adds a build/test gate. See docs/superskills/specs/2026-07-30-
calm-dfb-levers-design.md (§1.2/§1.4) and its audit-design Risk Assessment
(mitigations #1/#2/L6 folded in here, not just described).
"""

from __future__ import annotations

import subprocess
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "b12_tier1_tier2_tool_correctness"))
import ground_truth as gt  # noqa: E402


def real_call_sites(
    root: Path, name: str, def_path: str, def_line: int, lang: str, src_exts: tuple[str, ...],
) -> list[tuple[str, int]]:
    """B12's git_grep_call_sites has no file-extension filter -- a real gap
    found empirically while picking B7's first candidates: flask's `init_db`
    reported 9 "call sites" via plain `git grep`, 5 of which were `.rst` doc
    prose mentioning the function by name, not real Python call expressions
    (verified live: `git grep -n -E '(^|[^A-Za-z0-9_])init_db\\(' --` returns
    docs/appcontext.rst, docs/patterns/sqlalchemy.rst, etc. alongside the 3
    real .py files). B7 needs a precise callsite-recall denominator, so this
    wrapper restricts B12's oracle to the corpus's own source extensions --
    B12 itself doesn't need this fix since it only ever flags gross
    zero-recall failures, not a precise recall fraction.
    """
    sites = gt.git_grep_call_sites(root, name, def_path, def_line, lang)
    return [(p, ln) for p, ln in sites if p.endswith(src_exts)]


class BuildTestResult:
    def __init__(self, build_ok: bool, test_ok: bool, output: str):
        self.build_ok = build_ok
        self.test_ok = test_ok
        self.output = output

    @property
    def passed(self) -> bool:
        return self.build_ok and self.test_ok


def run_cmd(cmd: list[str], cwd: Path, timeout: float = 300.0) -> tuple[bool, str]:
    try:
        proc = subprocess.run(cmd, cwd=cwd, capture_output=True, text=True, timeout=timeout)
        return proc.returncode == 0, (proc.stdout + proc.stderr)[-4000:]
    except subprocess.TimeoutExpired as e:
        return False, f"TIMEOUT after {timeout}s: {e}"
    except FileNotFoundError as e:
        return False, f"COMMAND NOT FOUND: {cmd} ({e})"


def build_test_gate(corpus: Path, build_cmd: list[str] | None, test_cmd: list[str]) -> BuildTestResult:
    """Runs the corpus's REAL toolchain -- never a hardcoded pass/fail
    ("not a stub" acceptance criterion from the design's brainstorming pass).
    `build_cmd=None` when the language has no build step distinct from its
    test runner (e.g. Rust: `cargo test` both builds and tests)."""
    output = ""
    if build_cmd:
        build_ok, out = run_cmd(build_cmd, corpus)
        output += out
        if not build_ok:
            return BuildTestResult(False, False, output)
    else:
        build_ok = True
    test_ok, out = run_cmd(test_cmd, corpus)
    output += out
    return BuildTestResult(build_ok, test_ok, output)


def baseline_green(corpus: Path, build_cmd: list[str] | None, test_cmd: list[str]) -> BuildTestResult:
    """Run once BEFORE any refactor -- audit-design L6 mitigation: without
    this, a corpus's own pre-existing flaky/failing test would get
    misattributed to "the arm broke it." A task whose baseline isn't green
    must be rejected/reported, never silently scored as an arm failure."""
    return build_test_gate(corpus, build_cmd, test_cmd)
