use super::common::*;
use super::locate::*;
use super::*;

/// Lookback window for the `trend` field — chosen to match typical daily CI
/// cadence (one `calm fitness-check` snapshot/day) while staying short enough
/// to reflect recent activity rather than all-time drift.
const EDIT_CONTEXT_TREND_LOOKBACK_DAYS: i64 = 7;

impl CalmServer {
    #[tool(
        name = "edit_context",
        description = "ALWAYS CALL THIS before any code modification — mandatory, never skip. USE WHEN: you are about to edit, refactor, or delete a symbol. NOT FOR: read-only inspection (use symbol_info + source). NOT post-edit (use diff_impact)."
    )]
    pub(crate) fn edit_context(&self, #[tool(aggr)] p: EditContextParams) -> String {
        self.timed_tool("edit_context", || {
            // READ-only: open a dedicated read connection (SINGLE_WRITER enforcement)
            let conn = match self.make_read_conn() {
                Ok(c) => c,
                Err(e) => return format!(r#"{{"error": "db connection failed: {e}"}}"#),
            };
            let resolution = resolve_symbol(&conn, &p.symbol, p.path.as_deref(), p.line);
            let c = match resolution {
                SymbolResolution::NotFound => return not_found_json(&p.symbol),
                SymbolResolution::Ambiguous(candidates) => return ambiguous_json(&candidates),
                SymbolResolution::Found(c) => *c,
            };
            self.track_symbol(&c.qualified_name);
            self.track_file(&c.path);

            // For edit_lines/edit_symbol's expected_hash — computed the same
            // way apply_hunks hashes a range, so this checksum is directly
            // usable without a separate round trip to learn it.
            let range_checksum = std::fs::read_to_string(self.project_root.join(&c.path))
                .ok()
                .and_then(|content| {
                    calm_core::edit::range_checksum(
                        &content,
                        c.line_start as usize,
                        c.line_end as usize,
                    )
                });

            let callers: Vec<CallerEntry> = {
                let mut stmt = conn
                    .prepare(
                        "SELECT from_symbol, from_path, edge_confidence, call_site_line, edge_kind
                         FROM call_edges WHERE to_symbol = ?1 AND ruled_out_by_scip = 0",
                    )
                    .unwrap();
                stmt.query_map(rusqlite::params![c.qualified_name], |row| {
                    let path: String = row.get::<_, String>(1).unwrap_or_default();
                    let line: Option<i64> = row.get(3)?;
                    Ok(CallerEntry {
                        symbol: row.get(0)?,
                        preview: line_preview(&self.project_root, &path, line),
                        path,
                        edge_confidence: row.get(2)?,
                        edge_kind: row.get(4)?,
                        line,
                    })
                })
                .unwrap()
                .filter_map(|r| r.ok())
                .collect()
            };

            let callees: Vec<CalleeEntry> = {
                let mut stmt = conn
                    .prepare(
                        "SELECT to_symbol, to_path, edge_confidence, call_site_line, edge_kind
                         FROM call_edges WHERE from_symbol = ?1 AND ruled_out_by_scip = 0",
                    )
                    .unwrap();
                let from_path = c.path.clone();
                stmt.query_map(rusqlite::params![c.qualified_name], |row| {
                    let line: Option<i64> = row.get(3)?;
                    Ok(CalleeEntry {
                        symbol: row.get(0)?,
                        path: row.get::<_, String>(1).unwrap_or_default(),
                        edge_confidence: row.get(2)?,
                        edge_kind: row.get(4)?,
                        preview: line_preview(&self.project_root, &from_path, line),
                        line,
                    })
                })
                .unwrap()
                .filter_map(|r| r.ok())
                .collect()
            };

            let config = calm_core::config::load_config(&self.project_root).unwrap_or_default();

            let blast_radius = {
                let (entries, _capped) = transitive_bfs(
                    &conn,
                    &c.qualified_name,
                    EdgeDirection::Callers,
                    config.callers.max_depth_cap,
                    config.callers.transitive_timeout_ms,
                );
                let mut files_affected: Vec<String> =
                    entries.iter().map(|e| e.path.clone()).collect();
                files_affected.sort();
                files_affected.dedup();
                BlastRadiusInfo {
                    transitive: entries.len() as i64,
                    files_affected,
                }
            };

            let co_changed_files: Vec<CoChangedFileOutput> =
                calm_core::analysis::cochange::compute_co_changes(
                    &self.project_root,
                    &c.path,
                    &config.cochange.since,
                    config.cochange.min_co_changes,
                    config.cochange.top_n,
                )
                .entries
                .into_iter()
                .map(CoChangedFileOutput::from)
                .collect();

            // `callers` (shown in full above, `ambiguous` entries included so
            // the caller can judge each one) is not the same count as "risk of
            // touching this symbol" — an `ambiguous` edge is index-time fan-out
            // to every same-named candidate when a call's receiver type
            // couldn't be resolved (see `refresh_caller_counts`'s doc comment),
            // not a confirmed caller of *this* one. Counting it here inflated
            // risk to "high" purely from name-collision noise (e.g. a common
            // method name shared by several unrelated classes) even when the
            // real, confirmed caller count was low or zero — the same
            // "confirmed caller" definition `symbols.caller_count` already
            // uses elsewhere in this codebase, now applied consistently here.
            let confirmed_caller_count = callers
                .iter()
                .filter(|c| c.edge_confidence != "ambiguous")
                .count();
            let mut risk = risk_level_from_caller_count(confirmed_caller_count as i64).to_string();

            // Raw caller-count alone can't see entry points, test-only
            // helpers, or runtime dispatch (reflection, framework callbacks)
            // that never shows up in the static call graph — exactly the
            // richer signal `compute_dead_code_confidence` already computes
            // for `symbol_info`'s `build_health`, just never wired in here
            // before. Only relevant when there are zero confirmed callers
            // (`risk_level_from_caller_count` already tiers nonzero counts
            // sensibly on its own): if the dead-code heuristic disagrees
            // that this looks safely removable — `"none"` (confirmed
            // entry-point/test) or `"low"` (runtime-covered despite no
            // static callers, or genuinely ambiguous scope) — escalate from
            // "low" to "medium" so the caller doesn't read a bare 0-caller
            // count as "safe to delete" when it isn't.
            let abs_path =
                calm_core::analysis::coverage::normalize_path(&self.project_root.join(&c.path));
            let is_private = calm_core::analysis::dead_code::is_private_symbol(
                &c.language,
                &c.name,
                &c.signature,
            );
            let scope_clear = calm_core::analysis::dead_code::scope_clear_for_language(&c.language);
            let (dead_code_confidence, dead_code_source) =
                calm_core::analysis::dead_code::compute_dead_code_confidence(
                    &abs_path,
                    c.line_start,
                    c.line_end,
                    confirmed_caller_count as i64,
                    c.is_entry_point,
                    c.is_test,
                    is_private,
                    scope_clear,
                    &self.coverage.read().unwrap(),
                    &c.kind,
                );
            if confirmed_caller_count == 0
                && risk == "low"
                && matches!(dead_code_confidence, "none" | "low")
            {
                risk = "medium".to_string();
            }
            let risk = Some(risk);

            let trend = calm_core::fitness::compute_trend(
                &conn,
                &c.qualified_name,
                EDIT_CONTEXT_TREND_LOOKBACK_DAYS,
            )
            .ok()
            .flatten()
            .map(TrendOutput::from);

            serde_json::to_string_pretty(&EditContextOutput {
                symbol: p.symbol,
                edges_ready: self.edges_ready(),
                index_freshness: self.phase_str(),
                callers,
                callees,
                blast_radius,
                range_checksum,
                risk_assessment: risk,
                dead_code_confidence: dead_code_confidence.to_string(),
                dead_code_source: dead_code_source.to_string(),
                trend,
                co_changed_files,
                suggested_next: self.filter_sn(suggested(
                    "diff_impact",
                    "MANDATORY after changes — verify blast radius",
                )),
            })
            .unwrap_or_default()
        })
    }

    #[tool(
        name = "diff_impact",
        description = "CALL THIS after every code change, BEFORE commit or push — never skip. USE WHEN: you have uncommitted changes and want to verify blast radius. NOT FOR: pre-edit analysis (use edit_context). vs edit_context: edit_context=pre-edit; diff_impact=post-edit. Omit all three for the unstaged working-tree diff, or provide at most one of: diff, staged=true, commits=<range>."
    )]
    pub(crate) fn diff_impact(&self, #[tool(aggr)] p: DiffImpactParams) -> String {
        self.timed_tool("diff_impact", || {
            self.clear_written_files();

            let input_count =
                p.diff.is_some() as u8 + p.staged.is_some() as u8 + p.commits.is_some() as u8;
            if input_count > 1 {
                return error_json(
                    "INVALID_INPUT",
                    "At most one of diff, staged, or commits may be provided (omit all three for the unstaged working-tree diff)",
                    false,
                );
            }

            const DIFF_GIT_TIMEOUT_SECS: u64 = 10;
            let diff_text = if let Some(d) = p.diff {
                d
            } else {
                let staged = p.staged.unwrap_or(false);
                let (diff, err) = calm_core::analysis::diff_impact::get_git_diff(
                    &self.project_root,
                    staged,
                    p.commits.as_deref(),
                    DIFF_GIT_TIMEOUT_SECS,
                );
                match diff {
                    Some(d) => d,
                    None => {
                        return error_json(
                            "FEATURE_UNAVAILABLE",
                            &err.unwrap_or_else(|| "git diff failed".into()),
                            true,
                        );
                    }
                }
            };

            let file_diffs = calm_core::analysis::diff_impact::parse_unified_diff(&diff_text);
            let files_changed: Vec<String> = file_diffs.iter().map(|f| f.path.clone()).collect();

            let mut unindexed_files: Vec<UnindexedFileOutput> = Vec::new();
            let mut affected: Vec<std::collections::HashMap<String, serde_json::Value>> =
                Vec::new();

            // READ-only: open a dedicated read connection (SINGLE_WRITER enforcement)
            {
                let conn = match self.make_read_conn() {
                    Ok(c) => c,
                    Err(e) => return format!(r#"{{"error": "db connection failed: {e}"}}"#),
                };
                for fd in &file_diffs {
                    // file_index has one row per file the indexer has ever
                    // scanned, independent of how many symbols it found — a
                    // file with 0 symbols (e.g. a Rust `mod.rs` that's just
                    // `pub mod` statements) is still fully indexed, just
                    // empty, and must not be reported as "unindexed" (the old
                    // `symbols`-only check couldn't tell the two apart). A row
                    // whose `language` is NULL is a third case: a
                    // recognized-but-unparseable extension (see
                    // `is_recognized_unparsed_extension`) the indexer tracks by
                    // path only — it can never have symbols no matter how long
                    // you wait, so it's still reported here, with its own
                    // reason rather than silently reading as a normal empty file.
                    let row_language: Option<Option<String>> = conn
                        .query_row(
                            "SELECT language FROM file_index WHERE path = ?1",
                            rusqlite::params![fd.path],
                            |r| r.get::<_, Option<String>>(0),
                        )
                        .ok();
                    let reason = match &row_language {
                        None => {
                            let path = std::path::Path::new(&fd.path);
                            let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
                            // A recognized extension only means "pending_scan" if the
                            // indexer would ever actually reach it — a .rs file under
                            // a dotdir or IGNORE_DIRS (e.g. .claude/, target/) never
                            // gets scanned no matter how long you wait, so it must be
                            // "out_of_scope" too, not just files of an unrecognized
                            // extension. See `calm_core::walk::path_has_ignored_dir_component`.
                            if !calm_core::walk::path_has_ignored_dir_component(path)
                                && (calm_core::indexer::lang_constants::language_for_extension(ext)
                                    .is_some()
                                    || calm_core::indexer::lang_constants::is_recognized_unparsed_extension(
                                        ext,
                                    ))
                            {
                                Some("pending_scan")
                            } else {
                                Some("out_of_scope")
                            }
                        }
                        Some(None) => Some("recognized_unparsed"),
                        Some(Some(_)) => None,
                    };
                    if let Some(reason) = reason {
                        unindexed_files.push(UnindexedFileOutput {
                            path: fd.path.clone(),
                            reason: reason.to_string(),
                        });
                        continue;
                    }

                    let mut stmt = conn
                        .prepare(
                            "SELECT qualified_name, name, kind, line_start, line_end, caller_count, signature
                             FROM symbols WHERE path = ?1",
                        )
                        .unwrap();
                    let rows: Vec<(String, String, String, i64, i64, i64, String)> = stmt
                        .query_map(rusqlite::params![fd.path], |row| {
                            Ok((
                                row.get(0)?,
                                row.get(1)?,
                                row.get(2)?,
                                row.get(3)?,
                                row.get(4)?,
                                row.get(5)?,
                                row.get(6)?,
                            ))
                        })
                        .unwrap()
                        .filter_map(|r| r.ok())
                        .collect();

                    for (qualified_name, name, kind, line_start, line_end, caller_count, signature) in
                        rows
                    {
                        let overlaps = fd
                            .hunks
                            .iter()
                            .any(|&(hs, he)| !(he < line_start || hs > line_end));
                        if !overlaps {
                            continue;
                        }

                        // The indexer's own signature extraction (parser.rs::walk_symbols)
                        // already scans to the real body-opening `{` (or `:` for Python) —
                        // its embedded newlines tell us exactly how many lines the real
                        // signature spans, instead of guessing with a fixed line cap (which
                        // silently missed changes past line 3 of a longer signature, e.g.
                        // calm_core::analysis::cochange::compute_co_changes's 7-line one).
                        // Clamped to line_end as a defensive bound, never exceeded in
                        // practice since a signature can't outlive its own symbol.
                        let sig_end =
                            (line_start + signature.matches('\n').count() as i64).min(line_end);
                        let is_new_symbol = calm_core::analysis::diff_impact::is_new_symbol(
                            (line_start, sig_end),
                            fd.is_new_file,
                            &fd.added_lines,
                            caller_count,
                        );
                        // A symbol that didn't exist before this diff cannot have had
                        // its signature "changed" — there is no prior signature to
                        // compare against, and (by definition) no prior call sites.
                        // `is_signature_changed` (line-overlap) is a cheap pre-filter;
                        // when it's true, `is_signature_semantically_changed` still has
                        // to agree the *text* actually differs (not just a parameter
                        // rename or reformat) before this escalates to high risk.
                        let signature_changed = !is_new_symbol
                            && calm_core::analysis::diff_impact::is_signature_changed(
                                (line_start, sig_end),
                                &fd.added_lines,
                            )
                            && {
                                let (old_text, new_text) =
                                    calm_core::analysis::diff_impact::signature_text_before_after(
                                        fd,
                                        (line_start, sig_end),
                                    );
                                calm_core::analysis::diff_impact::is_signature_semantically_changed(
                                    &old_text, &new_text,
                                )
                            };

                        let base_level = risk_level_from_caller_count(caller_count);
                        let mut reasons: Vec<String> = Vec::new();
                        let level = if is_new_symbol {
                            reasons.push(
                                "newly added symbol — no prior call sites to check; review its own correctness".to_string(),
                            );
                            base_level.to_string()
                        } else {
                            calm_core::analysis::diff_impact::escalate_risk_if_signature_changed(
                                signature_changed,
                                base_level,
                                &mut reasons,
                            )
                        };

                        let mut m: std::collections::HashMap<String, serde_json::Value> =
                            std::collections::HashMap::new();
                        m.insert("qualified_name".into(), serde_json::json!(qualified_name));
                        m.insert("name".into(), serde_json::json!(name));
                        m.insert("path".into(), serde_json::json!(fd.path));
                        m.insert("kind".into(), serde_json::json!(kind));
                        m.insert(
                            "signature_changed".into(),
                            serde_json::json!(signature_changed),
                        );
                        m.insert("symbol_is_new".into(), serde_json::json!(is_new_symbol));
                        m.insert(
                            "blast_radius".into(),
                            serde_json::json!({"direct_callers": caller_count}),
                        );
                        m.insert(
                            "risk_assessment".into(),
                            serde_json::json!({"level": level, "reasons": reasons}),
                        );
                        affected.push(m);
                    }
                }
            }

            let pending_scan_paths: Vec<String> = unindexed_files
                .iter()
                .filter(|f| f.reason == "pending_scan")
                .map(|f| f.path.clone())
                .collect();
            let aggregate_risk = calm_core::analysis::diff_impact::compute_aggregate_risk(
                &affected,
                &pending_scan_paths,
            );
            const MAX_AFFECTED_SYMBOLS: usize = 20;
            calm_core::analysis::diff_impact::sort_affected_symbols(
                &mut affected,
                MAX_AFFECTED_SYMBOLS,
            );

            let affected_symbols: Vec<AffectedSymbolOutput> = affected
                .into_iter()
                .filter_map(|m| {
                    serde_json::to_value(m)
                        .ok()
                        .and_then(|v| serde_json::from_value(v).ok())
                })
                .collect();

            let codeowner_patterns =
                calm_core::analysis::codeowners::load_codeowners(&self.project_root);
            let mut suggested_reviewers: Vec<String> = Vec::new();
            for f in &files_changed {
                for owner in calm_core::analysis::codeowners::find_owners(&codeowner_patterns, f) {
                    if !suggested_reviewers.contains(&owner) {
                        suggested_reviewers.push(owner);
                    }
                }
            }

            let sn = if !pending_scan_paths.is_empty() {
                suggested("indexing_status", "Wait for index before treating as safe")
            } else if aggregate_risk == "critical" || aggregate_risk == "high" {
                affected_symbols.first().map(|s| SuggestedNext {
                    tool: "callers".into(),
                    reason: "Verify high-risk callers manually".into(),
                    args: Some(serde_json::json!({"symbol": s.name})),
                })
            } else if aggregate_risk == "medium" {
                affected_symbols.first().map(|s| SuggestedNext {
                    tool: "callers".into(),
                    reason: "Medium-risk changes — spot-check key callers".into(),
                    args: Some(serde_json::json!({"symbol": s.name})),
                })
            } else if aggregate_risk == "unknown" {
                suggested("indexing_status", "Risk unknown — check index state")
            } else {
                None
            };

            serde_json::to_string_pretty(&DiffImpactOutput {
                files_changed,
                affected_symbols,
                unindexed_files,
                aggregate_risk,
                suggested_reviewers,
                note: None,
                suggested_next: self.filter_sn(sn),
            })
            .unwrap_or_default()
        })
    }
}

pub(crate) fn error_json(code: &str, message: &str, recoverable: bool) -> String {
    serde_json::to_string_pretty(&ErrorOutput {
        error: ErrorDetail {
            code: code.into(),
            message: message.into(),
            recoverable,
        },
    })
    .unwrap_or_default()
}

#[derive(Deserialize, JsonSchema)]
#[allow(dead_code)]
pub(crate) struct EditContextParams {
    /// Bare symbol name (not a `path::name` qualified name) — e.g. `load`,
    /// not `crates/calm-core/src/embedding.rs::Embedder::load`.
    pub(crate) symbol: String,
    /// Narrows the search to one file when `symbol` alone is ambiguous
    /// across the repo. Repo-relative, e.g. `crates/calm-core/src/embedding.rs`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) path: Option<String>,
    /// Disambiguates same-named symbols in the same file (e.g. a
    /// `#[cfg(feature)]` real impl vs. its stub) — any line within the
    /// intended candidate's range, as echoed in an earlier `ambiguous`
    /// response's `line_start`/`line_end`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) line: Option<i64>,
}

#[derive(Serialize, JsonSchema)]
pub(crate) struct BlastRadiusInfo {
    pub(crate) transitive: i64,
    pub(crate) files_affected: Vec<String>,
}

/// How much `caller_count`/`coreness`/`is_hub` moved since the oldest snapshot
/// still at least `EDIT_CONTEXT_TREND_LOOKBACK_DAYS` old — see
/// `calm_core::fitness::compute_trend`.

#[derive(Serialize, JsonSchema)]
pub(crate) struct TrendOutput {
    pub(crate) compared_to: String,
    pub(crate) caller_count_delta: i64,
    pub(crate) coreness_delta: i64,
    pub(crate) is_hub_changed: bool,
}

impl From<calm_core::fitness::TrendInfo> for TrendOutput {
    fn from(t: calm_core::fitness::TrendInfo) -> Self {
        Self {
            compared_to: t.compared_to,
            caller_count_delta: t.caller_count_delta,
            coreness_delta: t.coreness_delta,
            is_hub_changed: t.is_hub_changed,
        }
    }
}

/// A file with no import/call relationship to the symbol's file, but that
/// historically changed alongside it in the same commit — a coupling signal
/// the static graph cannot see. See `calm_core::analysis::cochange`.

#[derive(Serialize, JsonSchema)]
pub(crate) struct CoChangedFileOutput {
    pub(crate) path: String,
    pub(crate) co_change_count: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) last_co_changed: Option<String>,
}

impl From<calm_core::analysis::cochange::CoChangeEntry> for CoChangedFileOutput {
    fn from(e: calm_core::analysis::cochange::CoChangeEntry) -> Self {
        Self {
            path: e.path,
            co_change_count: e.co_change_count,
            last_co_changed: e.last_co_changed,
        }
    }
}

#[derive(Serialize, JsonSchema)]
pub(crate) struct EditContextOutput {
    pub(crate) symbol: String,
    pub(crate) edges_ready: bool,
    pub(crate) index_freshness: String,
    pub(crate) callers: Vec<CallerEntry>,
    pub(crate) callees: Vec<CalleeEntry>,
    pub(crate) blast_radius: BlastRadiusInfo,
    /// Hash of the symbol's current `[line_start, line_end]` — pass this
    /// straight to `edit_lines`/`edit_symbol` as `expected_hash` to skip
    /// the "learn the hash" preview round trip. Absent if the file
    /// couldn't be read from disk.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) range_checksum: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) risk_assessment: Option<String>,
    /// `"none"` (confirmed not dead — entry point, test, or has confirmed
    /// callers), `"high"`/`"medium"`/`"low"` confidence it genuinely is dead
    /// code — see `calm_core::analysis::dead_code::compute_dead_code_confidence`.
    /// Also feeds `risk_assessment`: a 0-caller symbol only keeps "low" risk
    /// when this independently agrees (`"high"`/`"medium"`).
    pub(crate) dead_code_confidence: String,
    /// `"static"` or `"static+coverage"` — whether a runtime coverage file
    /// (see `scripts/gen-coverage.sh`) was available to inform `dead_code_confidence`.
    pub(crate) dead_code_source: String,
    /// Absent when there's no snapshot yet at least `EDIT_CONTEXT_TREND_LOOKBACK_DAYS`
    /// old (e.g. `calm fitness-check` hasn't run for that long) — not an error.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) trend: Option<TrendOutput>,
    /// Empty when git is unavailable or no file co-changed with this
    /// symbol's file often enough to clear `config.cochange.min_co_changes`
    /// — not an error signal, most edits legitimately have none.
    pub(crate) co_changed_files: Vec<CoChangedFileOutput>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) suggested_next: Option<SuggestedNext>,
}

// ---------------------------------------------------------------------------
// Tool 11: session_context
// ---------------------------------------------------------------------------

#[derive(Deserialize, JsonSchema)]
pub(crate) struct DiffImpactParams {
    /// A raw unified diff (`git diff` output) to analyze directly, instead
    /// of having this tool run git itself. At most one of `diff`, `staged`,
    /// `commits` may be set — omitting all three analyzes the unstaged
    /// working-tree diff (plain `git diff`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) diff: Option<String>,
    /// `true` to analyze the staged diff (`git diff --cached`); `false` or
    /// omitted analyzes the unstaged working-tree diff. At most one of
    /// `diff`, `staged`, `commits` may be set.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) staged: Option<bool>,
    /// A commit range/rev understood by `git diff`, e.g. `HEAD~3..HEAD` or
    /// a single commit SHA. At most one of `diff`, `staged`, `commits`
    /// may be set.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) commits: Option<String>,
}

#[derive(Serialize, Deserialize, JsonSchema)]
pub(crate) struct BlastRadiusOutput {
    pub(crate) direct_callers: i64,
}

#[derive(Serialize, Deserialize, JsonSchema)]
pub(crate) struct RiskAssessmentOutput {
    pub(crate) level: String,
    pub(crate) reasons: Vec<String>,
}

#[derive(Serialize, Deserialize, JsonSchema)]
pub(crate) struct AffectedSymbolOutput {
    pub(crate) qualified_name: String,
    pub(crate) name: String,
    pub(crate) path: String,
    pub(crate) kind: String,
    pub(crate) signature_changed: bool,
    /// True when this symbol didn't exist before the diff (new file, or a
    /// pure-addition hunk covering its signature) — it has zero prior call
    /// sites by definition, so `signature_changed` is always false for it
    /// and risk is not escalated on "callers may need update" grounds.
    pub(crate) symbol_is_new: bool,
    pub(crate) blast_radius: BlastRadiusOutput,
    pub(crate) risk_assessment: RiskAssessmentOutput,
}

#[derive(Serialize, JsonSchema)]
pub(crate) struct UnindexedFileOutput {
    pub(crate) path: String,
    /// "pending_scan" — a recognized source file, in a path the indexer
    /// actually walks, that just hasn't been scanned yet; resolves itself
    /// once indexing catches up (check `indexing_status`).
    /// "out_of_scope" — will stay unindexed no matter how long you wait:
    /// either not a source extension the indexer parses at all (docs,
    /// config, etc.), or it sits under a dotdir/`IGNORE_DIRS` path (e.g.
    /// `.claude/`, `target/`) the walker categorically never descends into,
    /// regardless of extension.
    /// "recognized_unparsed" — a `file_index` row exists (the indexer has
    /// scanned it) but `language` is NULL: a recognized-but-unsupported
    /// extension (see `is_recognized_unparsed_extension`) tracked by path
    /// only. Like "out_of_scope", this never resolves on its own — there is
    /// no symbol extraction to wait for — but unlike "out_of_scope" the file
    /// genuinely is indexed (has a row), just never for symbols.
    pub(crate) reason: String,
}

#[derive(Serialize, JsonSchema)]
pub(crate) struct DiffImpactOutput {
    pub(crate) files_changed: Vec<String>,
    pub(crate) affected_symbols: Vec<AffectedSymbolOutput>,
    pub(crate) unindexed_files: Vec<UnindexedFileOutput>,
    pub(crate) aggregate_risk: String,
    pub(crate) suggested_reviewers: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) note: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) suggested_next: Option<SuggestedNext>,
}

// ---------------------------------------------------------------------------
// Tool 13: indexing_status
// ---------------------------------------------------------------------------
