use super::common::*;
use super::*;

impl CodeIntelligenceServer {
    #[tool(
        name = "remember",
        description = "Save a durable, interpretive note (architecture decision, gotcha, rationale) under a short topic key. Persists across sessions and server restarts — unlike session_context, which only tracks the current session's navigation. USE WHEN: you've learned something a future session should know that the graph/AST can't capture on its own (a WHY, not a fact derivable from code). Upserts: calling again with the same topic overwrites its content."
    )]
    pub(crate) fn remember(&self, #[tool(aggr)] p: RememberParams) -> String {
        self.timed_tool("remember", || {
            let topic = p.topic.trim();
            let content = p.content.trim();
            if topic.is_empty() || content.is_empty() {
                return r#"{"error": "topic and content must both be non-empty"}"#.to_string();
            }

            let conn = match self.memory_write_conn() {
                Ok(c) => c,
                Err(e) => return format!(r#"{{"error": "db connection failed: {e}"}}"#),
            };
            let now = utc_now_iso8601();
            let result = conn.execute(
                "INSERT INTO project_memory (topic, content, created_at, updated_at) \
                 VALUES (?1, ?2, ?3, ?3) \
                 ON CONFLICT(topic) DO UPDATE SET content = excluded.content, updated_at = excluded.updated_at",
                rusqlite::params![topic, content, now],
            );
            if let Err(e) = result {
                return format!(r#"{{"error": "write failed: {e}"}}"#);
            }

            // Best-effort: capture which real files this note references, so
            // a later `recall` can tell if they've changed since — a failure
            // here shouldn't fail the note itself, which is already saved.
            let ignore_patterns = calm_core::config::load_config(&self.project_root)
                .map(|c| c.ignore)
                .unwrap_or_default();
            let refs = calm_core::memory::capture_refs(&self.project_root, &ignore_patterns, content);
            let refs_captured = refs.len();
            if let Err(e) = calm_core::memory::store_refs(&conn, topic, &refs) {
                tracing::error!("remember: failed to store refs for topic {topic}: {e}");
            }

            serde_json::to_string_pretty(&RememberOutput {
                topic: topic.to_string(),
                updated_at: now,
                refs_captured,
                suggested_next: self.filter_sn(suggested("recall", "Verify the note was saved")),
            })
            .unwrap_or_default()
        })
    }

    #[tool(
        name = "recall",
        description = "Retrieve durable notes saved by remember. USE WHEN: starting work on a topic you might have left notes about, or checking for known gotchas before touching an area. Pass `topic` for one exact note, `query` for a keyword search across all notes, or neither to list everything (most recently updated first)."
    )]
    pub(crate) fn recall(&self, #[tool(aggr)] p: RecallParams) -> String {
        self.timed_tool("recall", || {
            const RECALL_LIMIT: i64 = 50;

            let conn = match self.make_read_conn() {
                Ok(c) => c,
                Err(e) => return format!(r#"{{"error": "db connection failed: {e}"}}"#),
            };

            let query_result = if let Some(topic) =
                p.topic.as_deref().map(str::trim).filter(|t| !t.is_empty())
            {
                conn.prepare(
                    "SELECT topic, content, updated_at FROM project_memory WHERE topic = ?1",
                )
                .and_then(|mut stmt| {
                    stmt.query_map(rusqlite::params![topic], memory_note_row)?
                        .collect::<Result<Vec<_>, _>>()
                })
            } else if let Some(q) = p.query.as_deref().map(str::trim).filter(|q| !q.is_empty()) {
                let fts_query = Self::escape_fts5_query(q);
                conn.prepare(
                    "SELECT p.topic, p.content, p.updated_at \
                     FROM project_memory_fts \
                     JOIN project_memory p ON p.id = project_memory_fts.rowid \
                     WHERE project_memory_fts MATCH ?1 \
                     ORDER BY bm25(project_memory_fts), p.updated_at DESC \
                     LIMIT ?2",
                )
                .and_then(|mut stmt| {
                    stmt.query_map(
                        rusqlite::params![fts_query, RECALL_LIMIT + 1],
                        memory_note_row,
                    )?
                    .collect::<Result<Vec<_>, _>>()
                })
            } else {
                conn.prepare(
                    "SELECT topic, content, updated_at FROM project_memory \
                     ORDER BY updated_at DESC LIMIT ?1",
                )
                .and_then(|mut stmt| {
                    stmt.query_map(rusqlite::params![RECALL_LIMIT + 1], memory_note_row)?
                        .collect::<Result<Vec<_>, _>>()
                })
            };

            let notes = match query_result {
                Ok(n) => n,
                Err(e) => return format!(r#"{{"error": "query failed: {e}"}}"#),
            };

            let truncated = notes.len() as i64 > RECALL_LIMIT;
            let mut notes: Vec<MemoryNote> =
                notes.into_iter().take(RECALL_LIMIT as usize).collect();

            // Per note: "unchecked" (no file-path refs were ever captured for
            // it — either the content had none, or none resolved at
            // `remember` time) stays the `memory_note_row` default; otherwise
            // diff captured refs against the live files.
            for note in &mut notes {
                let has_refs = calm_core::memory::ref_count(&conn, &note.topic).unwrap_or(0) > 0;
                if !has_refs {
                    continue;
                }
                let stale =
                    calm_core::memory::check_staleness(&conn, &self.project_root, &note.topic)
                        .unwrap_or_default();
                note.staleness = if stale.is_empty() {
                    "fresh"
                } else if stale.iter().any(|s| s.status == "deleted") {
                    "gone"
                } else {
                    "stale"
                };
                note.stale_refs = stale.into_iter().map(StaleRefOutput::from).collect();
            }

            let sn = if notes.is_empty() {
                suggested(
                    "remember",
                    "No notes found — save one if you learn something worth keeping",
                )
            } else {
                None
            };

            serde_json::to_string_pretty(&RecallOutput {
                notes,
                truncated,
                suggested_next: self.filter_sn(sn),
            })
            .unwrap_or_default()
        })
    }

    /// Wraps `query` as a single FTS5 phrase (embedded `"` doubled per FTS5
    /// syntax) so arbitrary user input — including FTS5 operators like `-`,
    /// `*`, `:` — is always treated as literal text to match, never parsed
    /// as query syntax. Mirrors `calm_core::search`'s identical helper for
    /// code search; duplicated rather than shared across crates for one
    /// small, dependency-free function.
    fn escape_fts5_query(query: &str) -> String {
        let mut escaped = String::with_capacity(query.len() + 2);
        escaped.push('"');
        for ch in query.chars() {
            if ch == '"' {
                escaped.push('"');
                escaped.push('"');
            } else {
                escaped.push(ch);
            }
        }
        escaped.push('"');
        escaped
    }
}

#[derive(Deserialize, JsonSchema)]
pub(crate) struct RememberParams {
    /// Short stable key for this note (e.g. "why-resolver-tiers",
    /// "auth-module-gotcha"). Calling `remember` again with the same topic
    /// overwrites its content — one note per topic, not an append log.
    pub(crate) topic: String,
    /// The note's full text — replaces whatever was previously stored
    /// under `topic`.
    pub(crate) content: String,
}

#[derive(Serialize, JsonSchema)]
pub(crate) struct RememberOutput {
    pub(crate) topic: String,
    pub(crate) updated_at: String,
    /// Count of file-path-like references found in `content` that resolved
    /// to a real file and got snapshotted for later staleness checks (see
    /// `MemoryNote::staleness`) — 0 just means the note didn't mention any
    /// file, not that anything went wrong.
    pub(crate) refs_captured: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) suggested_next: Option<SuggestedNext>,
}

// ---------------------------------------------------------------------------
// Tool 18: recall
// ---------------------------------------------------------------------------

#[derive(Deserialize, JsonSchema)]
pub(crate) struct RecallParams {
    /// Exact topic to fetch. Takes priority over `query` if both are set.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) topic: Option<String>,
    /// Keyword search across topic + content (SQL LIKE, case-insensitive
    /// for ASCII). Ignored if `topic` is set.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) query: Option<String>,
}

#[derive(Serialize, JsonSchema)]
pub(crate) struct StaleRefOutput {
    pub(crate) reference: String,
    /// "changed" (file still exists but its content hash no longer matches
    /// what `remember` captured) or "deleted" (file no longer exists).
    pub(crate) status: String,
}

impl From<calm_core::memory::StaleRef> for StaleRefOutput {
    fn from(s: calm_core::memory::StaleRef) -> Self {
        Self {
            reference: s.reference,
            status: s.status.to_string(),
        }
    }
}

#[derive(Serialize, JsonSchema)]
pub(crate) struct MemoryNote {
    pub(crate) topic: String,
    pub(crate) content: String,
    pub(crate) updated_at: String,
    /// "unchecked" (no file refs were ever captured for this note — nothing
    /// to compare), "fresh" (all captured refs still match), "stale" (a
    /// referenced file changed since this note was written), or "gone" (a
    /// referenced file was deleted) — set by `recall`, not `memory_note_row`.
    pub(crate) staleness: &'static str,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub(crate) stale_refs: Vec<StaleRefOutput>,
}

#[derive(Serialize, JsonSchema)]
pub(crate) struct RecallOutput {
    pub(crate) notes: Vec<MemoryNote>,
    pub(crate) truncated: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) suggested_next: Option<SuggestedNext>,
}

pub(crate) fn memory_note_row(row: &rusqlite::Row) -> rusqlite::Result<MemoryNote> {
    Ok(MemoryNote {
        topic: row.get(0)?,
        content: row.get(1)?,
        updated_at: row.get(2)?,
        staleness: "unchecked",
        stale_refs: Vec::new(),
    })
}

// ---------------------------------------------------------------------------
// Tool router
// ---------------------------------------------------------------------------
