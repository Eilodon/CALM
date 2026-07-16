---
title: Markdown as a document language — link/anchor integrity + front-matter validity, not tree-sitter syntax checking
date: 2026-07-16
SPEC_APPROVED: true
SPEC_ESCALATION: false
---

## Problem

Explicit framing for this spec, from this session's design discussion:
Markdown is a *document* language, not a *programming* language. It should
not be made to carry the same "does it parse cleanly" contract CALM already
gives real code — its correctness axis is content and semantics (do the
links this doc makes actually resolve, is the front-matter well-formed),
not syntax well-formedness.

Two things exist today, verified by reading the actual source, not docs:

1. **`extract_markdown_symbols`** (`crates/calm-core/src/indexer/parser.rs`,
   line 2492-2562) extracts ATX headings (`#`..`######`) as symbols, one
   per line, fence-aware (skips headings-that-look-like-comments inside
   fenced code blocks). Each symbol's `line_start == line_end` — a heading
   is a *location*, not a content span. Deliberately not routed through the
   tree-sitter/shallow-extraction pipeline code uses (own doc comment: "the
   default `#`-as-comment rule would eat every heading"). No links, no
   anchors, no front-matter are extracted at all.
2. **`check_config_drift`** (`crates/calm-core/src/analysis/config_drift.rs`)
   already is a real content checker for docs — flags a backtick-quoted bare
   file-path reference (e.g. `` `docs/foo.md` ``) that doesn't resolve to a
   real file. Wired into `fitness_report` (`crates/calm-core/src/fitness.rs
   ::run_fitness_check`, confirmed via `mcp__calm__callers`: 8 direct
   callers, all either the one real call site or its own test suite). Scope
   is narrow: bare-path prose mentions only. It does not understand markdown
   hyperlink syntax `[text](target)`, does not resolve `#anchor` fragments
   against a target file's actual headings, and has no concept of "which
   other docs point at this heading" as a blast-radius signal.

Separately, `edit_lines`/`edit_symbol`'s `parse_status` field is misleading
for markdown specifically (found this session, `crates/calm-core/src/
edit.rs::validate_syntax_diff` line 459-470): `language_for_extension("md")`
returns `Some("markdown")` — real, since markdown genuinely is one of
CALM's indexed languages (line 1582 of `lang_constants.rs`) — but
`parse_tree(content, "markdown")` has no tree-sitter grammar to run (it's a
dedicated line-scan, not tree-sitter), so the field always reports
`"skipped_unrecognized_language"` for `.md` edits. That name conflates
"CALM doesn't know this language at all" with "CALM indexes this language
but has no syntax grammar for it" — two different facts. **This spec
explicitly does not try to fix that by giving markdown a syntax-error-count
contract like code's** — per the framing above, that would be solving the
wrong problem. It's named here only as the reason a *separate*,
doc-shaped correctness signal is worth building instead.

## Design

**1. Extend the existing line-scan extractor, not tree-sitter.** Add link
and anchor extraction to (or alongside) `extract_markdown_symbols`, keeping
the same fence-aware, dedicated-scan architecture — never routed through the
code-shaped pipeline:
- Per heading: compute its GitHub-slug anchor (lowercase, strip punctuation,
  spaces→hyphens, `-2`/`-3`… suffix on duplicate slugs within one file —
  the same algorithm every markdown renderer CALM's docs actually render
  through uses, so a check against it means what a human clicking the link
  would experience).
- Per markdown link `[text](target)`: capture `target`, split into
  `path#fragment` / `#fragment`-only / bare-`path`, with source line.

**2. New analysis module, sibling to `config_drift.rs`** (e.g.
`analysis::doc_links`) that, given the link/anchor data above plus
`config_drift.rs::build_real_path_index` (reused, not duplicated — it
already does exactly the "does this path exist" half of the job):
- Flags a link whose file target doesn't resolve (config_drift's existing
  job, now understanding real markdown link syntax instead of only
  backtick-quoted bare paths).
- Flags a link whose `#fragment` doesn't match any heading-derived anchor in
  the resolved target file (new).
- Wired into `fitness_report` alongside `check_config_drift`, same shape.

**3. This closes a real gap in `diff_impact` for docs.** Today a markdown
heading can never appear in `affected_symbols` with a meaningful
`caller_count` — nothing builds edges for markdown, so headings are
call-graph-invisible. Once anchor targets are extracted, "which other docs
link to this heading's anchor" becomes a real, checkable relationship —
CALM's actual doc-equivalent of blast radius. Renaming a heading (which
silently changes its GitHub-slug anchor) becomes something `diff_impact` can
flag as risk: "N other file(s) link to `#old-heading-slug`, now dangling" —
squarely a content/semantic signal, not a syntax one. This is the same
category of bug this project has hand-caught and fixed on itself multiple
times already (stale doc claims found via manual re-verification, per this
session's own project memory) — this makes that class of check mechanical
instead of relying on a human/agent happening to notice.

**4. Front-matter YAML, as a narrow, separate, opt-in check.** A file
opening with `---\n...\n---` genuinely has parseable syntax in that block
even though the surrounding prose doesn't. Detect the block, parse with a
YAML crate (workspace currently has **no** YAML dependency — verified via
`Cargo.toml`; only `toml = "0.8"` exists, used elsewhere. A new dependency
would be needed, e.g. `serde_yaml` or `yaml-rust2` — pick in the plan, not
here). Surface as its own field (e.g. `frontmatter_status`), explicitly
**not** folded into the existing `parse_status` field — keeps "prose has no
syntax to validate" and "this specific structured block is malformed" as
two separate, honest signals instead of one field pretending to speak for
both.

**5. Explicitly out of scope for this spec:**
- Heading-hierarchy skip-level detection (h1→h3) — real but the most
  subjective/lowest-value item raised this session; revisit only if 1-4 ship
  and prove out the pattern.
- Any change to `parse_status`'s existing meaning for code languages.
- Routing markdown edits through the tree-sitter `PARSE_ERROR` gate — this
  spec's whole premise is that this is the wrong model for prose.
- Extending this to HTML (`<h1-6 id="...">`, `<a href="#...">`) — same
  architecture would apply almost directly once 1-3 land, but is a separate
  follow-on, not bundled here.

## Open questions for audit-design

1. **Edge computation cost/placement**: does anchor/link resolution run at
   index time (stored as edges in the graph DB, like real call-graph edges)
   or on-demand at `fitness_report`/`diff_impact` call time (like
   `config_drift` does today, which re-walks `doc_paths` fresh every call)?
   The blast-radius use case (item 3) wants it queryable the way `callers`
   is — that likely means index-time storage, a bigger change than
   `config_drift`'s current on-demand model.
2. **Anchor-slug algorithm fidelity**: GitHub's actual slugger has edge
   cases (emoji, non-ASCII, HTML entities in heading text) — how much
   fidelity is required before "flags a dangling link" is trustworthy enough
   not to train agents to ignore it (the same "precision over coverage"
   lesson `calm-nudge.sh`'s own 2026-07-13 redesign already learned the hard
   way for a different nudge).
3. **New dependency justification** for whichever YAML crate is chosen (item
   4) — supply-chain/audit cost vs. value, given this project's existing
   carefulness about dependency footprint (e.g. the tree-sitter ABI-pinning
   constraints documented elsewhere in this codebase).
4. **Cross-file rename ergonomics**: if `diff_impact` starts flagging
   heading renames as breaking N other docs' anchors, does this become
   noisy/annoying for the extremely common case of a doc-only PR that
   legitimately renames a heading and updates its own known referrers in the
   same commit — needs a design for "already-fixed-in-this-diff" awareness,
   not just "matches an old anchor somewhere."

## Resolution — 2026-07-16 verification (this session, pre-re-audit)

Each Required Revision from the first audit-design pass was checked against
the actual running codebase (not re-derived from the spec's own prose) via
`mcp__calm__source`/`file_overview`/`repo_overview`. Findings below replace
the corresponding Open Question; the Open Questions section above is left
as-is as the historical record of what was unresolved going in.

**(a) Item 3 v1 scope — NOT descoped to v2; shipped on-demand instead of
index-time.** The audit's own math ("full doc-corpus scan on every
diff_impact call") assumed a scan expensive enough to force index-time
storage. Verified corpus size via `repo_overview.module_map`: `docs/` is 50
files / 753 symbols total — the same order of magnitude `check_config_drift`
already re-walks on every `fitness_report`/`diff_impact` call today via
`build_real_path_index`'s full-tree walk, with no reported perf complaint in
this project's history. Decision: ship items 1+2+3 together in v1, all
on-demand (`doc_links` module mirrors `config_drift`'s call shape exactly —
no schema migration, no graph DB edge storage). Revisit index-time only if
doc corpus size grows an order of magnitude or profiling shows measured
diff_impact latency regression — not preemptively.

**(b) Stage-7 interaction — resolved by construction, independent of (a).**
Doc-link/anchor findings are surfaced as a new, separate informational field
(e.g. `diff_impact.doc_link_impact`), explicitly excluded from
`aggregate_risk`'s computation — verified `aggregate_risk` today is driven
solely by `compute_touch_risk`/hub-flag logic over the `symbols` table
(`crates/calm-server/src/tools/edit.rs::compute_touch_risk`,
`crates/calm-core/src/graph/hub.rs::update_is_hub_flags`), which doc-link
edges must not feed into for v1. This also substantially defangs Open
Question 4 (cross-file rename ergonomics): since the signal is advisory, not
gating, "noisy" costs an agent a read, not a blocked commit. The
"already-fixed-in-this-diff" check itself is cheap to add regardless: for
each flagged external referrer file, check whether that path is also present
in the same diff's `files_changed` (diff_impact already has this list) and
suppress/downgrade the finding if so.

**(c) `resolve_reference` reuse — VERIFIED NOT COMPATIBLE AS-IS, not merely
unverified.** Read `crates/calm-core/src/analysis/config_drift.rs::
resolve_reference` (line 67-82) directly: it resolves a reference only two
ways — (1) `project_root.join(reference)` (repo-root-relative) or (2) an
anywhere-in-repo `ends_with("/{reference}")` suffix match. It has no
"relative to the referring file's own directory" mode at all. Markdown links
routinely use `./sibling.md` and `../other/doc.md` — the latter would try to
resolve as `project_root.join("../other/doc.md")` (escapes the repo root,
essentially always fails) and fall through to the suffix match, which also
fails (no real path literally ends in `../other/doc.md`) — a **false
positive** on a perfectly valid link. `./sibling.md` survives only because
`extract_path_refs` strips the `./` prefix before matching (that stripping
lives in the sibling `doc_refs.rs`, a different module than the one the spec
named) — but then resolves via the anywhere-in-repo suffix match, which
picks arbitrarily among same-named files in different directories: a
**false negative** if the wrong one matches, exactly the failure direction
Open Question/FM2 worried about but here found in the reuse target instead.
Plan must add a distinct `resolve_markdown_link_target(referring_doc_dir,
real_paths, target)` that tries relative-to-file resolution FIRST, falling
back to the existing repo-root/suffix logic only for bare mentions — reusing
`build_real_path_index`'s output (the path universe) is still valid and
cheap, `resolve_reference` itself is not reusable unchanged.

**(d) YAML crate + hub-gating interaction — both verified, both need an
explicit v1 decision, not a default.** `Cargo.toml` confirmed to have zero
yaml/slug dependencies today (grep, zero hits) — if front-matter (item 4)
ships in v1, prefer `yaml-rust2`/`saphyr` over `serde_yaml` (the latter is in
maintenance-mode/archived upstream); pin the exact choice in the
implementation plan with a supply-chain check (unsafe-code audit, transitive
dep count), not here. Separately, `update_is_hub_flags`
(`crates/calm-core/src/graph/hub.rs`, line 11-86) is a mechanical `UPDATE`
over the `symbols` table keyed only on `caller_count`/`coreness` percentile
— it has **no kind-based exclusion**. Once anchor edges give a heading a
real `caller_count`, a heavily-cross-linked heading (this repo's own `docs/`
already has 50 files to cross-link from) **will** mechanically qualify as
`is_hub`, inheriting `edit_lines`'s `confirm:true` + grounded-`reason`
ceremony that a hot code path needs (`compute_touch_risk`,
`crates/calm-server/src/tools/edit.rs`, line 1168-1203) — this is not a
"could," it is what the current query does today, verified by reading it.
**v1 decision: exclude `SymbolKind::Heading` from `update_is_hub_flags`'s
hub-eligible query** (a `WHERE kind != 'heading'` addition or equivalent) —
a doc-heading rename should not need the same edit ceremony as touching a
hub function. This must be a stated, explicit change in the plan, not left
as accidental default behavior.

## Risk Assessment (audit-design)
<!-- audit-design: DO NOT DUPLICATE — update this section, do not append a second one -->
<!-- last-run: 2026-07-16 | trigger: UPDATE (re-audit after Resolution section verification) -->

**Tier:** 2 (Production) — unchanged from the first pass: item 3 still touches
`diff_impact`'s output shape, a hook-enforced pre-commit gate, even though v1
now keeps it out of `aggregate_risk` specifically.

### Failure Modes

1. **New `diff_impact.doc_link_impact` field breaks the schema-snapshot test,
   not just `aggregate_risk`** — MEDIUM — mitigation in plan: NO (not yet in
   either audit pass). Verified `crates/calm-server/src/tools.rs::
   tool_schemas_match_committed_snapshots` (line 921-938) locks tool output
   shapes against committed files in `crates/calm-server/src/__toolsnaps__/`,
   and `diff_impact.snap` is one of them (confirmed on disk). Adding ANY new
   field to `diff_impact`'s response — even a purely additive, non-
   `aggregate_risk` one — requires updating this snapshot as an explicit
   plan task, or CI fails on the first commit that ships it.
2. **The Resolution section's own item (a) recommendation was wrong before
   verification, and would have shipped a worse design if not caught** —
   MEDIUM, now RESOLVED — mitigation in plan: YES (see Layer Signals L3).
   This session's own first-pass answer to Open Question 1 ("ship on-demand,
   avoid index-time's bigger schema-migration cost") was itself an
   unverified assumption, structurally identical to the failure pattern this
   whole audit exists to catch. Reading `crates/calm-core/src/db/schema.rs`
   directly shows `call_edges` already has a generic `edge_kind TEXT NOT
   NULL DEFAULT 'call'` column, already used for a second non-"call" value
   (`"reference"`, SQL table-read edges, per `CallEdge`'s own doc comment in
   `indexer/edges.rs`) — meaning index-time storage of doc-link/anchor edges
   needs **zero schema migration**, contradicting both audit passes' cost
   model. Corrected recommendation below.
3. **`SymbolKind::Heading` hub-exclusion (Resolution item d) fix point
   verified real, but only checked for `update_is_hub_flags` — not for
   whichever new edge-emission code path item 3 ends up using** —
   LOW-MEDIUM — mitigation in plan: PARTIAL. The `symbols.kind` TEXT column
   is confirmed (schema.rs line 4-10), so `WHERE kind != 'heading'` is a
   real, implementable fix. But if item 3 moves to index-time (per corrected
   FM2 above), the NEW edge-emission code writing `call_edges` rows with
   `edge_kind='doc_link'` must also be checked for whether it accidentally
   feeds `caller_count` on the `symbols` table in a way that re-triggers hub
   eligibility even with the `kind != 'heading'` guard elsewhere — this
   specific interaction (new edge writer → existing caller_count
   aggregation → existing hub query) was not traced end-to-end this session
   and should be an explicit plan task, not assumed safe by analogy.

### Layer Signals

- L1 Logic: original L1 (case-collision slug dedup, "Setup" vs "setup")
  still open, no new evidence gathered this pass — carry forward unresolved.
- L2 Concurrency: no signal (unchanged).
- L3 Data: CORRECTED, not just re-examined. `call_edges.edge_kind` already
  generalizes (verified, see FM2) and the project has already extended it
  once before (SQL `"reference"` edges) — this is proven-safe precedent,
  not a novel architectural leap. Combined with the existing two-pass
  symbol-then-edge indexer architecture (`indexer/edges.rs::
  insert_call_edges_batch`, a generic batch-insert, not call-specific),
  **index-time is now the recommended v1 placement for item 3, reversing the
  Resolution section's on-demand recommendation** — it is not materially
  more expensive than on-demand once the schema fact is known, and it
  delivers the actual `callers`-like queryability Design item 3 wants,
  natively, instead of an on-demand approximation. On-demand remains the
  fallback if implementation reveals the cross-file two-pass ordering
  doesn't extend cleanly to markdown's link-then-target-heading resolution
  (untested assumption, flagged below).
- L4 Integration: unchanged from first pass — YAML crate choice still
  deferred to plan (yaml-rust2/saphyr over serde_yaml, per Resolution (d)).
- L5 Security: unchanged (YAML resource-exhaustion) PLUS one new item found
  only after designing the relative-link-resolution fix (Resolution (c)):
  `resolve_markdown_link_target`'s relative-to-file fallback (`../`-walking)
  must clamp its resolved path to stay within `project_root` — an unclamped
  `../../../` chain could resolve outside the repo tree. Not a live
  vulnerability today (no such function exists yet) but a correctness bound
  the plan must state explicitly for whoever writes it.
- L6 Observability: unchanged, still unaddressed — carry forward as a plan
  task regardless of on-demand/index-time placement.
- L7 Cross-cutting: substantially improved (Resolution (b)'s
  already-fixed-in-this-diff check, informational-not-gating placement) but
  the check itself ("is the referrer file also in this diff's
  files_changed") is new code with its own test surface — not free, should
  be sized as a real task in the plan, not assumed trivial because it
  "sounds cheap."

### Assumptions to Verify

- **ASSUMED, now VERIFIED FALSE:** Resolution (a)'s claim that index-time
  storage is "a bigger change" than on-demand — corrected in FM2 above; not
  carried forward.
- **ASSUMED, now VERIFIED TRUE:** `symbols.kind` column exists and can
  support a heading exclusion (schema.rs confirmed) — Resolution (d)'s fix
  point is real.
- **NEW ASSUMPTION (this pass):** the existing cross-file two-pass
  edge-resolution architecture (built for code call-graphs) extends cleanly
  to markdown's "link in file A resolves to a heading-anchor in file B"
  case without needing its own bespoke resolution pass. Plausible by analogy
  (SQL's `edge_kind='reference'` precedent) but not read end-to-end this
  session — the first implementation task should confirm this before
  assuming the architecture is a drop-in fit.
- **DEFERRED ("TBD"), unchanged:** exact YAML crate pick (Resolution (d)) —
  plan-time decision with a supply-chain check.
- **DEFERRED, unchanged:** GitHub-slugger fidelity implementation — port the
  documented algorithm (not hand-roll), golden-fixture tests as a completion
  gate (Resolution/original FM2).

### Abductive Hypotheses

1. **Toolsnap schema-lock interacts with an "additive, informational-only"
   field exactly the way the original audit's Abductive 1 warned about a
   different gate.** Item 3 was redesigned specifically to stay out of
   `aggregate_risk` (Resolution b) to avoid Stage-7 gate noise, but the
   schema-snapshot test doesn't distinguish "additive/informational" from
   "any shape change at all" — the same failure shape (a technically
   correct, intentionally gentle design change still trips an existing
   hard-enforced check it wasn't designed against) recurs one level down
   from where the first audit caught it. Same lesson, needs re-applying: any
   new `diff_impact` field is a snapshot-breaking change regardless of how
   carefully it avoids `aggregate_risk`.
2. **This audit's own Resolution section (a) demonstrates the exact
   "asserted without checking" pattern the spec was HOLD-gated for, one
   level up.** The deep-dive investigation confidently proposed
   on-demand-over-index-time using a cost argument (doc corpus size vs.
   `config_drift`'s existing walk) without first reading `db/schema.rs`. Had
   this second verification pass not happened, the plan would have shipped
   the wrong architectural choice with high confidence and a
   plausible-sounding justification — worth naming explicitly as a
   demonstration of why this project's "verify against running code, not
   prose" discipline needs to apply recursively to the audit's own
   intermediate conclusions, not just the original spec's claims.

### Gate Result

**PASS WITH FLAGS.** All four Required Revisions from the first HOLD are now
resolved against verified code, with one correction surfacing mid-
verification (index-time, not on-demand, is the right v1 placement for item
3 — cheaper than assumed, and delivers the real capability instead of an
approximation). No remaining spec-level inconsistency (the original HOLD's
core complaint — a capability asserted while its mechanism was marked
undecided) survives: item 3's mechanism is now specified and verified cheap.
Remaining items are implementation-plan-level, not spec-level, and must be
carried into `writing-plans` as explicit tasks:

1. Update `crates/calm-server/src/__toolsnaps__/diff_impact.snap` (and any
   other affected toolsnap) as part of this change, not as an afterthought
   (FM1).
2. Confirm the two-pass edge-resolution architecture extends to markdown
   link→anchor resolution before assuming it (new Assumption above) —
   spike/prototype this specific piece first if there's doubt.
3. Add a `kind != 'heading'`-equivalent exclusion to `update_is_hub_flags`,
   AND verify the new edge-emission path doesn't re-introduce hub
   eligibility another way (FM3).
4. Clamp `resolve_markdown_link_target`'s relative-path resolution to
   `project_root` (L5, new).
5. Pin the YAML crate choice (yaml-rust2/saphyr recommended) with a
   supply-chain check if item 4 (front-matter) ships in this same v1.
6. Port the documented GitHub-slugger algorithm verbatim + golden-fixture
   tests (non-ASCII, emoji, case-collision) as a completion gate, not a
   nice-to-have.
7. Design L6 observability (ran-and-clean vs. not-run-yet) for the new
   check, matching whatever pattern `indexing_status`/`config_drift` already
   use.

Proceed to `writing-plans` with the above 7 items as scoped, risk-scored
tasks.
