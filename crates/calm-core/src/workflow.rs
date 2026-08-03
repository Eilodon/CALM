//! Shared CALM workflow guidance text — single source of truth for the
//! `calm_workflow` MCP Prompt (`calm-server`) and the `calm init
//! --agents-md` scaffold (`calm-cli`), so the two surfaces can't silently
//! drift apart. `.claude/skills/calm-guide/SKILL.md` names exactly this
//! failure mode for this repo's own `AGENTS.md`; keeping one const behind
//! both consumers is the same fix applied one layer down, to every project
//! that scaffolds this content into its own `AGENTS.md`.

/// Start marker for `calm init --agents-md`'s managed block inside an
/// external project's `AGENTS.md`. Exported so `calm-cli` never has to
/// duplicate the literal string by hand.
pub const AGENTS_MD_MARKER_START: &str = "<!-- calm:workflow:start -->";
/// End marker — see [`AGENTS_MD_MARKER_START`].
pub const AGENTS_MD_MARKER_END: &str = "<!-- calm:workflow:end -->";

/// Condensed 8-stage CALM MCP tool workflow. Deliberately carries no
/// trailing pointer to "see AGENTS.md for full detail": that sentence means
/// different things depending on where this text ends up (a live prompt
/// response, where AGENTS.md may not exist at all, vs. the scaffolded file
/// itself, where pointing at itself would be circular) — each caller
/// appends its own contextually-correct trailer instead of baking one in
/// here.
pub const CALM_WORKFLOW_GUIDE: &str = "\
CALM MCP tool workflow -- 8 stages, 34 tools, `suggested_next` on every response tells you what to call next:
1. Orient -- repo_overview() ALWAYS first, then hotspots()/fitness_report() as needed.
2. Locate -- locate(query) (search+file_overview+symbol_info in 1 call) or search(query, kind=...).
3. Inspect -- source(symbol) for a symbol-precise read, or understand(symbol) for locate+source+callers together.
4. Trace -- callers(symbol), callees(symbol), path(from,to), dependencies(path).
5. Pre-Edit -- edit_context(symbol): MANDATORY before touching any symbol, no exceptions for real code (a Markdown/text heading is the one exception -- never carries a blast radius to check).
6. Edit -- edit_symbol(...)/edit_lines(...): CALM's own write path, hash-verified, risk-gated, reindexes immediately. Fall back to native Edit/Write only for a brand-new file or a path CALM doesn't index (dotdirs, target/, node_modules/, etc.).
7. Verify -- diff_impact(staged=true) MANDATORY before any commit/push -- no exceptions.
8. Recover -- session_context() after 10+ calls without progress; remember(topic, content)/recall(topic) for durable cross-session notes.
Only steps 5 and 7 are hard-enforced (a hook denies the native equivalent without them, under Claude Code); the rest is strongly recommended but not blocking.";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn workflow_guide_names_every_stage_and_both_hard_gates() {
        for stage in [
            "1. Orient",
            "2. Locate",
            "3. Inspect",
            "4. Trace",
            "5. Pre-Edit",
            "6. Edit",
            "7. Verify",
            "8. Recover",
        ] {
            assert!(
                CALM_WORKFLOW_GUIDE.contains(stage),
                "missing stage: {stage}"
            );
        }
        assert!(CALM_WORKFLOW_GUIDE.contains("edit_context"));
        assert!(CALM_WORKFLOW_GUIDE.contains("diff_impact"));
        assert!(
            !CALM_WORKFLOW_GUIDE.contains("AGENTS.md"),
            "the shared core text must stay pointer-free — callers append their own trailer"
        );
    }

    #[test]
    fn markers_are_html_comments_and_distinct() {
        assert_ne!(AGENTS_MD_MARKER_START, AGENTS_MD_MARKER_END);
        assert!(AGENTS_MD_MARKER_START.starts_with("<!--"));
        assert!(AGENTS_MD_MARKER_END.starts_with("<!--"));
    }
}
