use super::common::*;
use super::*;

/// `(symbol, path, edge_confidence, edge_kind, line, formal_source)` --
/// D2 (2026-07-30 stack-graphs-demotion-lever) pushed this past clippy's
/// type_complexity threshold as a bare tuple; named here purely to satisfy
/// that lint, same row shape serves both `callers`' and `callees`' queries.
type EdgeRow = (String, String, String, String, Option<i64>, Option<String>);

#[rmcp::tool_router(router = "trace_tool_router", vis = "pub(crate)")]
impl CalmServer {
    #[tool(
        name = "callers",
        description = "USE WHEN: you need to know who calls a specific symbol — blast radius scan, refactoring impact. USE THIS for SYMBOL-LEVEL call sites. NOT for file-level imports (use dependencies). vs edit_context: callers is for exploration; edit_context is the mandatory pre-edit tool.",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    pub(crate) fn callers(
        &self,
        Parameters(p): Parameters<CallersParams>,
    ) -> Json<ResolvedOutcome<CallersOutput>> {
        Json(self.timed_tool("callers", || {
            // READ-only: open a dedicated read connection (SINGLE_WRITER enforcement)
            let conn = match self.make_read_conn() {
                Ok(c) => c,
                Err(e) => return db_error_resolved(e),
            };
            let resolution = match resolve_symbol(&conn, &p.symbol, p.path.as_deref(), p.line) {
                Ok(r) => r,
                Err(e) => return db_error_resolved(e),
            };
            let c = match resolution {
                SymbolResolution::NotFound => return ResolvedOutcome::not_found(&p.symbol),
                SymbolResolution::Ambiguous(candidates) => {
                    return ResolvedOutcome::ambiguous(&candidates);
                }
                SymbolResolution::Found(c) => *c,
            };
            self.track_symbol(&c.qualified_name);
            self.track_file(&c.path);

            let config = self.config();

            let all: Vec<CallerEntry> = {
                let mut stmt = match conn.prepare(
                    "SELECT ce.from_symbol, ce.from_path, ce.edge_confidence, ce.call_site_line, ce.edge_kind, ce.formal_source
                     FROM call_edges ce
                     LEFT JOIN symbols s ON s.qualified_name = ce.from_symbol
                     WHERE ce.to_symbol = ?1 AND ce.ruled_out_by_scip = 0
                     ORDER BY COALESCE(s.is_test, 0) ASC, ce.from_path, ce.call_site_line",
                ) {
                    Ok(s) => s,
                    Err(e) => return db_error_resolved(e),
                };
                let rows: Vec<EdgeRow> =
                    match stmt.query_map(rusqlite::params![c.qualified_name], |row| {
                        Ok((
                            row.get::<_, String>(0)?,
                            row.get::<_, String>(1).unwrap_or_default(),
                            row.get::<_, String>(2)?,
                            row.get::<_, String>(4)?,
                            row.get::<_, Option<i64>>(3)?,
                            row.get::<_, Option<String>>(5)?,
                        ))
                    }) {
                        Ok(iter) => iter.filter_map(|r| r.ok()).collect(),
                        Err(e) => return db_error_resolved(e),
                    };
                let preview_items: Vec<(String, Option<i64>)> = rows
                    .iter()
                    .map(|(_, path, _, _, line, _)| (path.clone(), *line))
                    .collect();
                let previews = line_previews_batched(&self.project_root, &preview_items);
                rows.into_iter()
                    .zip(previews)
                    .map(
                        |(
                            (symbol, _path, edge_confidence, edge_kind, line, formal_source),
                            preview,
                        )| CallerEntry {
                            symbol,
                            edge_confidence,
                            formal_source,
                            edge_kind,
                            line,
                            preview,
                        },
                    )
                    .collect()
            };

            let (transitive, transitive_count, transitive_capped) = if p.transitive {
                let max_depth = p
                    .max_depth
                    .map(|d| (d.max(1) as usize).min(config.callers.max_depth_cap))
                    .unwrap_or(config.callers.max_depth_cap);
                let (entries, capped) = transitive_bfs(
                    &conn,
                    &c.qualified_name,
                    EdgeDirection::Callers,
                    max_depth,
                    config.callers.transitive_timeout_ms,
                );
                let count = entries.len();
                (Some(entries), Some(count), Some(capped))
            } else {
                (None, None, None)
            };

            // Split `ambiguous`-confidence edges out of `direct`. These are
            // index-time fan-out: when a method call's receiver type can't be
            // resolved (`x.as_str()` with `x` of unknown type), the indexer
            // emits an edge to EVERY same-named symbol, each marked `ambiguous`.
            // They are not confirmed callers of this specific symbol, so
            // surfacing them as `direct` collapses precision — bucket them
            // separately for the caller to weigh.
            let (ambiguous, direct): (Vec<CallerEntry>, Vec<CallerEntry>) = all
                .into_iter()
                .partition(|e| e.edge_confidence == "ambiguous");
            let count = direct.len();
            let ambiguous_count = ambiguous.len();

            // Fingerprint of the (direct, ambiguous) answer — lets a caller
            // re-checking this same symbol after an unrelated edit elsewhere
            // skip re-paying the full token cost when nothing changed here.
            let etag = Some(hash_caller_entries(direct.iter().chain(ambiguous.iter())));
            if p.if_none_match.is_some() && p.if_none_match == etag {
                return ResolvedOutcome::success(CallersOutput {
                    symbol: p.symbol,
                    edges_ready: self.edges_ready(),
                    direct: Vec::new(),
                    direct_count: count,
                    ambiguous: Vec::new(),
                    ambiguous_count,
                    direct_truncated: None,
                    ambiguous_truncated: None,
                    transitive,
                    transitive_count,
                    transitive_capped,
                    etag,
                    not_modified: Some(true),
                    suggested_next: None,
                });
            }

            let has_textual = direct.iter().any(|e| e.edge_confidence == "textual");
            let sn = if has_textual || count > 10 {
                suggested(
                    "edit_context",
                    "High blast radius or uncertain edges — verify before modifying",
                )
            } else if count > 0 {
                suggested_with_args(
                    "source",
                    "Read top caller implementation",
                    serde_json::json!({"target": direct[0].symbol}),
                )
            } else if ambiguous_count > 0 {
                suggested(
                    "edit_context",
                    "Only ambiguous (unresolved-receiver) call sites — verify before trusting",
                )
            } else {
                None
            };
            // Zero direct AND zero ambiguous callers is the exact "0 usages"
            // trap a pre-edit safety check can be fooled by — attach an
            // advisory caveat rather than let an empty list read as proof.
            // `is_entry_point` symbols (rmcp #[tool] handlers, main, trait-
            // dispatch protocol methods, decorator-registered handlers, ...)
            // get a distinct caveat: for them, zero is the expected,
            // permanent shape, not a "maybe dead code" signal.
            let no_usage_caveat = (count == 0 && ambiguous_count == 0).then(|| {
                if c.is_entry_point {
                    Caveat::entry_point_dispatch(&p.symbol)
                } else {
                    Caveat::no_direct_usage(&p.symbol)
                }
            });

            // Cap the raw per-entry dump AFTER everything above (etag, sn,
            // caveat) has already looked at the full sets — direct_count/
            // ambiguous_count stay the true totals no matter what gets
            // truncated here.
            let cap = config.callers.direct_list_cap;
            let direct_truncated = (direct.len() > cap).then_some(true);
            let ambiguous_truncated = (ambiguous.len() > cap).then_some(true);
            let mut direct = direct;
            let mut ambiguous = ambiguous;
            direct.truncate(cap);
            ambiguous.truncate(cap);

            let out = ResolvedOutcome::success(CallersOutput {
                symbol: p.symbol,
                edges_ready: self.edges_ready(),
                direct,
                direct_count: count,
                ambiguous,
                ambiguous_count,
                direct_truncated,
                ambiguous_truncated,
                transitive,
                transitive_count,
                transitive_capped,
                etag,
                not_modified: None,
                suggested_next: self.filter_sn(sn),
            });
            match no_usage_caveat {
                Some(cv) => out.with_caveat(cv),
                None => out,
            }
        }))
    }
    #[tool(
        name = "callees",
        description = "USE WHEN: you need to trace what a symbol calls — understanding logic flow, internal deps. NOT for finding who calls this symbol (use callers). vs callers: callers=upward (who calls X); callees=downward (what X calls).",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    pub(crate) fn callees(
        &self,
        Parameters(p): Parameters<CalleesParams>,
    ) -> Json<ResolvedOutcome<CalleesOutput>> {
        Json(self.timed_tool("callees", || {
            // READ-only: open a dedicated read connection (SINGLE_WRITER enforcement)
            let conn = match self.make_read_conn() {
                Ok(c) => c,
                Err(e) => return db_error_resolved(e),
            };
            let resolution = match resolve_symbol(&conn, &p.symbol, p.path.as_deref(), p.line) {
                Ok(r) => r,
                Err(e) => return db_error_resolved(e),
            };
            let c = match resolution {
                SymbolResolution::NotFound => return ResolvedOutcome::not_found(&p.symbol),
                SymbolResolution::Ambiguous(candidates) => {
                    return ResolvedOutcome::ambiguous(&candidates);
                }
                SymbolResolution::Found(c) => *c,
            };
            self.track_symbol(&c.qualified_name);
            self.track_file(&c.path);

            let config = self.config();

            let all: Vec<CalleeEntry> = {
                let mut stmt = match conn.prepare(
                    "SELECT ce.to_symbol, ce.to_path, ce.edge_confidence, ce.call_site_line, ce.edge_kind, ce.formal_source
                     FROM call_edges ce
                     LEFT JOIN symbols s ON s.qualified_name = ce.to_symbol
                     WHERE ce.from_symbol = ?1 AND ce.ruled_out_by_scip = 0
                     ORDER BY COALESCE(s.is_test, 0) ASC, ce.to_path, ce.call_site_line",
                ) {
                    Ok(s) => s,
                    Err(e) => return db_error_resolved(e),
                };
                let rows: Vec<EdgeRow> =
                    match stmt.query_map(rusqlite::params![c.qualified_name], |row| {
                        Ok((
                            row.get::<_, String>(0)?,
                            row.get::<_, String>(1).unwrap_or_default(),
                            row.get::<_, String>(2)?,
                            row.get::<_, String>(4)?,
                            row.get::<_, Option<i64>>(3)?,
                            row.get::<_, Option<String>>(5)?,
                        ))
                    }) {
                        Ok(iter) => iter.filter_map(|r| r.ok()).collect(),
                        Err(e) => return db_error_resolved(e),
                    };
                // The call site lives in the symbol being inspected
                // (`c.path`), not in the callee's own file (`to_path`) --
                // every row's preview key is this same constant path, so
                // line_previews_batched reads it exactly once no matter how
                // many callees there are (audit F11).
                let from_path = c.path.clone();
                let preview_items: Vec<(String, Option<i64>)> = rows
                    .iter()
                    .map(|(_, _, _, _, line, _)| (from_path.clone(), *line))
                    .collect();
                let previews = line_previews_batched(&self.project_root, &preview_items);
                rows.into_iter()
                    .zip(previews)
                    .map(
                        |(
                            (symbol, path, edge_confidence, edge_kind, line, formal_source),
                            preview,
                        )| CalleeEntry {
                            symbol,
                            path,
                            edge_confidence,
                            formal_source,
                            edge_kind,
                            line,
                            preview,
                        },
                    )
                    .collect()
            };

            let (transitive, transitive_count, transitive_capped) = if p.transitive {
                let max_depth = p
                    .max_depth
                    .map(|d| (d.max(1) as usize).min(config.callees.max_depth_cap))
                    .unwrap_or(config.callees.max_depth_cap);
                let (entries, capped) = transitive_bfs(
                    &conn,
                    &c.qualified_name,
                    EdgeDirection::Callees,
                    max_depth,
                    config.callees.transitive_timeout_ms,
                );
                let count = entries.len();
                (Some(entries), Some(count), Some(capped))
            } else {
                (None, None, None)
            };

            let (ambiguous, direct): (Vec<CalleeEntry>, Vec<CalleeEntry>) = all
                .into_iter()
                .partition(|e| e.edge_confidence == "ambiguous");
            let count = direct.len();
            let ambiguous_count = ambiguous.len();

            // Fingerprint of the (direct, ambiguous) answer — same
            // conditional-fetch pattern as `callers`/`source`.
            let etag = Some(hash_callee_entries(direct.iter().chain(ambiguous.iter())));
            if p.if_none_match.is_some() && p.if_none_match == etag {
                return ResolvedOutcome::success(CalleesOutput {
                    symbol: p.symbol,
                    edges_ready: self.edges_ready(),
                    direct: Vec::new(),
                    direct_count: count,
                    ambiguous: Vec::new(),
                    ambiguous_count,
                    direct_truncated: None,
                    ambiguous_truncated: None,
                    transitive,
                    transitive_count,
                    transitive_capped,
                    etag,
                    not_modified: Some(true),
                    suggested_next: None,
                });
            }

            let sn = if count > 0 {
                suggested("path", "Trace specific call chain")
            } else {
                None
            };

            // Cap the raw per-entry dump AFTER etag/sn have already looked
            // at the full sets — direct_count/ambiguous_count stay the true
            // totals no matter what gets truncated here. Same
            // `config.callees.direct_list_cap` as `callers`.
            let cap = config.callees.direct_list_cap;
            let direct_truncated = (direct.len() > cap).then_some(true);
            let ambiguous_truncated = (ambiguous.len() > cap).then_some(true);
            let mut direct = direct;
            let mut ambiguous = ambiguous;
            direct.truncate(cap);
            ambiguous.truncate(cap);

            ResolvedOutcome::success(CalleesOutput {
                symbol: p.symbol,
                edges_ready: self.edges_ready(),
                direct,
                direct_count: count,
                ambiguous,
                ambiguous_count,
                direct_truncated,
                ambiguous_truncated,
                transitive,
                transitive_count,
                transitive_capped,
                etag,
                not_modified: None,
                suggested_next: self.filter_sn(sn),
            })
        }))
    }
    #[tool(
        name = "dependencies",
        description = "USE WHEN: you need to understand file-level architectural connections. USE THIS for FILE-LEVEL import graph. NOT for symbol-level call sites (use callers/callees). vs callers/callees: dependencies is file-level; callers/callees is symbol-level.",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    pub(crate) fn dependencies(
        &self,
        Parameters(p): Parameters<DependenciesParams>,
    ) -> Json<ToolOutcome<DependenciesOutput>> {
        Json(self.timed_tool("dependencies", || {
            self.track_file(&p.path);
            // READ-only: open a dedicated read connection (SINGLE_WRITER enforcement)
            let conn = match self.make_read_conn() {
                Ok(c) => c,
                Err(e) => return db_error(e),
            };
            let dep_config = self.config().dependencies;

            let mut stmt_imports = match conn.prepare(
                "SELECT from_path, COALESCE(to_path, ''), module_name, symbols_used
                 FROM import_edges WHERE from_path = ?1 LIMIT ?2",
            ) {
                Ok(s) => s,
                Err(e) => return db_error(e),
            };

            let imports: Vec<ImportEntry> = match stmt_imports.query_map(
                rusqlite::params![p.path, dep_config.max_imports as i64 + 1],
                |row| {
                    Ok(ImportEntry {
                        from_path: row.get(0)?,
                        to_path: row.get(1)?,
                        module_name: row.get(2)?,
                        symbols_used: parse_symbols_used(&row.get::<_, String>(3)?),
                    })
                },
            ) {
                Ok(iter) => iter.filter_map(|r| r.ok()).collect(),
                Err(e) => return db_error(e),
            };
            let imports_truncated = imports.len() > dep_config.max_imports;
            let imports = imports.into_iter().take(dep_config.max_imports).collect();

            // Drop the first statement before preparing the second on the same conn
            drop(stmt_imports);
            let mut stmt_imported_by = match conn.prepare(
                "SELECT from_path, COALESCE(to_path, ''), module_name, symbols_used
                 FROM import_edges WHERE to_path = ?1 LIMIT ?2",
            ) {
                Ok(s) => s,
                Err(e) => return db_error(e),
            };

            let imported_by: Vec<ImportEntry> = match stmt_imported_by.query_map(
                rusqlite::params![p.path, dep_config.max_imported_by as i64 + 1],
                |row| {
                    Ok(ImportEntry {
                        from_path: row.get(0)?,
                        to_path: row.get(1)?,
                        module_name: row.get(2)?,
                        symbols_used: parse_symbols_used(&row.get::<_, String>(3)?),
                    })
                },
            ) {
                Ok(iter) => iter.filter_map(|r| r.ok()).collect(),
                Err(e) => return db_error(e),
            };
            let imported_by_truncated = imported_by.len() > dep_config.max_imported_by;
            let imported_by = imported_by
                .into_iter()
                .take(dep_config.max_imported_by)
                .collect::<Vec<_>>();

            // Call-graph dependents: files with call sites that resolve INTO
            // this file but never appear in `imported_by` — e.g. a fully-
            // qualified `crate::foo::Bar::baz()` call with no matching `use`,
            // which the import graph cannot see. Best-effort and coarser than
            // imports (may include ambiguous-receiver calls), so it complements
            // the import graph rather than replacing it.
            let call_dependents: Vec<String> = {
                let already: std::collections::HashSet<&str> =
                    imported_by.iter().map(|e| e.from_path.as_str()).collect();
                // PATTERN-DEBT call-edges-missing-ruled-out-filter: a
                // SCIP-disproven caller isn't a real dependent.
                let stmt = conn.prepare(
                    "SELECT DISTINCT from_path FROM call_edges \
                     WHERE to_path = ?1 AND from_path IS NOT NULL AND from_path != ?1 \
                     AND ruled_out_by_scip = 0 \
                     ORDER BY from_path LIMIT ?2",
                );
                let mut stmt = match stmt {
                    Ok(s) => s,
                    Err(e) => return db_error(e),
                };
                let mapped = stmt.query_map(
                    rusqlite::params![p.path, dep_config.max_imported_by as i64],
                    |row| row.get::<_, String>(0),
                );
                match mapped {
                    Ok(iter) => iter
                        .filter_map(|r| r.ok())
                        .filter(|f| !already.contains(f.as_str()))
                        .collect(),
                    Err(e) => return db_error(e),
                }
            };

            // Glob re-export dependents: files that reach this file's symbols
            // through exactly one `use other::*;` hop — e.g. `common.rs` has
            // `use super::*;` and never names `Embedder` itself, but `super`
            // (`tools.rs`) has its own `use calm_core::embedding::Embedder;`, so
            // `common.rs` genuinely depends on this file even though no
            // direct `import_edges` row names it. A glob import row is
            // identified by `symbols_used = '[]'` (a glob names no specific
            // item, unlike every other `use` form `parse_rust_import`
            // produces) joined to a resolved, non-glob import from the same
            // target back into this file. One hop only — a chain of two or
            // more globs (`use a::*` re-exporting `use b::*`) is not walked;
            // that's a real gap but a much rarer pattern than the one-hop
            // case this closes, and unbounded chain-following risks the same
            // combinatorial blowup depth caps elsewhere in this file exist to
            // avoid.
            let glob_reexport_dependents: Vec<String> = {
                let mut already: std::collections::HashSet<String> =
                    imported_by.iter().map(|e| e.from_path.clone()).collect();
                already.extend(call_dependents.iter().cloned());
                let stmt = conn.prepare(
                    "SELECT DISTINCT g.from_path FROM import_edges g \
                     JOIN import_edges d ON d.from_path = g.to_path \
                     WHERE g.symbols_used = '[]' \
                       AND g.to_path IS NOT NULL AND g.to_path != '' \
                       AND g.from_path != ?1 \
                       AND d.to_path = ?1 \
                       AND d.symbols_used != '[]' \
                     ORDER BY g.from_path LIMIT ?2",
                );
                let mut stmt = match stmt {
                    Ok(s) => s,
                    Err(e) => return db_error(e),
                };
                let mapped = stmt.query_map(
                    rusqlite::params![p.path, dep_config.max_imported_by as i64],
                    |row| row.get::<_, String>(0),
                );
                match mapped {
                    Ok(iter) => iter
                        .filter_map(|r| r.ok())
                        .filter(|f| !already.contains(f))
                        .collect(),
                    Err(e) => return db_error(e),
                }
            };

            let sn = if imported_by.len() > 20 {
                suggested("callers", "High fan-in — check symbol blast radius")
            } else {
                None
            };
            ToolOutcome::success(DependenciesOutput {
                path: p.path,
                imports,
                imports_truncated,
                imported_by,
                imported_by_truncated,
                call_dependents,
                glob_reexport_dependents,
                suggested_next: self.filter_sn(sn),
            })
        }))
    }

    #[tool(
        name = "reference_impact",
        description = "USE WHEN: planning a rename/removal and need the FULL reference surface, not just the call graph -- imports, re-exports, and textual matches too. Merges call edges (callers), import edges naming this symbol, and a repo-wide textual grep into one classified list (must_change/likely_change/review/textual_only). Broader but coarser than callers/dependencies alone -- composes them instead of replacing them; still not a substitute for a real compiler/language-server rename.",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    pub(crate) fn reference_impact(
        &self,
        Parameters(p): Parameters<ReferenceImpactParams>,
    ) -> Json<ResolvedOutcome<ReferenceImpactOutput>> {
        Json(self.timed_tool("reference_impact", || {
            // READ-only: open a dedicated read connection (SINGLE_WRITER enforcement)
            let conn = match self.make_read_conn() {
                Ok(c) => c,
                Err(e) => return db_error_resolved(e),
            };
            let resolution = match resolve_symbol(&conn, &p.symbol, p.path.as_deref(), p.line) {
                Ok(r) => r,
                Err(e) => return db_error_resolved(e),
            };
            let c = match resolution {
                SymbolResolution::NotFound => return ResolvedOutcome::not_found(&p.symbol),
                SymbolResolution::Ambiguous(candidates) => {
                    return ResolvedOutcome::ambiguous(&candidates);
                }
                SymbolResolution::Found(c) => *c,
            };
            self.track_symbol(&c.qualified_name);
            self.track_file(&c.path);

            let mut hits: Vec<ReferenceHit> = Vec::new();
            // Dedup key: (path, line) for a line-precise hit; a file-level
            // hit (an import edge, which carries no line number) keys on
            // (path, None).
            let mut seen: std::collections::HashSet<(String, Option<i64>)> =
                std::collections::HashSet::new();
            // Files with a file-level import-edge hit (no line number) --
            // used below to suppress a textual grep hit that's really just
            // re-finding that same import statement. Deliberately NOT
            // built from `seen` as a whole (which also holds line-precise
            // call-edge hits): a file can have a real call edge at one
            // line AND an unrelated textual reference at another line --
            // the latter must still surface, not get suppressed just
            // because the file already appears elsewhere in `seen`.
            let mut import_hit_files: std::collections::HashSet<String> =
                std::collections::HashSet::new();

            // 1. Call edges -- same query shape `callers` uses, but
            // classified by edge_confidence instead of split into
            // direct/ambiguous.
            let call_rows: Vec<(String, Option<i64>, String)> = {
                let mut stmt = match conn.prepare(
                    "SELECT ce.from_path, ce.call_site_line, ce.edge_confidence \
                     FROM call_edges ce \
                     WHERE ce.to_symbol = ?1 AND ce.ruled_out_by_scip = 0",
                ) {
                    Ok(s) => s,
                    Err(e) => return db_error_resolved(e),
                };
                match stmt.query_map(rusqlite::params![c.qualified_name], |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, Option<i64>>(1)?,
                        row.get::<_, String>(2)?,
                    ))
                }) {
                    Ok(iter) => iter.filter_map(|r| r.ok()).collect(),
                    Err(e) => return db_error_resolved(e),
                }
            };
            let preview_items: Vec<(String, Option<i64>)> = call_rows
                .iter()
                .map(|(path, line, _)| (path.clone(), *line))
                .collect();
            let previews = line_previews_batched(&self.project_root, &preview_items);
            for ((path, line, confidence), preview) in call_rows.into_iter().zip(previews) {
                if !seen.insert((path.clone(), line)) {
                    continue;
                }
                let classification = match confidence.as_str() {
                    "resolved" | "formal" => "must_change",
                    "inferred" | "textual" => "likely_change",
                    _ => "review",
                };
                hits.push(ReferenceHit {
                    path,
                    line,
                    classification: classification.to_string(),
                    source: "call_edge".to_string(),
                    confidence: Some(confidence),
                    snippet: preview,
                });
            }

            // 2. Import edges directly naming this symbol -- catches a bare
            // re-export/import that never becomes a call edge. This is the
            // exact gap behind two real benchmarks/b7_task_correctness
            // misses (rename_express_set_charset, rename_zod_prettify_error
            // -- both miss a bare re-export statement; see
            // KNOWN_LIMITATIONS.md "No unified reference-impact tool").
            {
                let mut stmt = match conn
                    .prepare("SELECT from_path, symbols_used FROM import_edges WHERE to_path = ?1")
                {
                    Ok(s) => s,
                    Err(e) => return db_error_resolved(e),
                };
                let rows: Vec<(String, String)> = match stmt
                    .query_map(rusqlite::params![c.path], |row| {
                        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
                    }) {
                    Ok(iter) => iter.filter_map(|r| r.ok()).collect(),
                    Err(e) => return db_error_resolved(e),
                };
                for (from_path, symbols_used_raw) in rows {
                    if from_path == c.path {
                        continue;
                    }
                    let names = parse_symbols_used(&symbols_used_raw);
                    if !names.iter().any(|n| n == &c.name) {
                        continue;
                    }
                    if !seen.insert((from_path.clone(), None)) {
                        continue;
                    }
                    import_hit_files.insert(from_path.clone());
                    hits.push(ReferenceHit {
                        path: from_path,
                        line: None,
                        classification: "must_change".to_string(),
                        source: "import".to_string(),
                        confidence: None,
                        snippet: None,
                    });
                }
            }

            // 3. Textual grep floor -- a word-boundary match on the bare
            // name anywhere in the repo, for whatever the two structured
            // signals above miss entirely (string/config references, a
            // language the indexer doesn't resolve calls for, ...).
            // Deliberately the widest and least precise signal:
            // `textual_only` may include false positives from an unrelated
            // same-named identifier -- that imprecision is why it's its own
            // bottom tier, not merged into `review`.
            let ignore_patterns = self.config().ignore;
            let pattern = format!(r"\b{}\b", regex::escape(&c.name));
            match calm_core::search::search_grep(
                &conn,
                &self.project_root,
                &pattern,
                None,
                false,
                0,
                &ignore_patterns,
                REFERENCE_IMPACT_LIMIT,
            ) {
                Ok(grep) => {
                    for r in grep.results {
                        if r.path == c.path
                            && r.line_start
                                .is_some_and(|l| l >= c.line_start && l <= c.line_end)
                        {
                            continue; // the definition site itself
                        }
                        if import_hit_files.contains(&r.path) {
                            continue; // already flagged (file-level) via an import edge
                        }
                        if !seen.insert((r.path.clone(), r.line_start)) {
                            continue;
                        }
                        hits.push(ReferenceHit {
                            path: r.path,
                            line: r.line_start,
                            classification: "textual_only".to_string(),
                            source: "textual".to_string(),
                            confidence: None,
                            snippet: r.snippet,
                        });
                    }
                }
                Err(e) => {
                    tracing::warn!("reference_impact: textual grep failed: {e}");
                }
            }

            hits.sort_by(|a, b| (a.path.as_str(), a.line).cmp(&(b.path.as_str(), b.line)));

            // Counts must reflect the FULL match set, not just what
            // survives truncation below -- computed here, before
            // truncating, so a caller relying on e.g. must_change_count to
            // gauge blast radius never sees a number that silently shrinks
            // just because hits.len() crossed REFERENCE_IMPACT_LIMIT.
            let must_change_count = hits
                .iter()
                .filter(|h| h.classification == "must_change")
                .count();
            let likely_change_count = hits
                .iter()
                .filter(|h| h.classification == "likely_change")
                .count();
            let review_count = hits.iter().filter(|h| h.classification == "review").count();
            let textual_only_count = hits
                .iter()
                .filter(|h| h.classification == "textual_only")
                .count();

            let truncated = hits.len() > REFERENCE_IMPACT_LIMIT;
            hits.truncate(REFERENCE_IMPACT_LIMIT);

            let sn = if review_count > 0 || textual_only_count > 0 {
                suggested(
                    "edit_context",
                    "Some references need manual review before a mechanical rename",
                )
            } else {
                None
            };

            ResolvedOutcome::success(ReferenceImpactOutput {
                symbol: p.symbol,
                qualified_name: c.qualified_name,
                definition_path: c.path,
                definition_line_start: c.line_start,
                definition_line_end: c.line_end,
                references: hits,
                must_change_count,
                likely_change_count,
                review_count,
                textual_only_count,
                truncated: truncated.then_some(true),
                suggested_next: self.filter_sn(sn),
            })
        }))
    }
    #[tool(
        name = "path",
        description = "USE WHEN: you need to trace if and how symbol A can reach symbol B through call chain. Bidirectional BFS — cycles terminate cleanly. path is DIRECTED: A→B ≠ B→A. terminated_by=null + exists=true/false → certain result.",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    pub(crate) fn path(
        &self,
        Parameters(p): Parameters<PathParams>,
    ) -> Json<ResolvedOutcome<PathOutput>> {
        Json(self.timed_tool("path", || {
            // READ-only: open a dedicated read connection (SINGLE_WRITER enforcement)
            let conn = match self.make_read_conn() {
                Ok(c) => c,
                Err(e) => return db_error_resolved(e),
            };
            let from = match resolve_symbol(&conn, &p.from_symbol, p.from_path.as_deref(), p.from_line) {
                Ok(r) => r,
                Err(e) => return db_error_resolved(e),
            };
            let from = match from {
                SymbolResolution::NotFound => return ResolvedOutcome::not_found(&p.from_symbol),
                SymbolResolution::Ambiguous(candidates) => return ResolvedOutcome::ambiguous(&candidates),
                SymbolResolution::Found(c) => *c,
            };
            self.track_symbol(&from.qualified_name);
            self.track_file(&from.path);

            let to = match resolve_symbol(&conn, &p.to_symbol, p.to_path.as_deref(), p.to_line) {
                Ok(r) => r,
                Err(e) => return db_error_resolved(e),
            };
            let to = match to {
                SymbolResolution::NotFound => return ResolvedOutcome::not_found(&p.to_symbol),
                SymbolResolution::Ambiguous(candidates) => return ResolvedOutcome::ambiguous(&candidates),
                SymbolResolution::Found(c) => *c,
            };
            self.track_symbol(&to.qualified_name);
            self.track_file(&to.path);

            let path_config = self.config().path;

            let requested_hops = p.max_hops.unwrap_or(path_config.default_max_hops as i64);
            let hops_clamped = !(0..=path_config.max_allowed_hops as i64).contains(&requested_hops);
            let max_hops = requested_hops.clamp(0, path_config.max_allowed_hops as i64) as usize;

            let result = {
                calm_core::graph::path::bidirectional_bfs_path(
                    &conn,
                    &from.qualified_name,
                    &to.qualified_name,
                    max_hops,
                    5,
                    path_config.timeout_ms,
                )
            };

            let (routes, exists, terminated_by) = match result {
                Ok(r) => (
                    r.routes
                        .into_iter()
                        .map(|path| path.into_iter().map(|step| step.symbol).collect())
                        .collect::<Vec<Vec<String>>>(),
                    r.exists,
                    r.terminated_by.map(TerminatedByOutput::from),
                ),
                Err(_) => (vec![], None, None),
            };

            let count = routes.len();
            let sn = if matches!(&terminated_by, Some(TerminatedByOutput::Timeout)) {
                suggested_with_args("path", "Retry with smaller max_hops", serde_json::json!({"max_hops": 4}))
            } else if matches!(&terminated_by, Some(TerminatedByOutput::MaxHops)) {
                let new_hops = requested_hops + 4;
                suggested_with_args("path", "Path may exceed hop limit — retry with larger max_hops, or check the reverse direction",
                    serde_json::json!({"max_hops": new_hops, "from_symbol": p.to_symbol, "to_symbol": p.from_symbol}))
            } else if exists == Some(true) {
                suggested("source", "Read meeting node implementation")
            } else {
                None
            };
            ResolvedOutcome::success(PathOutput {
                from_symbol: p.from_symbol,
                to_symbol: p.to_symbol,
                routes,
                route_count: count,
                exists,
                terminated_by,
                hops_clamped,
                suggested_next: self.filter_sn(sn),
            })
        }))
    }
}

#[derive(Deserialize, JsonSchema)]
#[allow(dead_code)]
pub(crate) struct CallersParams {
    /// Bare symbol name (not a `path::name` qualified name).
    pub(crate) symbol: String,
    /// Narrows the search to one file when `symbol` alone is ambiguous
    /// across the repo. Repo-relative path.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) path: Option<String>,
    /// Disambiguates same-named symbols in the same file — any line within
    /// the intended candidate's range (see an earlier `ambiguous` response's
    /// `line_start`/`line_end`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) line: Option<i64>,
    /// `true` to also do a multi-hop BFS (`transitive`/`transitive_count`
    /// in the output) beyond direct callers. `false` (default) returns
    /// only direct callers — cheaper.
    #[serde(default)]
    pub(crate) transitive: bool,
    /// Max BFS depth when `transitive` is set. Clamped to
    /// `callers.max_depth_cap` in config.json (4 out of the box); ignored
    /// if `transitive` is `false`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) max_depth: Option<i64>,
    /// `etag` from a prior `callers` call on this exact `symbol`/`path`/
    /// `line` (with the same `transitive`/`max_depth`) — if the caller set
    /// hasn't changed since, the response omits `direct`/`ambiguous` and
    /// sets `not_modified: true` instead of re-sending them.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) if_none_match: Option<String>,
}

#[derive(Serialize, JsonSchema)]
pub(crate) struct CallersOutput {
    pub(crate) symbol: String,
    pub(crate) edges_ready: bool,
    pub(crate) direct: Vec<CallerEntry>,
    /// `ambiguous`-confidence callers split out of `direct`: index-time
    /// fan-out edges (a method call whose receiver type couldn't be resolved
    /// fans out to every same-named symbol), so each may or may not actually
    /// call this symbol. Excluded from `direct`/`direct_count` — weigh these
    /// explicitly rather than trusting them as confirmed callers.
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub(crate) ambiguous: Vec<CallerEntry>,
    pub(crate) ambiguous_count: usize,
    pub(crate) direct_count: usize,
    /// `true` when `direct` was cut down to `config.callers.direct_list_cap`
    /// entries — `direct_count` above is still the true total regardless.
    /// A real hub symbol can have 50-200+ direct callers; without this cap
    /// a single `callers` call on one could cost several thousand tokens
    /// dumping near-duplicate entries (e.g. dozens of unit-test call sites
    /// in one file) with the one production caller buried at the end.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) direct_truncated: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) ambiguous_truncated: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) transitive: Option<Vec<TransitiveEntry>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) transitive_count: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) transitive_capped: Option<bool>,
    /// Fingerprint of this response's `direct`+`ambiguous` (see
    /// `hash_caller_entries`) — pass back as `if_none_match` on a later
    /// `callers` call for the same symbol to skip re-sending them if
    /// nothing changed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) etag: Option<String>,
    /// `true` when `if_none_match` matched this response's `etag` —
    /// `direct`/`ambiguous` are empty in this case, not actually zero
    /// callers; re-call without `if_none_match` for the real lists.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) not_modified: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) suggested_next: Option<SuggestedNext>,
}

// ---------------------------------------------------------------------------
// Tool 7: callees
// ---------------------------------------------------------------------------

#[derive(Deserialize, JsonSchema)]
#[allow(dead_code)]
pub(crate) struct CalleesParams {
    /// Bare symbol name (not a `path::name` qualified name).
    pub(crate) symbol: String,
    /// Narrows the search to one file when `symbol` alone is ambiguous
    /// across the repo. Repo-relative path.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) path: Option<String>,
    /// Disambiguates same-named symbols in the same file — any line within
    /// the intended candidate's range (see an earlier `ambiguous` response's
    /// `line_start`/`line_end`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) line: Option<i64>,
    /// `true` to also do a multi-hop BFS (`transitive`/`transitive_count`
    /// in the output) beyond direct callees. `false` (default) returns
    /// only direct callees — cheaper.
    #[serde(default)]
    pub(crate) transitive: bool,
    /// Max BFS depth when `transitive` is set. Clamped to
    /// `callees.max_depth_cap` in config.json (4 out of the box); ignored
    /// if `transitive` is `false`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) max_depth: Option<i64>,
    /// `etag` from a prior `callees` call on this exact `symbol`/`path`/
    /// `line` — if the callee set hasn't changed since, the response omits
    /// `direct`/`ambiguous` and sets `not_modified: true` instead.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) if_none_match: Option<String>,
}

#[derive(Serialize, JsonSchema)]
pub(crate) struct CalleesOutput {
    pub(crate) symbol: String,
    pub(crate) edges_ready: bool,
    pub(crate) direct: Vec<CalleeEntry>,
    /// `ambiguous`-confidence callees split out of `direct` — see
    /// `CallersOutput::ambiguous`. Excluded from `direct`/`direct_count`.
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub(crate) ambiguous: Vec<CalleeEntry>,
    pub(crate) ambiguous_count: usize,
    pub(crate) direct_count: usize,
    /// See `CallersOutput::direct_truncated` — same
    /// `config.callees.direct_list_cap`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) direct_truncated: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) ambiguous_truncated: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) transitive: Option<Vec<TransitiveEntry>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) transitive_count: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) transitive_capped: Option<bool>,
    /// See `CallersOutput::etag`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) etag: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) not_modified: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) suggested_next: Option<SuggestedNext>,
}

// ---------------------------------------------------------------------------
// Tool: reference_impact
// ---------------------------------------------------------------------------

/// Cap on the merged `references` list (and the textual-grep sub-search
/// feeding it) -- same rationale as `callers.direct_list_cap`: a real hub
/// symbol can have far more usages than are useful to dump in one response.
const REFERENCE_IMPACT_LIMIT: usize = 200;

#[derive(Deserialize, JsonSchema)]
#[allow(dead_code)]
pub(crate) struct ReferenceImpactParams {
    /// Bare symbol name (not a `path::name` qualified name).
    pub(crate) symbol: String,
    /// Narrows the search to one file when `symbol` alone is ambiguous
    /// across the repo. Repo-relative path.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) path: Option<String>,
    /// Disambiguates same-named symbols in the same file -- any line within
    /// the intended candidate's range (see an earlier `ambiguous` response's
    /// `line_start`/`line_end`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) line: Option<i64>,
}

#[derive(Serialize, JsonSchema, Clone)]
pub(crate) struct ReferenceHit {
    pub(crate) path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) line: Option<i64>,
    /// `"must_change"` (a confirmed call site, or an import statement
    /// directly naming this symbol) / `"likely_change"` (a heuristically-
    /// resolved call site) / `"review"` (an ambiguous-confidence call site
    /// -- needs a human look) / `"textual_only"` (a bare-name grep match
    /// not otherwise explained -- could be a real reference this repo's
    /// parser doesn't model, e.g. a string/config reference, or could be an
    /// unrelated identifier that merely shares this name).
    pub(crate) classification: String,
    /// `"call_edge"` / `"import"` / `"textual"` -- which signal produced
    /// this hit.
    pub(crate) source: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) confidence: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) snippet: Option<String>,
}

#[derive(Serialize, JsonSchema)]
pub(crate) struct ReferenceImpactOutput {
    pub(crate) symbol: String,
    pub(crate) qualified_name: String,
    pub(crate) definition_path: String,
    pub(crate) definition_line_start: i64,
    pub(crate) definition_line_end: i64,
    pub(crate) references: Vec<ReferenceHit>,
    pub(crate) must_change_count: usize,
    pub(crate) likely_change_count: usize,
    pub(crate) review_count: usize,
    pub(crate) textual_only_count: usize,
    /// `true` when `references` was cut down to `REFERENCE_IMPACT_LIMIT`
    /// entries.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) truncated: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) suggested_next: Option<SuggestedNext>,
}

#[derive(Deserialize, JsonSchema)]
#[allow(dead_code)]
pub(crate) struct DependenciesParams {
    /// Repo-relative path of the file whose imports/importers to list, e.g.
    /// `crates/calm-core/src/embedding.rs`.
    pub(crate) path: String,
}

#[derive(Serialize, JsonSchema)]
pub(crate) struct ImportEntry {
    pub(crate) from_path: String,
    pub(crate) to_path: String,
    pub(crate) module_name: String,
    pub(crate) symbols_used: Vec<String>,
}

pub(crate) fn parse_symbols_used(raw: &str) -> Vec<String> {
    serde_json::from_str(raw).unwrap_or_default()
}

#[derive(Serialize, JsonSchema)]
pub(crate) struct DependenciesOutput {
    pub(crate) path: String,
    pub(crate) imports: Vec<ImportEntry>,
    pub(crate) imports_truncated: bool,
    pub(crate) imported_by: Vec<ImportEntry>,
    pub(crate) imported_by_truncated: bool,
    /// Files that reference this file via call edges but are absent from
    /// `imported_by` — e.g. a fully-qualified `crate::foo::Bar::baz()` call
    /// with no `use`. Best-effort (can include ambiguous-receiver calls);
    /// complements the import graph, does not replace it.
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub(crate) call_dependents: Vec<String>,
    /// Files that reach this file's symbols through exactly one `use other::*;`
    /// hop — see the doc comment where this is computed in `dependencies`.
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub(crate) glob_reexport_dependents: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) suggested_next: Option<SuggestedNext>,
}

// ---------------------------------------------------------------------------
// Tool 9: path
// ---------------------------------------------------------------------------

#[derive(Deserialize, JsonSchema)]
#[allow(dead_code)]
pub(crate) struct PathParams {
    /// Bare name of the starting symbol (not a `path::name` qualified name).
    pub(crate) from_symbol: String,
    /// Bare name of the target symbol to reach.
    pub(crate) to_symbol: String,
    /// Narrows `from_symbol` to one file when the bare name is ambiguous
    /// across the repo. Repo-relative path.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) from_path: Option<String>,
    /// Same as `from_path`, for `to_symbol`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) to_path: Option<String>,
    /// Disambiguates a same-named `from_symbol` in the same file — any line
    /// within the intended candidate's range.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) from_line: Option<i64>,
    /// Same as `from_line`, for `to_symbol`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) to_line: Option<i64>,
    /// Max BFS depth to search before giving up. Defaults to
    /// `path.default_max_hops` in config.json (8 out of the box), clamped
    /// to `path.max_allowed_hops` (20 out of the box).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) max_hops: Option<i64>,
}

/// Local mirror of `calm_core::types::TerminatedBy` — that type lives in
/// `calm-core`, which doesn't depend on `schemars`, so it can't derive
/// `JsonSchema` itself.

#[derive(Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub(crate) enum TerminatedByOutput {
    Timeout,
    MaxHops,
    PathCount,
}

impl From<calm_core::types::TerminatedBy> for TerminatedByOutput {
    fn from(t: calm_core::types::TerminatedBy) -> Self {
        match t {
            calm_core::types::TerminatedBy::Timeout => TerminatedByOutput::Timeout,
            calm_core::types::TerminatedBy::MaxHops => TerminatedByOutput::MaxHops,
            calm_core::types::TerminatedBy::PathCount => TerminatedByOutput::PathCount,
        }
    }
}

#[derive(Serialize, JsonSchema)]
pub(crate) struct PathOutput {
    pub(crate) from_symbol: String,
    pub(crate) to_symbol: String,
    pub(crate) routes: Vec<Vec<String>>,
    pub(crate) route_count: usize,
    pub(crate) exists: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) terminated_by: Option<TerminatedByOutput>,
    pub(crate) hops_clamped: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) suggested_next: Option<SuggestedNext>,
}
