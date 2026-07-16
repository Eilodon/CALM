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

## Risk Assessment (audit-design)
<!-- audit-design: DO NOT DUPLICATE — update this section, do not append a second one -->
<!-- last-run: 2026-07-16 | trigger: NORMAL -->

**Tier:** 2 (Production) — upgraded from an initial Tier-1 read because item
3 (diff_impact doc blast-radius) directly touches `diff_impact`'s
`aggregate_risk` computation, a hard, hook-enforced pre-commit gate (AGENTS.md
Stage 7) — a bug here has the same class of workflow impact as a bug in the
gate itself, even though this feature is additive/informational by intent.

### Failure Modes

1. **Index-time vs. on-demand placement (Open Question 1) is left
   unresolved, but the Design section's item 3 already asserts the
   capability that decision determines** — HIGH — mitigation in plan: NO.
   On-demand (config_drift's current model, re-walking `doc_paths` fresh
   every call) cannot cheaply answer "which docs across the whole repo link
   here" without a full doc-corpus scan on every `diff_impact` call — a
   tool this project's own hooks call on every commit/push, no exceptions.
   Index-time (real edges in the graph DB, queryable like `callers`) is a
   materially bigger change — schema/migration, not mentioned anywhere in
   the Design section. The spec's own Design text describes item 3 as
   already achieved ("becomes a real, checkable relationship... CALM's
   actual doc-equivalent of blast radius") while the mechanism to deliver it
   is simultaneously flagged TBD in Open Questions — the same "individually-
   correct components, broken combination" pattern this project's own prior
   audit (sibling onboarding spec, Item A Failure Mode 2) already named as a
   real risk shape.
2. **Anchor-slug fidelity gap risks silent false negatives, not just noisy
   false positives** — MEDIUM-HIGH — mitigation in plan: NO. Open Question 2
   only names the precision risk ("trustworthy enough not to train agents to
   ignore it"). The opposite direction is worse and unexamined: a slugger
   simpler than GitHub's real algorithm makes both sides of a link/anchor
   comparison agree with each other using the same wrong function — a
   genuinely broken link on the real rendered page passes CALM's check
   silently, offering false confidence instead of an honest, if noisy,
   warning.
3. **`build_real_path_index`/`check_config_drift` reuse (Design item 2) is
   asserted as compatible ("already does exactly the ... half of the job")
   without checking it against markdown-link-syntax conventions** — MEDIUM —
   mitigation in plan: NO. That function was built for backtick-quoted bare
   paths (its own test suite covers an exact-path form and a "short suffix
   form"); markdown links add relative-to-current-file resolution, `./`
   prefixes, and possible query/fragment combinations bare-path mentions
   never had. Reuse is the right instinct but unverified as stated.

### Layer Signals

- L1 Logic: the GitHub-slug dedup-suffix behavior for repeated headings
  within one file (`-1`/`-2`…) has an easy-to-miss edge case — whether two
  headings that differ only in case ("Setup" vs "setup") collide into the
  same base slug and how the numbering resolves — untested branch, not
  addressed.
- L2 Concurrency: no signal — read/analysis feature, not concurrent-write
  state (unlike the sibling hook spec).
- L3 Data: real schema question gated entirely on FM1's resolution — no
  schema sketch or migration story for existing `.calm/index.db` files is
  in the Design section either way.
- L4 Integration: front-matter YAML (item 4) introduces a new external
  dependency with no crate chosen and no stated vetting bar (maintenance
  status, unsafe code, transitive footprint) — notable given this project's
  own documented carefulness about dependency footprint elsewhere (the
  tree-sitter ABI-pinning constraints the spec itself cites).
- L5 Security: narrow but real — a YAML parser processing repo doc content
  is processing untrusted-ish input in any workflow that ingests external
  PRs' docs (not the typical single-maintainer case, but a real one for an
  OSS project). Whether the chosen crate guards against
  billion-laughs/anchor-expansion-style resource exhaustion isn't
  addressed.
- L6 Observability: not addressed — no signal for how a user/agent
  distinguishes "link-integrity check ran and found nothing" from "didn't
  run yet" (the same ambiguity `config_drift` already has today, extended
  to new surface here rather than newly introduced).
- L7 Cross-cutting (idempotency/noise): Open Question 4 (cross-file rename
  ergonomics, "already-fixed-in-this-diff" awareness) is exactly this
  layer's concern and is explicitly unresolved by the spec's own admission.

### Assumptions to Verify

- **ASSUMED:** `build_real_path_index` reuse "just works" for markdown-link
  targets without adaptation (FM3) — stated close to fact in Design item 2,
  not verified.
- **DEFERRED ("TBD"):** index-time vs. on-demand placement (Open Q1) — and,
  compounding it, the spec doesn't state that this choice also determines
  whether item 3 is deliverable as pitched (FM1).
- **DEFERRED ("TBD"):** YAML crate choice (Open Q3).
- **ASSUMED:** an anchor-slug implementation with "good enough" fidelity is
  achievable — no concrete algorithm/library proposed, only a fidelity
  concern noted (Open Q2).

### Abductive Hypotheses

1. **Interaction with this project's OWN already-shipped hard gate.** Item
   3, even if link-extraction and anchor-resolution are each individually
   correct, creates a new interaction with `diff_impact`'s Stage-7
   hard-enforced pre-commit gate. A trivial, correct, self-contained doc
   commit that renames a heading could newly trip elevated `aggregate_risk`
   for what is otherwise a routine prose edit — the exact "a nudge/gate that
   fires on correct usage teaches agents to discount every future signal"
   failure class `calm-nudge.sh`'s own 2026-07-13 redesign already learned
   the hard way, now risked again on a gate that is not this feature's own
   but an existing one it plugs into. The spec does not examine this
   interaction at all.
2. **Doc-anchor "hub" status could unexpectedly trigger code-symbol-grade
   edit friction.** A heavily-cross-linked anchor (this project's own
   `docs/` already has 48 files per `repo_overview`) could, once real edges
   exist, get flagged `is_hub: true` through the same machinery real code
   hubs use — and `edit_lines`' own schema (confirmed this session) requires
   `confirm: true` + a same-session `edit_context` call + a grounded
   `reason` for exactly that flag. Editing a heavily-linked heading in a
   big README could start requiring the same heavyweight confirmation dance
   a hot code path needs. The spec never states whether doc anchors should
   be exempted from hub-gating or deliberately included in it.

### Gate Result

**HOLD.** The spec's own Design section (item 3) asserts a capability whose
delivery mechanism is simultaneously marked undecided in its Open Questions
(FM1) — that inconsistency, plus an unexamined interaction with an already-
shipped hard-enforced gate (Abductive 1) and an unexamined false-negative
failure direction (FM2), are spec-level gaps, not implementation-plan
details. Required revisions before re-audit: (a) commit item 3 to an
explicit v1 scope — recommended: ship items 1-2 (extraction + resolution,
on-demand, `config_drift`-style) first, and explicitly descope the
diff_impact/blast-radius payoff (item 3) to a v2 follow-on once 1-2 are
proven, rather than bundling a graph-schema migration into the same change
as a brand-new extractor; (b) if item 3 ships at all in v1, an explicit
noise-mitigation design for its Stage-7 interaction (Abductive 1) — at
minimum, keep doc-anchor findings out of `aggregate_risk` and surface them
as a separate informational field for v1; (c) address the false-negative
direction of slug fidelity (FM2), not just precision/false-positive framing;
(d) verify, not assume, that `build_real_path_index` reuse fits
markdown-link-target conventions (FM3) before a plan assumes it as free
reuse.
