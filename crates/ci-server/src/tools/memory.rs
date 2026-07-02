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

            serde_json::to_string_pretty(&RememberOutput {
                topic: topic.to_string(),
                updated_at: now,
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
                let pattern = format!("%{q}%");
                conn.prepare(
                    "SELECT topic, content, updated_at FROM project_memory \
                     WHERE topic LIKE ?1 OR content LIKE ?1 ORDER BY updated_at DESC LIMIT ?2",
                )
                .and_then(|mut stmt| {
                    stmt.query_map(
                        rusqlite::params![pattern, RECALL_LIMIT + 1],
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
            let notes: Vec<MemoryNote> = notes.into_iter().take(RECALL_LIMIT as usize).collect();

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
}

#[derive(Deserialize, JsonSchema)]
pub(crate) struct RememberParams {
    /// Short stable key for this note (e.g. "why-resolver-tiers",
    /// "auth-module-gotcha"). Calling `remember` again with the same topic
    /// overwrites its content — one note per topic, not an append log.
    pub(crate) topic: String,
    pub(crate) content: String,
}

#[derive(Serialize, JsonSchema)]
pub(crate) struct RememberOutput {
    pub(crate) topic: String,
    pub(crate) updated_at: String,
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
pub(crate) struct MemoryNote {
    pub(crate) topic: String,
    pub(crate) content: String,
    pub(crate) updated_at: String,
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
    })
}

// ---------------------------------------------------------------------------
// Tool router
// ---------------------------------------------------------------------------
