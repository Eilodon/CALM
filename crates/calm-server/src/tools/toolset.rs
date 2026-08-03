//! Tool-preset / composable-toolset resolution, extracted verbatim from
//! `tools/common.rs` (2026-07-28 hotspot split). Every item here was
//! `pub(crate)` in `common` and is re-exported through it
//! (`pub(crate) use crate::tools::toolset::*;`), so both the existing
//! `use super::common::*;` glob imports across `tools/*.rs` and the explicit
//! `common::resolve_preset(...)` / `common::effective_tool_names(...)` /
//! `common::SAFETY_FLOOR_TOOLSETS` references in `tools.rs`/`orient.rs`
//! keep resolving unchanged. Logic is byte-for-byte identical to the
//! pre-split version.
use super::*;

// ---------------------------------------------------------------------------
// Tool Presets — selective tool set definitions
// ---------------------------------------------------------------------------

pub(crate) fn preset_tools(preset: &str) -> Option<&'static [&'static str]> {
    match preset {
        "orient" => Some(&[
            "repo_overview",
            "locate",
            "dependencies",
            "hotspots",
            "fitness_report",
            "indexing_status",
        ]),
        "trace" => Some(&[
            "repo_overview",
            "search",
            "locate",
            "symbol_info",
            "source",
            "callers",
            "callees",
            "path",
            "dependencies",
            "indexing_status",
        ]),
        "edit" => Some(&[
            "repo_overview",
            "search",
            "locate",
            "symbol_info",
            "source",
            "callers",
            "callees",
            "edit_context",
            "edit_lines",
            "edit_symbol",
            "diff_impact",
            "indexing_status",
            // WS-1 durable-transaction/maintenance-outbox diagnostics
            // (added 2026-08-02, docs/plans/2026-08-02-toolsurface-
            // writesafety-ledger-research.md#part-1; see tools/txn.rs's
            // module comment for the journal's current shadow-vs-enforced
            // boundary) -- relevant to a session that is actively editing,
            // unlike orient/trace (read-only exploration).
            "edit_transaction_status",
            "maintenance_status",
            "retry_maintenance",
            "repair_consistency",
            // WS-6 first slice (2026-08-03, docs/plans/2026-08-03-ws6-
            // verification-pipeline-execution-plan.md): the tool that picks
            // up a transaction parked at VerifyPending -- same toolset as
            // the other txn diagnostics, same reasoning (relevant to a
            // session that is actively editing, not to orient/trace).
            "verify_change",
        ]),
        "compound" => Some(&[
            "repo_overview",
            "locate",
            "hotspots",
            "fitness_report",
            "source",
            "understand",
            "edit_context",
            "diff_impact",
            "session_context",
            "indexing_status",
            "remember",
            "recall",
        ]),
        "full" | "" => None, // None = all tools, no filtering
        _ => None,
    }
}

/// Toolset (module-domain) names for the composable preset registry
/// (2026-07-14 upgrade item, ported from github/github-mcp-server's
/// `pkg/inventory` toolset registry — verified against its real source:
/// a fixed, hand-chosen taxonomy of tool groups, not something read back
/// off individual tool metadata). One name per `#[tool_router]` module
/// merged into `CalmServer::full_tool_router()` — `toolset_tools` below
/// resolves a name to its actual tools by calling that module's own
/// router directly, so toolset MEMBERSHIP can never drift from what's
/// really registered even though this NAME list is hand-maintained (a
/// stale/renamed entry here just means that name resolves to zero tools,
/// caught immediately by `every_toolset_name_is_nonempty_and_real`
/// below). Mirrored in `calm_core::config::VALID_TOOLSET_NAMES` for
/// config.json's own light-weight preset syntax check (calm-core can't
/// depend on calm-server to validate this dynamically — see that const's
/// doc comment); `toolset_names_match_calm_core_valid_toolset_names`
/// below is the trip-wire if the two ever drift apart.
pub(crate) const TOOLSET_NAMES: &[&str] = &[
    "trace",
    "locate",
    "orient",
    "memory",
    "guardrails",
    "recover",
    "scip",
    "lsp",
    "security",
    "testgap",
    "inspect",
    "edit",
    "patterndebt",
    "txn",
];

/// Toolsets that runtime narrowing (`set_toolset`) can NEVER disable — the
/// non-negotiable floor. Verified against the real `#[tool_router]` grouping
/// (not assumed from tool names): `orient` backs the `oriented` gate and
/// carries `set_toolset` itself (so a session can always widen back out);
/// `guardrails` — NOT `edit` — is where `edit_context`/`diff_impact` are
/// actually registered (`guardrails_tool_router`, guardrails.rs), the
/// mandatory pre-edit/pre-commit gates; `recover` — also NOT `orient` — is
/// where `indexing_status`/`session_context` actually live
/// (`recover_tool_router`, recover.rs), the stuck-session escape hatch;
/// `edit` itself must stay so a session that clears `edit_context` can still
/// perform the edit (edit_lines/edit_symbol). Dropping any one of these four
/// would either make a safety gate unreachable (bypass) or make edits
/// deadlock (gate reachable but never satisfiable). Caught by
/// `safety_floor_toolsets_are_all_real_and_cover_the_gates` — the original
/// two-toolset guess (`["orient", "edit"]`) failed that test because
/// `edit_context`/`diff_impact`/`indexing_status`/`session_context` are not
/// where their names suggest. See Task 1.5 in the plan for the
/// enforcement-path argument.
pub(crate) const SAFETY_FLOOR_TOOLSETS: &[&str] = &["orient", "edit", "guardrails", "recover"];

/// Tool names belonging to one toolset, read directly off that module's
/// own `#[tool_router]`-generated router — the same accessor
/// `full_tool_router()` itself merges, so this can never name a tool the
/// toolset doesn't really have. `None` for an unrecognized toolset name.
fn toolset_tools(name: &str) -> Option<Vec<String>> {
    let router = match name {
        "trace" => CalmServer::trace_tool_router(),
        "locate" => CalmServer::locate_tool_router(),
        "orient" => CalmServer::orient_tool_router(),
        "memory" => CalmServer::memory_tool_router(),
        "guardrails" => CalmServer::guardrails_tool_router(),
        "recover" => CalmServer::recover_tool_router(),
        "scip" => CalmServer::scip_tool_router(),
        "lsp" => CalmServer::lsp_tool_router(),
        "security" => CalmServer::security_tool_router(),
        "testgap" => CalmServer::testgap_tool_router(),
        "inspect" => CalmServer::inspect_tool_router(),
        "edit" => CalmServer::edit_tool_router(),
        "patterndebt" => CalmServer::patterndebt_tool_router(),
        "txn" => CalmServer::txn_tool_router(),
        _ => return None,
    };
    Some(
        router
            .list_all()
            .into_iter()
            .map(|t| t.name.to_string())
            .collect(),
    )
}

pub(crate) fn calm_all_tool_names() -> std::collections::BTreeSet<String> {
    CalmServer::full_tool_router()
        .list_all()
        .into_iter()
        .map(|t| t.name.to_string())
        .collect()
}

/// The concrete tool-name set a session should see, given the preset ceiling
/// and an optional runtime narrowing. `None` narrowing = return the ceiling
/// unchanged. `Some(sets)` = (union of those toolsets' tools ∪ floor) ∩ ceiling.
/// The floor is unconditional; the ceiling is never exceeded.
pub(crate) fn effective_tool_names(
    preset_ceiling: &std::collections::BTreeSet<String>,
    narrowing: Option<&std::collections::BTreeSet<String>>,
) -> std::collections::BTreeSet<String> {
    let Some(sets) = narrowing else {
        return preset_ceiling.clone();
    };
    // Owned Vec first (not `.chain(...map(|s| &s.to_string()))`) — borrowing
    // a `&String` from a temporary created inside `.map` would not outlive
    // the chained iterator.
    let mut names: Vec<String> = sets.iter().cloned().collect();
    names.extend(SAFETY_FLOOR_TOOLSETS.iter().map(|s| s.to_string()));
    let mut visible: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    for name in &names {
        if let Some(tools) = toolset_tools(name) {
            visible.extend(tools);
        }
    }
    visible.intersection(preset_ceiling).cloned().collect()
}

/// Resolves a `preset` spec to the concrete tool-name allow-list it
/// grants — `Ok(None)` means "every tool, no filtering" (the `full`/`""`
/// sentinel `tool_router_for_preset` already treats specially).
///
/// Two syntaxes, both accepted:
/// - A single bare legacy name (`"orient"`/`"trace"`/`"edit"`/
///   `"compound"`/`"full"`/`""`) resolves EXACTLY as `preset_tools`
///   always has — unchanged, byte-for-byte, so every existing
///   config.json and `--preset` invocation keeps working identically.
///   These 4 names are hand-curated cross-cutting workflow bundles (e.g.
///   `"edit"`'s 12 tools span 6 different toolsets), not toolset unions —
///   that's deliberate and predates this function; see `preset_tools`.
/// - Anything else is parsed as a composable spec: a comma-separated list
///   of `TOOLSET_NAMES` entries (or `"full"`), each optionally prefixed
///   with `-` to SUBTRACT that toolset's tools from the accumulated set
///   instead of adding it (e.g. `"trace,security"` unions two toolsets;
///   `"full,-edit"` is every tool except the edit toolset's). This is the
///   new, actually-composable half of the registry — deliberately scoped
///   to whole-toolset granularity, not individual tool names, both to
///   keep `calm_core::config::VALID_TOOLSET_NAMES` a small syntactically
///   checkable list and because per-tool `-`-tokens are a natural, non-
///   breaking extension of this same grammar if ever needed later.
///
/// A single bare token that ISN'T one of the 5 legacy names now goes
/// through the composable parser too (e.g. `"security"` alone resolves
/// to just the security toolset) rather than silently falling through to
/// `preset_tools`'s catch-all `_ => None` (= unfiltered full access) —
/// that silent-fallback-to-everything behavior for an unrecognized name
/// was the actual footgun motivating this function: an unrecognized
/// *composable* token is now a hard `Err`, never a silent grant.
pub(crate) fn resolve_preset(
    preset: &str,
) -> anyhow::Result<Option<std::collections::BTreeSet<String>>> {
    let trimmed = preset.trim();
    if matches!(
        trimmed,
        "" | "full" | "orient" | "trace" | "edit" | "compound"
    ) {
        return Ok(preset_tools(trimmed).map(|list| list.iter().map(|s| s.to_string()).collect()));
    }

    let mut include_all = false;
    let mut included: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    let mut excluded: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();

    for raw_token in trimmed.split(',') {
        let token = raw_token.trim();
        if token.is_empty() {
            anyhow::bail!("empty token in preset spec {preset:?} (stray comma?)");
        }
        let (is_exclude, name) = match token.strip_prefix('-') {
            Some(rest) => (true, rest.trim()),
            None => (false, token),
        };
        if name == "full" {
            if is_exclude {
                excluded.extend(calm_all_tool_names());
            } else {
                include_all = true;
            }
            continue;
        }
        let Some(tools) = toolset_tools(name) else {
            anyhow::bail!(
                "unknown toolset {name:?} in preset spec {preset:?} — valid toolsets: {}. \
                 (Legacy bare presets full/orient/trace/edit/compound are also still accepted, \
                 alone, with no comma.)",
                TOOLSET_NAMES.join(", ")
            );
        };
        if is_exclude {
            excluded.extend(tools);
        } else {
            included.extend(tools);
        }
    }

    let base = if include_all {
        calm_all_tool_names()
    } else {
        included
    };
    Ok(Some(base.difference(&excluded).cloned().collect()))
}

#[cfg(test)]
mod preset_registry_tests {
    use super::*;

    #[test]
    fn every_toolset_name_is_nonempty_and_real() {
        let all_tools = calm_all_tool_names();
        for name in TOOLSET_NAMES {
            let tools = toolset_tools(name)
                .unwrap_or_else(|| panic!("TOOLSET_NAMES entry {name:?} does not resolve"));
            assert!(!tools.is_empty(), "toolset {name:?} resolved to zero tools");
            for t in &tools {
                assert!(
                    all_tools.contains(t),
                    "toolset {name:?} claims tool {t:?}, which full_tool_router() doesn't have"
                );
            }
        }
    }

    #[test]
    fn toolset_names_match_calm_core_valid_toolset_names() {
        let here: std::collections::BTreeSet<&str> = TOOLSET_NAMES.iter().copied().collect();
        let there: std::collections::BTreeSet<&str> = calm_core::config::VALID_TOOLSET_NAMES
            .iter()
            .copied()
            .collect();
        assert_eq!(
            here, there,
            "calm-server's TOOLSET_NAMES and calm-core's VALID_TOOLSET_NAMES have drifted apart"
        );
    }

    #[test]
    fn resolve_preset_legacy_bare_tokens_are_unchanged() {
        for name in ["", "full", "orient", "trace", "edit", "compound"] {
            let legacy = preset_tools(name).map(|l| {
                l.iter()
                    .map(|s| s.to_string())
                    .collect::<std::collections::BTreeSet<_>>()
            });
            assert_eq!(
                resolve_preset(name).unwrap(),
                legacy,
                "resolve_preset({name:?}) must match preset_tools({name:?}) exactly"
            );
        }
    }

    #[test]
    fn resolve_preset_composes_toolsets_by_union() {
        let trace_only = toolset_tools("trace").unwrap();
        let security_only = toolset_tools("security").unwrap();
        let combined = resolve_preset("trace,security").unwrap().unwrap();
        for t in trace_only.iter().chain(security_only.iter()) {
            assert!(combined.contains(t), "combined preset missing {t:?}");
        }
        assert_eq!(
            combined.len(),
            trace_only.len() + security_only.len(),
            "trace and security toolsets should not overlap"
        );
    }

    #[test]
    fn resolve_preset_supports_toolset_exclusion() {
        let edit_tools = toolset_tools("edit").unwrap();
        let resolved = resolve_preset("full,-edit").unwrap().unwrap();
        for t in &edit_tools {
            assert!(
                !resolved.contains(t),
                "excluded toolset tool {t:?} still present"
            );
        }
        assert!(
            resolved.contains("repo_overview"),
            "non-excluded tool should remain"
        );
    }

    #[test]
    fn resolve_preset_rejects_unknown_toolset() {
        assert!(resolve_preset("not_a_real_toolset").is_err());
        assert!(resolve_preset("trace,not_a_real_toolset").is_err());
    }

    #[test]
    fn resolve_preset_single_bare_toolset_no_longer_silently_grants_full_access() {
        // Before this upgrade, an unrecognized bare preset name silently
        // fell through `preset_tools`'s `_ => None` catch-all -> every
        // tool available, no error. A bare toolset name must now resolve
        // to just that toolset, not everything.
        let resolved = resolve_preset("security").unwrap().unwrap();
        let expected: std::collections::BTreeSet<String> =
            toolset_tools("security").unwrap().into_iter().collect();
        assert_eq!(resolved, expected);
    }

    #[test]
    fn safety_floor_toolsets_are_all_real_and_cover_the_gates() {
        // Every floor toolset must be a real toolset name…
        for name in SAFETY_FLOOR_TOOLSETS {
            assert!(
                TOOLSET_NAMES.contains(name),
                "floor toolset {name:?} not in TOOLSET_NAMES"
            );
        }
        // …and the union of floor tools must include the mandatory gate tools.
        let floor: std::collections::BTreeSet<String> = SAFETY_FLOOR_TOOLSETS
            .iter()
            .flat_map(|n| toolset_tools(n).unwrap())
            .collect();
        for required in [
            "repo_overview",
            "indexing_status",
            "session_context",
            "edit_context",
            "diff_impact",
            "set_toolset",
        ] {
            assert!(
                floor.contains(required),
                "safety floor missing gate tool {required:?}"
            );
        }
    }

    #[test]
    fn effective_tool_names_intersects_preset_and_floors_safety() {
        let preset_full = calm_all_tool_names();
        // Narrow to just "trace": result = trace tools + floor tools, all ⊆ preset.
        let narrowed = Some(std::collections::BTreeSet::from(["trace".to_string()]));
        let got = effective_tool_names(&preset_full, narrowed.as_ref());
        assert!(got.contains("edit_context"), "floor tool dropped");
        assert!(
            got.contains("callers") || got.contains("path"),
            "requested toolset tools missing"
        );
        assert!(got.is_subset(&preset_full), "escaped the preset ceiling");
        // None = unchanged passthrough of the preset.
        assert_eq!(effective_tool_names(&preset_full, None), preset_full);
    }
}
