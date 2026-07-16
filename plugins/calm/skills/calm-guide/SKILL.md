---
name: calm-guide
description: >-
  Use before searching, reading, or editing files in this repo (or any repo
  with a "calm" MCP server), or whenever unsure which CALM tool fits — a
  self-triggerable orientation to the CALM Stage 1-8 tool workflow
  (repo_overview → locate → source → callers → edit_context →
  edit_symbol/edit_lines → diff_impact → session_context). Also invoke
  manually as /calm-guide for a mid-session refresher, or whenever this
  project's AGENTS.md SessionStart auto-injection didn't happen this turn
  (some MCP clients — and non-Claude-Code sessions entirely — never get it).
metadata:
  version: "1.0"
  origin: >-
    Plugin-distributed copy of CALM's own internal .claude/skills/calm-guide,
    added when the harness (hooks + this skill) was migrated from
    dogfooding-only to the distributed plugin. Companion to the
    calm_workflow MCP Prompt in crates/calm-server/src/tools.rs — same
    underlying content (calm_core::workflow::CALM_WORKFLOW_GUIDE), three
    trigger surfaces now: this Skill, the MCP Prompt, and a scaffolded
    AGENTS.md (crates/calm-core/assets/hooks/calm-hooks.sh's SessionStart
    bootstrap runs `calm init --agents-md` on first use of this plugin in a
    project, so AGENTS.md usually exists by the time this skill fires — but
    never assume it, see below).
---

# CALM MCP workflow — quick trigger

**Read `AGENTS.md` at the project root in full now, if it exists.** When
present it is the single source of truth for this workflow — this skill
exists to make sure you actually reach for it at the right moment, not to
hold a second copy of its content that would silently drift out of sync with
it over time.

**If `AGENTS.md` doesn't exist at the project root** — a brand-new install of
this plugin, before its own SessionStart bootstrap has had a chance to
scaffold one, or a non-Claude-Code MCP client where no scaffold ever ran —
this is still very likely a CALM-instrumented repo if a "calm" MCP server is
connected. Call the **`calm_workflow` MCP Prompt** (no arguments) instead: it
renders the exact same workflow-guide text straight from the connected
server's own binary, no project-root file needed. Only conclude "CALM isn't
active here" if that prompt call itself is unavailable or fails.

## If you only have room for the short version right now

1. **Orient** — `repo_overview()` always first, then `hotspots()`/
   `fitness_report()` as needed.
2. **Locate** — `locate(query)` (search + file_overview + symbol_info in one
   call) or `search(query, kind=...)`.
3. **Inspect** — `source(symbol)` for a symbol-precise read, or
   `understand(symbol)` for locate+source+callers together.
4. **Trace** — `callers(symbol)`, `callees(symbol)`, `path(from, to)`,
   `dependencies(path)`.
5. **Pre-Edit** — `edit_context(symbol)`: mandatory before touching any real
   code symbol, no exceptions. (A Markdown/text heading is the one
   exception — it never carries a blast radius to check.)
6. **Edit** — `edit_symbol(...)`/`edit_lines(...)`: CALM's own write path,
   hash-verified, risk-gated, reindexes immediately. Fall back to native
   `Edit`/`Write` only for a brand-new file or a path CALM doesn't index
   (dotdirs, `target/`, `node_modules/`, `dist/`, `build/`, etc.).
7. **Verify** — `diff_impact(staged=true)`: mandatory before any commit or
   push, no exceptions.
8. **Recover** — `session_context()` after 10+ calls without progress;
   `remember(topic, content)`/`recall(topic)` for durable cross-session
   notes.

Steps 5 and 7 are the only hard-enforced ones under Claude Code, and only
once `calm init --hooks=enforce` has scaffolded `.claude/hooks/calm-hooks.sh`
into this project (this plugin's SessionStart bootstrap does that
automatically on first use — check `calm doctor` if unsure whether it's
active). The rest is strongly recommended, not blocking. Every signal, edge
case, and the preset/toolset reference lives in `AGENTS.md` (or the
`calm_workflow` prompt, if no AGENTS.md exists) — go read it in full if you
haven't this session.
