# Comparing `calm` with other code-intelligence MCP servers

Context (as of mid-2026): "code intelligence for AI agents" has become its own product
category, no longer a niche — several tools have passed tens of thousands of GitHub stars.
The table below compares `calm` against the main players in this category, based on each
project's public documentation at the time of writing. The numbers (stars, market share...)
change fast — read this table to understand the **shape of the differences**, not as a fixed
leaderboard.

| Tool | Languages | Call-graph / blast-radius | Direct file edits | Pre-edit safety | Cross-session memory | Integrated navigation/workflow |
|---|---|---|---|---|---|---|
| **`calm`** | 6 Tier-0 (full call graph) + 18 Tier-0.5 (real grammar + call graph when the feature flag is on; 7/18 on by default) + a standalone SQL indexer | Yes — `callers`/`callees`/`edit_context`/`diff_impact` | Yes — `edit_lines`/`edit_symbol` | Yes — hash-verified conflict guard, hard-refuses hub/high-caller symbols without `confirm:true`, `diff_impact` mandatory before commit | Yes — `remember`/`recall`, survives restarts | Yes — `suggested_next` on every response, 8-stage workflow (`AGENTS.md`) |
| **Serena** | 40+ (via LSP) | Limited — mostly symbol references, no risk scoring | Yes, symbol-level | **No** — directly verified (see B11): `replace_symbol_body` has no `confirm`/`force` field in its schema at all; it genuinely overwrote a hub symbol with no confirmation required | **Yes** — directly verified (see B11): `write_memory`/`read_memory`, survives process restarts. *(This table used to say "No" here — that was wrong, corrected after real testing found Serena has 6 memory tools: write/read/list/delete/rename/edit_memory.)* | No |
| **CodeGraph** | 23, full graph | Yes, fully | **No** — query-only, read-only | N/A (doesn't edit files) | File watcher (not interpretive memory) | No |
| **grepai** | Multi-language via tree-sitter | Yes — `trace_callers`/`trace_callees`/`trace_graph`, plus semantic search (Ollama, 100% local) | **No** — query-only, read-only | N/A (doesn't edit files) | No | No |
| **GitNexus** | Mostly TypeScript | Yes, via 16 tools | Not clearly documented | Not clearly documented | Skills + hooks (not a memory tool) | Yes (its own skills/hooks, tied to Claude Code) |
| **Sourcegraph/Cody MCP** | Multi-language, multi-repo | Yes, cross-repo (Deep Search) | Not clearly documented | No | No | No |
| **Cursor (built-in indexing)** | Multi-language | No — embedding-based, not a graph | Yes (via Cursor's own editor, not MCP) | No | No | No |
| **Aider (repo-map)** | Multi-language | No — PageRank over a tag-map, not a typed-edge graph | Yes (via Aider itself, not an interactive MCP tool) | No | No | Yes, but closed inside Aider's own loop |

The table above is based on public documentation (qualitative) — **except the Serena/grepai/CodeGraph
rows in the "Direct edits"/"Safety"/"Memory" columns, which were verified with real tool calls, not
just by reading docs** (see B11). For numbers measured via real tool calls — same self-repo corpus,
`calm` vs CodeGraph vs Semble vs grepai vs Serena — see
[`benchmarks/b11_extended_competitor_ab/`](../benchmarks/b11_extended_competitor_ab/):
CodeGraph missed the same cross-crate caller on both `find_callers` (1/2) and `pre_edit_blast_radius`
(1/5); Serena genuinely overwrote a hub symbol with zero confirmation (`calm` refuses, with an
`is_hub=true` explanation); raw token-ratio numbers shouldn't be read as a leaderboard, since each
tool answers at a different level of detail (details in B11's README).

## Why this table matters more than it looks

An independent survey of this tool category (2026) concluded two things outright:

> "No tools [in this category] implement pre-edit safety gates or impact warnings before structural changes."
>
> "Memory integration [is] notably absent across all tools — a gap that remains."

The two pillars `calm` invests in most — **hard risk-gating before edits** and
**cross-session `remember`/`recall`** — land exactly in those two gaps, according to that survey.
On the second point specifically, real testing (B11) shows the survey doesn't hold for Serena in
particular — Serena has `write_memory`/`read_memory` that survives process restarts, directly
verified. On the first point (risk-gating), the survey holds: Serena's `replace_symbol_body` has no
confirmation field at all, verified in the same pass. Most tools in this category still stop at
"help the agent *find* code faster"; `calm` goes a step further on both axes, but "no tool in this
category has memory" is no longer accurate once at least one tool in the category has actually been
tested.

## When to choose `calm`

- Your agent **edits code directly**, not just looks things up/answers questions — and you want a
  real safety net (hash-verified, risk-gated) instead of just "hoping the agent is careful."
- Your main codebase is in one of the 6 Tier-0 languages (Python/TypeScript/JavaScript/Java/Rust/Go)
  — where `calm` has a full call graph, not shallow symbols.
- You want the agent to **remember on its own** architectural decisions/gotchas across sessions,
  instead of re-explaining everything from scratch every time.
- You use several different MCP clients (Claude Code, Cursor, VS Code, Windsurf, JetBrains, Codex CLI,
  Antigravity) and want the same safety/navigation layer to work identically everywhere, not locked
  to one host.
- You care about running locally, no outbound calls, no dependency on a paid embedding API.

## When not to choose `calm` (or worth weighing further)

- Your codebase is mostly outside the 6 Tier-0 + 18 Tier-0.5 languages + SQL's standalone indexer —
  25 languages total now that the 25-language expansion plan has shipped in full (see
  `docs/architecture.md`'s Multi-tier indexing section for the exact breakdown; Perl was the one
  language explicitly evaluated and excluded from that plan). For a codebase mostly in a language
  outside that set — no tree-sitter grammar wired in at all — `calm` can't parse anything beyond text
  search there, and the core value proposition (blast-radius) disappears entirely; CodeGraph
  (23 languages, full graph) may be a better fit for that case.
- You just need quick lookup/search, not an agent that edits files via MCP — pure read-only tools
  (CodeGraph, CodeGraphContext, claude-context) are lighter, have a bigger community, and less
  surface to worry about.
- You work at **multi-repo/enterprise** scale and need cross-repo search and navigation —
  Sourcegraph/Cody is built for exactly that problem; `calm` currently scopes to one repo at a time.
- You already use Aider and just need context auto-selected for each chat turn, without an
  interactive tool set (callers/callees/edit_context...) — Aider's built-in repo-map may already be
  enough.
- `calm` is a small, thinly-maintained project — if you need a tool that's been battle-tested by a
  large community for a long time, the options with more stars/users (Serena, CodeGraph) may be the
  safer choice on that axis, though the trade-off is no hard risk-gate before edits like `calm` has
  (Serena does have similarly durable cross-session memory — that's not the trade-off here, see the
  section below).

## Closest comparison: `calm` vs Serena

Serena is the tool closest to `calm` in shape — both let an agent *edit* code via MCP, not just read
it, and both have memory that survives across sessions (`write_memory`/`read_memory` in Serena,
`remember`/`recall` in `calm` — both verified across a real process restart, see B11). The main
difference: Serena is strong on language coverage (via LSP, 40+ languages) and is already the
de-facto standard widely used by the community; `calm` trades language coverage for depth — a call
graph with confidence labels attached (`resolved`/`inferred`/`formal`/`textual`) and a hard risk-gate
before editing a hub/high-caller symbol (Serena's `replace_symbol_body` has no confirmation field at
all — verified: it genuinely overwrote a real hub symbol with no confirmation asked), a point where
Serena genuinely lacks the capability, not just "undocumented."
