use super::common::*;
use super::guardrails::*;
use super::*;

impl CodeIntelligenceServer {
    #[tool(
        name = "edit_lines",
        description = "The only write-capable tool in ci — line-range granularity, works on ANY tracked file (source code, Cargo.toml, docs — not just parsed symbols). NOT FOR: symbol-scoped edits with auto-resolved range (use edit_symbol). Requires expected_hash from a prior call's current_hash (or edit_context's range_checksum for a whole symbol); omit it to preview a range's hash/content without writing anything. All hunks in one call apply to the same file and must be disjoint (non-overlapping)."
    )]
    pub(crate) fn edit_lines(&self, #[tool(aggr)] p: EditLinesParams) -> String {
        self.timed_tool("edit_lines", || {
            let hunks: Vec<calm_core::edit::HunkRequest> = p
                .edits
                .into_iter()
                .map(|h| calm_core::edit::HunkRequest {
                    start_line: h.start_line.max(0) as usize,
                    end_line: h.end_line.max(0) as usize,
                    expected_hash: h.expected_hash,
                    new_text: h.new_text,
                })
                .collect();
            self.edit_lines_impl(&p.path, hunks, p.confirm)
        })
    }

    #[tool(
        name = "edit_symbol",
        description = "Sugar over edit_lines: resolves symbol (+ optional path/line, same disambiguation contract as edit_context) to its [line_start, line_end] and replaces the whole thing in one hunk. USE WHEN: replacing an entire function/class/method body by name. NOT FOR: editing a single line inside a symbol, or anything outside a parsed symbol (an import line, Cargo.toml) — use edit_lines directly for those."
    )]
    pub(crate) fn edit_symbol(&self, #[tool(aggr)] p: EditSymbolParams) -> String {
        self.timed_tool("edit_symbol", || {
            let c = {
                let conn = match self.make_read_conn() {
                    Ok(c) => c,
                    Err(e) => return format!(r#"{{"error": "db connection failed: {e}"}}"#),
                };
                match resolve_symbol(&conn, &p.symbol, p.path.as_deref(), p.line) {
                    SymbolResolution::NotFound => return not_found_json(&p.symbol),
                    SymbolResolution::Ambiguous(candidates) => return ambiguous_json(&candidates),
                    SymbolResolution::Found(c) => *c,
                }
            };
            let hunk = calm_core::edit::HunkRequest {
                start_line: c.line_start as usize,
                end_line: c.line_end as usize,
                expected_hash: p.expected_hash,
                new_text: p.new_text,
            };
            self.edit_lines_impl(&c.path, vec![hunk], p.confirm)
        })
    }

    /// Shared implementation for `edit_lines`/`edit_symbol`. Flow: apply
    /// hunks in-memory (all-or-nothing, see `calm_core::edit::apply_hunks`) →
    /// pre-write syntax validation → risk gate (query-only, against
    /// pre-edit symbol ranges) → atomic write → reindex (same
    /// `reindex_changed` + `embed_pending*` gate the file watcher uses, so
    /// the DB is never observably staler than a watcher-driven update) →
    /// post-edit symbol lookup for the response.
    fn edit_lines_impl(
        &self,
        path: &str,
        hunks: Vec<calm_core::edit::HunkRequest>,
        confirm: bool,
    ) -> String {
        let full_path = self.project_root.join(path);
        let original = match std::fs::read_to_string(&full_path) {
            Ok(s) => s,
            Err(e) => {
                return error_json("READ_FAILED", &format!("could not read {path}: {e}"), false);
            }
        };

        let outcome = match calm_core::edit::apply_hunks(&original, &hunks) {
            Ok(o) => o,
            Err(e) => return error_json("INVALID_HUNKS", &e.to_string(), false),
        };

        let hunks_output: Vec<EditHunkResultOutput> = outcome
            .results
            .iter()
            .map(EditHunkResultOutput::from)
            .collect();

        if !outcome.all_applied {
            return serde_json::to_string_pretty(&EditLinesOutput {
                path: path.to_string(),
                applied: false,
                hunks: hunks_output,
                parse_status: None,
                touched_symbols: vec![],
                risk_assessment: None,
                note: Some(
                    "nothing written — some hunk was a preview or had a stale hash; \
                     retry with the current_hash shown for each hunk"
                        .into(),
                ),
                suggested_next: None,
            })
            .unwrap_or_default();
        }
        let new_content = outcome.new_content.expect("all_applied implies Some");

        let ext = std::path::Path::new(path)
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("");
        let parse_status = match calm_core::edit::validate_syntax(&new_content, ext) {
            Some(true) => "clean",
            Some(false) => {
                return error_json(
                    "PARSE_ERROR",
                    &format!(
                        "this edit would introduce a syntax error in {path} — nothing written"
                    ),
                    true,
                );
            }
            None => "skipped_unrecognized_language",
        };

        let (risk, hub_hit, _pre_edit_touched) = {
            let conn = match self.make_read_conn() {
                Ok(c) => c,
                Err(e) => return format!(r#"{{"error": "db connection failed: {e}"}}"#),
            };
            let ranges: Vec<(i64, i64)> = hunks
                .iter()
                .map(|h| (h.start_line as i64, h.end_line as i64))
                .collect();
            compute_touch_risk(&conn, path, &ranges)
        };
        if !confirm && (risk.as_deref() == Some("high") || hub_hit) {
            let why = if hub_hit {
                "a hub symbol (is_hub=true)".to_string()
            } else {
                "a high-risk symbol (>10 callers)".to_string()
            };
            return error_json(
                "CONFIRM_REQUIRED",
                &format!("this edit touches {why} — pass confirm:true to proceed"),
                true,
            );
        }

        // Serialize the write+reindex sequence — rmcp dispatches tool calls
        // concurrently, and this is the one section that touches both the
        // filesystem and the DB's single-writer slot.
        let _guard = self.edit_lock.lock().unwrap();

        if let Err(e) = calm_core::edit::atomic_write(&full_path, &new_content) {
            drop(_guard);
            return error_json(
                "WRITE_FAILED",
                &format!("failed to write {path}: {e}"),
                false,
            );
        }

        let mut write_conn = match calm_core::db::conn::open_writer(&self.db_path) {
            Ok(c) => c,
            Err(e) => {
                drop(_guard);
                return error_json(
                    "DB_ERROR",
                    &format!(
                        "wrote {path} but could not open DB to reindex: {e} — call indexing_status"
                    ),
                    true,
                );
            }
        };
        match calm_core::indexer::pipeline::reindex_changed(&mut write_conn, &self.project_root) {
            Ok(summary) if !summary.is_noop() => {
                if let Some(model) = self.embedder() {
                    if let Err(e) = calm_core::embedding::embed_pending(&write_conn, model.as_ref())
                    {
                        tracing::error!("edit_lines: incremental embedding failed: {e}");
                    }
                    if let Err(e) =
                        calm_core::embedding::embed_pending_chunks(&write_conn, model.as_ref())
                    {
                        tracing::error!("edit_lines: incremental chunk embedding failed: {e}");
                    }
                }
            }
            Ok(_) => {}
            Err(e) => {
                drop(write_conn);
                drop(_guard);
                return error_json(
                    "REINDEX_FAILED",
                    &format!(
                        "wrote {path} but reindex failed: {e} — index may be stale, call indexing_status"
                    ),
                    true,
                );
            }
        }
        drop(write_conn);
        drop(_guard);

        self.track_file(path);
        self.mark_written(path);

        let touched_symbols = {
            let conn = match self.make_read_conn() {
                Ok(c) => c,
                Err(_) => {
                    return serde_json::to_string_pretty(&EditLinesOutput {
                        path: path.to_string(),
                        applied: true,
                        hunks: hunks_output,
                        parse_status: Some(parse_status.to_string()),
                        touched_symbols: vec![],
                        risk_assessment: risk,
                        note: Some("edit applied but could not re-query touched symbols".into()),
                        suggested_next: None,
                    })
                    .unwrap_or_default();
                }
            };
            let new_ranges: Vec<(i64, i64)> = outcome
                .results
                .iter()
                .map(|r| (r.start_line as i64, r.new_end_line as i64))
                .collect();
            let (_, _, touched) = compute_touch_risk(&conn, path, &new_ranges);
            touched
        };

        serde_json::to_string_pretty(&EditLinesOutput {
            path: path.to_string(),
            applied: true,
            hunks: hunks_output,
            parse_status: Some(parse_status.to_string()),
            touched_symbols,
            risk_assessment: risk,
            note: None,
            suggested_next: self.filter_sn(suggested(
                "diff_impact",
                "Verify wider blast radius, especially if this touched a hub/high-risk symbol",
            )),
        })
        .unwrap_or_default()
    }
}

/// Symbols in `path` whose `[line_start, line_end]` overlaps any of `ranges`
/// — shared by the pre-write risk gate (against original ranges) and the
/// post-write response (against the edited ranges' new positions).
fn symbols_overlapping_ranges(
    conn: &rusqlite::Connection,
    path: &str,
    ranges: &[(i64, i64)],
) -> Vec<(String, i64, bool)> {
    let mut stmt = match conn.prepare(
        "SELECT qualified_name, caller_count, is_hub, line_start, line_end FROM symbols WHERE path = ?1",
    ) {
        Ok(s) => s,
        Err(_) => return vec![],
    };
    stmt.query_map(rusqlite::params![path], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, i64>(1)?,
            row.get::<_, i64>(2)? != 0,
            row.get::<_, i64>(3)?,
            row.get::<_, i64>(4)?,
        ))
    })
    .map(|it| {
        it.filter_map(|r| r.ok())
            .filter(|(_, _, _, line_start, line_end)| {
                ranges
                    .iter()
                    .any(|&(rs, re)| !(*line_end < rs || *line_start > re))
            })
            .map(|(qn, callers, is_hub, _, _)| (qn, callers, is_hub))
            .collect()
    })
    .unwrap_or_default()
}

/// `(risk_level, hub_hit, touched_symbols)` for whatever symbols in `path`
/// overlap `ranges`. `risk_level` is `None` when nothing overlaps (editing
/// dead space between symbols, or a file with no parsed symbols at all —
/// Cargo.toml, docs) — that's not an error, just nothing to gate on.
fn compute_touch_risk(
    conn: &rusqlite::Connection,
    path: &str,
    ranges: &[(i64, i64)],
) -> (Option<String>, bool, Vec<TouchedSymbolOutput>) {
    let rows = symbols_overlapping_ranges(conn, path, ranges);
    let mut max_callers = 0i64;
    let mut hub_hit = false;
    let mut touched = Vec::with_capacity(rows.len());
    for (qualified_name, caller_count, is_hub) in rows {
        max_callers = max_callers.max(caller_count);
        hub_hit |= is_hub;
        touched.push(TouchedSymbolOutput {
            qualified_name,
            caller_count,
            is_hub,
        });
    }
    let risk = (!touched.is_empty()).then(|| risk_level_from_caller_count(max_callers).to_string());
    (risk, hub_hit, touched)
}

// ---------------------------------------------------------------------------
// Params / Output
// ---------------------------------------------------------------------------

#[derive(Deserialize, JsonSchema)]
pub(crate) struct EditHunkParam {
    /// 1-indexed, inclusive.
    pub(crate) start_line: i64,
    /// 1-indexed, inclusive.
    pub(crate) end_line: i64,
    /// Hash of this range's current content — from a prior call's
    /// `current_hash`, or `edit_context`'s `range_checksum` when the range
    /// is exactly a whole symbol. Omit to preview instead of writing: the
    /// response still reports `current_hash`/`old_text` for this range, so
    /// a first call can learn the hash before a real edit.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) expected_hash: Option<String>,
    /// Replacement text for the range, used exactly as given (no implicit
    /// newline handling) — include your own `\n` between lines and after
    /// the last one if the following line should stay on its own line.
    pub(crate) new_text: String,
}

#[derive(Deserialize, JsonSchema)]
pub(crate) struct EditLinesParams {
    /// Repo-relative path. All hunks in one call apply to this one file.
    pub(crate) path: String,
    /// Must be disjoint (non-overlapping) ranges; applied bottom-up so
    /// earlier (lower-numbered) hunks are never affected by line-count
    /// changes from later (higher-numbered) ones.
    pub(crate) edits: Vec<EditHunkParam>,
    /// Required `true` to write when any touched range falls inside a
    /// `risk_assessment: "high"` symbol or one with `is_hub: true` (see
    /// `edit_context`). Omitted/`false` rejects such an edit with an
    /// explanation instead of proceeding.
    #[serde(default)]
    pub(crate) confirm: bool,
}

#[derive(Deserialize, JsonSchema)]
pub(crate) struct EditSymbolParams {
    /// Bare symbol name (not a `path::name` qualified name).
    pub(crate) symbol: String,
    /// Narrows the search to one file when `symbol` alone is ambiguous.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) path: Option<String>,
    /// Disambiguates same-named symbols in the same file — any line within
    /// the intended candidate's range, as echoed in an earlier `ambiguous`
    /// response's `line_start`/`line_end`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) line: Option<i64>,
    /// Same contract as `edit_lines`' hunk `expected_hash` — omit to
    /// preview the symbol's current hash/content instead of writing.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) expected_hash: Option<String>,
    /// Full replacement text for the symbol's `[line_start, line_end]`.
    pub(crate) new_text: String,
    #[serde(default)]
    pub(crate) confirm: bool,
}

#[derive(Serialize, JsonSchema)]
pub(crate) struct EditHunkResultOutput {
    pub(crate) start_line: i64,
    pub(crate) end_line: i64,
    /// "applied" | "preview" | "conflict"
    pub(crate) status: String,
    /// Hash of the range's content before this call — pass this as
    /// `expected_hash` on retry.
    pub(crate) current_hash: String,
    /// Content of the range before this call — undo material when
    /// `status == "applied"`, or what to inspect otherwise.
    pub(crate) old_text: String,
    /// Only present when `status == "applied"`: the line the replacement
    /// now ends at (`start_line` is unchanged).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) new_end_line: Option<i64>,
}

impl From<&calm_core::edit::HunkResult> for EditHunkResultOutput {
    fn from(r: &calm_core::edit::HunkResult) -> Self {
        let applied = r.status == calm_core::edit::HunkStatus::Applied;
        Self {
            start_line: r.start_line as i64,
            end_line: r.end_line as i64,
            status: match r.status {
                calm_core::edit::HunkStatus::Applied => "applied",
                calm_core::edit::HunkStatus::Preview => "preview",
                calm_core::edit::HunkStatus::Conflict => "conflict",
            }
            .to_string(),
            current_hash: r.current_hash.clone(),
            old_text: r.old_text.clone(),
            new_end_line: applied.then_some(r.new_end_line as i64),
        }
    }
}

#[derive(Serialize, JsonSchema)]
pub(crate) struct TouchedSymbolOutput {
    pub(crate) qualified_name: String,
    pub(crate) caller_count: i64,
    pub(crate) is_hub: bool,
}

#[derive(Serialize, JsonSchema)]
pub(crate) struct EditLinesOutput {
    pub(crate) path: String,
    pub(crate) applied: bool,
    pub(crate) hunks: Vec<EditHunkResultOutput>,
    /// "clean" | "skipped_unrecognized_language" — absent when nothing was
    /// written (preview/conflict/risk-blocked/parse error).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) parse_status: Option<String>,
    /// Symbols overlapping the touched ranges (post-edit positions once
    /// `applied`) — the same callers/is_hub signal `edit_context`/
    /// `diff_impact` report, bundled here so a caller doesn't need a
    /// separate round trip just to see what it just changed.
    pub(crate) touched_symbols: Vec<TouchedSymbolOutput>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) risk_assessment: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) note: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) suggested_next: Option<SuggestedNext>,
}
