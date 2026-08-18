"""Independent, tool-agnostic ground truth for B12.

Deliberately built with plain regex + `git grep`, NOT CALM's own tree-sitter
parser -- that independence is what makes it a real external oracle (same
principle as B11's `function_ground_truth_lines`/`grep_oracle_callers`:
computed directly from the file on disk, not trusting any tool under test's
own idea of where a symbol starts/ends or who calls it).

These patterns are deliberately approximate, not parser-grade -- they only
need to sample REAL, independently-verifiable definitions that a competent
human reading the file would also call "a function named X here", not
achieve 100% recall over every language construct.
"""
from __future__ import annotations

import re
import subprocess
from collections import Counter
from dataclasses import dataclass
from pathlib import Path

_NOT_A_NAME = {"if", "for", "while", "switch", "catch", "return", "function"}


@dataclass(frozen=True)
class Definition:
    name: str
    path: str  # repo-relative, forward slashes
    line: int  # 1-indexed
    kind: str  # "function" | "method" | "class" | "type" | ...


# lang -> list of (regex, kind). First capture group is always the symbol name.
PATTERNS: dict[str, list[tuple[str, str]]] = {
    "python": [
        (r"^\s*(?:async\s+)?def\s+(\w+)\s*\(", "function"),
        (r"^\s*class\s+(\w+)\s*[:\(]", "class"),
    ],
    "rust": [
        (r"^\s*(?:pub(?:\([^)]*\))?\s+)?(?:async\s+)?fn\s+(\w+)", "function"),
        (r"^\s*(?:pub(?:\([^)]*\))?\s+)?(?:struct|enum|trait)\s+(\w+)", "type"),
    ],
    "go": [
        (r"^func\s+(?:\([^)]*\)\s*)?(\w+)\s*\(", "function"),
        (r"^type\s+(\w+)\s+(?:struct|interface)\b", "type"),
    ],
    "javascript": [
        (r"^\s*(?:export\s+)?(?:async\s+)?function\s*\*?\s*(\w+)\s*\(", "function"),
        (r"^\s*(?:module\.exports\.|exports\.)(\w+)\s*=\s*(?:async\s+)?function", "assigned_function"),
        (r"\b\w+\.prototype\.(\w+)\s*=\s*function", "prototype_method"),
        (r"^\s*class\s+(\w+)", "class"),
    ],
    "typescript": [
        (r"^\s*(?:export\s+)?(?:default\s+)?(?:async\s+)?function\s*\*?\s*(\w+)\s*\(", "function"),
        (r"^\s*(?:export\s+)?(?:abstract\s+)?class\s+(\w+)", "class"),
        (r"^\s*(?:export\s+)?interface\s+(\w+)", "interface"),
        (r"^\s*(?:export\s+)?type\s+(\w+)\s*=", "type"),
    ],
    "java": [
        (r"^\s*(?:public|private|protected)?\s*(?:static\s+)?(?:final\s+)?(?:abstract\s+)?class\s+(\w+)", "class"),
        (r"^\s*(?:public|private|protected)?\s*(?:static\s+)?(?:final\s+)?interface\s+(\w+)", "interface"),
        (
            r"^\s*(?:public|private|protected)\s+(?:static\s+)?(?:final\s+)?"
            r"[\w<>\[\],\s]+?\s+(\w+)\s*\([^;{]*\)\s*(?:throws\s+[\w,\s]+)?\{",
            "method",
        ),
    ],
}

EXTENSIONS: dict[str, tuple[str, ...]] = {
    "python": (".py",),
    "rust": (".rs",),
    "go": (".go",),
    "javascript": (".js",),
    "typescript": (".ts",),
    "java": (".java",),
}

_SKIP_DIR_PARTS = {".git", "node_modules", "target", "vendor", "__pycache__", ".venv", "build", "dist"}


def iter_source_files(root: Path, lang: str):
    exts = EXTENSIONS[lang]
    for p in root.rglob("*"):
        if not p.is_file() or p.suffix not in exts:
            continue
        if _SKIP_DIR_PARTS & set(p.relative_to(root).parts):
            continue
        yield p


def extract_definitions(root: Path, lang: str) -> list[Definition]:
    compiled = [(re.compile(pat), kind) for pat, kind in PATTERNS[lang]]
    out: list[Definition] = []
    for f in iter_source_files(root, lang):
        try:
            text = f.read_text(errors="ignore")
        except OSError:
            continue
        rel = f.relative_to(root).as_posix()
        for i, line in enumerate(text.splitlines(), start=1):
            for rx, kind in compiled:
                m = rx.search(line)
                if not m:
                    continue
                name = m.group(1)
                if name and name not in _NOT_A_NAME:
                    out.append(Definition(name=name, path=rel, line=i, kind=kind))
    return out


def _looks_like_a_definition(line_text: str, lang: str) -> bool:
    """True if `line_text` itself matches ANY of this language's own
    def-patterns (regardless of which name) -- used to exclude redefinition/
    override/shadow lines from call-site ground truth. A first dry run on
    flask found this exact failure: `my_reverse` "call sites" were all OTHER
    `def my_reverse(s):` lines -- the same local-helper name redefined inside
    ~10 different test functions, not one real call anywhere -- and
    `jinja_loader` "call sites" were actually substring hits inside the
    unrelated, longer identifier `create_global_jinja_loader(`. Both are
    ground-truth bugs this filter (plus the word-boundary fix below) closes."""
    return any(re.search(pat, line_text) for pat, _kind in PATTERNS[lang])


# 2026-08-02 fix: single-line comment marker per language, used by
# `git_grep_call_sites` to exclude a comment that merely MENTIONS a function
# name (`// see also foo(...)`) from counting as a real call site. Same
# "good enough to catch gross oracle noise, not parser-grade" posture as
# `_looks_like_a_definition` above -- doesn't track block comments
# (`/* ... */`) or docstrings, only the common single-line-comment case that
# was verified as a real, already-published false positive (see
# `git_grep_call_sites`'s own docstring).
COMMENT_PREFIXES: dict[str, str] = {
    "python": "#",
    "rust": "//",
    "go": "//",
    "javascript": "//",
    "typescript": "//",
    "java": "//",
}


def _is_comment_line(line_text: str, lang: str) -> bool:
    """True if `line_text` (as returned by `git grep`, leading whitespace
    intact) is a single-line comment in `lang` -- i.e. its content, after
    stripping leading whitespace, starts with that language's line-comment
    marker. Covers Rust's `//`, `///`, and `//!` alike (all start with `//`)."""
    prefix = COMMENT_PREFIXES.get(lang)
    return bool(prefix) and line_text.lstrip().startswith(prefix)


# 2026-08-18 fix: quote characters used by each language's string literals,
# used by `_is_inside_string_literal` to exclude a `NAME(` match that only
# occurs as DATA inside a quoted string (e.g. a pytest parametrize tuple
# holding a Flask CLI factory-string) from counting as a real call site.
_STRING_QUOTE_CHARS: dict[str, tuple[str, ...]] = {
    "python": ("'", '"'),
    "javascript": ("'", '"', "`"),
    "typescript": ("'", '"', "`"),
    "go": ('"', "`"),
    "java": ('"',),
    "rust": ('"',),
}


def _is_inside_string_literal(line_text: str, match_start: int, lang: str) -> bool:
    """Best-effort, same 'good enough to catch gross oracle noise, not
    parser-grade' posture as the comment/definition filters above: a
    call-shaped `NAME(` occurring inside a quoted string on the same line
    (odd count of a quote char before match_start) is almost never a real
    call site -- e.g. a pytest parametrize tuple holding a Flask CLI
    factory-string like 'create_app2("foo", "bar")' as DATA, not code.
    Verified real on flask's `create_app2`: a fresh B12 run flagged it as a
    zero-recall bug (0 real callers found by `callers`) when in fact
    `git_grep_call_sites` was counting 3 string-literal occurrences inside
    tests/test_cli.py's parametrize table -- there is no real static call
    site anywhere in the corpus, `callers`' 0 was correct. Doesn't handle
    escaped quotes or multi-line strings -- narrow on purpose, only strong
    enough to catch this exact shape."""
    quote_chars = _STRING_QUOTE_CHARS.get(lang, ("'", '"'))
    prefix = line_text[:match_start]
    return any(prefix.count(q) % 2 == 1 for q in quote_chars)


def git_grep_call_sites(root: Path, name: str, def_path: str, def_line: int, lang: str) -> list[tuple[str, int]]:
    """Real CALL-shaped sites for `name` anywhere in the corpus, excluding the
    definition line itself and any OTHER line that looks like a (re)definition
    of the same name -- generalizes B11's `grep_oracle_callers` past a single
    `fn NAME` anchor.

    Word-bounded `NAME(` match (via `git grep -E '(^|[^A-Za-z0-9_])NAME\\('`,
    not a bare fixed-string substring) plus the `_looks_like_a_definition`
    filter above. Two real ground-truth bugs a first flask dry run surfaced,
    both fixed here:
      1. Fixed-string `name(` (no boundary) matched `jinja_loader(` as a
         SUBSTRING of the unrelated, longer `create_global_jinja_loader(`.
      2. A name redefined in multiple scopes (`my_reverse` as ~10 different
         local test-helper closures) had every OTHER definition line counted
         as a "call site", when textually there was never one real call
         anywhere (`my_reverse` was always invoked indirectly through a dict:
         `filters["my_reverse"](...)`).

    2026-08-02 fix (b13 CALM-vs-CodeGraph investigation): two more ground-truth
    bugs, found the same way as the two above -- verified real on already-
    published data, not hypothetical. `git grep` with no pathspec scanned
    EVERY tracked file in the repo, not just this corpus's own source
    extension, so a markdown/`.rst` doc that merely MENTIONS `name(` in prose
    (or inside an illustrative code snippet in a design doc) counted as a real
    caller; and no comment-line filter existed, so a `// see also name(...)`
    style comment counted too. b13's published run had 4 such false "oracle
    files" baked into its denominator (`docs/patterns/celery.rst`,
    `docs/logging.rst`, a markdown design doc with 3 non-code mentions, and a
    Rust `// ... common::resolve_preset(...)` re-export comment) -- neither
    CALM nor CodeGraph ever found any of them (correctly -- none are real call
    sites), so both tools were being docked for "missing" something that was
    never really there. Fixed by restricting the pathspec to this language's
    own source extension(s) and skipping comment-only lines. Still an
    approximation, not a call-graph-precision oracle -- only strong enough to
    flag gross zero-recall failures, the same profile that caught the real B1
    JS/TS bug and the two documented above.

    2026-08-18 fix: a third ground-truth bug of the same family, found via a
    fresh B12 run's own `zero_recall_bug` flag on flask's `create_app2` --
    see `_is_inside_string_literal`'s docstring. Filters out any match that
    falls inside a same-line quoted string, not just comments."""
    pattern = rf"(^|[^A-Za-z0-9_]){re.escape(name)}\("
    compiled = re.compile(pattern)
    pathspecs = [f"*{ext}" for ext in EXTENSIONS[lang]]
    proc = subprocess.run(
        ["git", "grep", "-n", "-E", "--", pattern, *pathspecs],
        cwd=root, capture_output=True, text=True, check=False,
    )
    sites: list[tuple[str, int]] = []
    for line in proc.stdout.splitlines():
        parts = line.split(":", 2)
        if len(parts) < 3:
            continue
        path, lineno_s, text = parts[0], parts[1], parts[2]
        try:
            lineno = int(lineno_s)
        except ValueError:
            continue
        if path == def_path and lineno == def_line:
            continue
        if _looks_like_a_definition(text, lang):
            continue
        if _is_comment_line(text, lang):
            continue
        m = compiled.search(text)
        if m is not None:
            # `m.group(1)` is the zero-width `^` alternative or the
            # captured non-word boundary char -- the real name starts
            # right after it, and that boundary char itself must be
            # INCLUDED in the quote-parity prefix (it's often the
            # opening quote itself, e.g. `'create_app2(...`).
            name_pos = m.start() + len(m.group(1))
            if _is_inside_string_literal(text, name_pos, lang):
                continue
        sites.append((path, lineno))
    return sites


def total_occurrences(root: Path, name: str) -> int:
    """Corpus-wide bare-word occurrence count for `name` -- used to filter
    OUT overly-generic identifiers (common English words, common stdlib
    method names) before sampling, so search/callers/file_overview checks
    only run against names a reasonable oracle can actually pin down. The
    flask dry run picked names like `index`/`add`/`default` that have
    hundreds of unrelated matches corpus-wide (docstrings, `list.index()`,
    other symbols with the same name) -- not fair samples for any tool to be
    judged against."""
    proc = subprocess.run(
        ["git", "grep", "-c", "-w", "--", name],
        cwd=root, capture_output=True, text=True, check=False,
    )
    total = 0
    for line in proc.stdout.splitlines():
        try:
            total += int(line.rsplit(":", 1)[-1])
        except ValueError:
            continue
    return total


_DUNDER_RE = re.compile(r"^__.+__$")


def unique_definitions(defs: list[Definition]) -> list[Definition]:
    """Definitions whose name occurs exactly once in `defs` AND isn't a
    dunder/magic method (`__init__`, `__str__`, ...) -- both patterns are
    near-universally redefined across many unrelated scopes (every class has
    its own `__init__`; test suites commonly repeat a local `create_app`/
    `Test`-style helper name per test module), so "does tool X find/count
    calls to THIS exact one" isn't a well-posed question for them. A flask
    dry run caught both failure modes: a `class Test` search sample that
    failed because CALM legitimately returned A `Test` class, just not the
    one specific instance sampled; then `__init__`/`create_app` "zero-recall
    bugs" in the callers check that were really just 14/27 call sites to
    OTHER same-named definitions elsewhere, not a missed call to the one
    definition actually sampled."""
    name_counts = Counter(d.name for d in defs)
    return [d for d in defs if name_counts[d.name] == 1 and not _DUNDER_RE.match(d.name)]


def sample_distinctive(
    rng, defs: list[Definition], root: Path, k: int,
    min_len: int = 4, max_total_occurrences: int = 40, pool_multiplier: int = 8,
) -> list[Definition]:
    """Randomly sample up to `k` definitions whose name is (a) long enough,
    (b) rare enough corpus-wide to be a fair/findable target (see
    `total_occurrences`), and (c) unique in the corpus (see
    `unique_definitions`)."""
    candidates = [d for d in unique_definitions(defs) if len(d.name) >= min_len]
    if not candidates:
        return []
    pool_size = min(len(candidates), max(k * pool_multiplier, k))
    pool = rng.sample(candidates, k=pool_size)
    out: list[Definition] = []
    for d in pool:
        if len(out) >= k:
            break
        if total_occurrences(root, d.name) <= max_total_occurrences:
            out.append(d)
    return out
