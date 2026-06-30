//! Opt-in semantic embeddings (Cargo feature `embeddings`).
//!
//! Pure-Rust static code embeddings via `model2vec-rs` (default
//! `minishlab/potion-code-16M`, 256-dim), stored and searched in `sqlite-vec`.
//! The feature is off by default so the musl static binary stays lean; this
//! module exposes the *same* surface in both builds — when the feature is off,
//! every entry point is a no-op and semantic search degrades to FTS.

use rusqlite::Connection;

/// True when the crate was built with the `embeddings` feature.
pub const ENABLED: bool = cfg!(feature = "embeddings");

/// The text embedded for a symbol: name + signature + docstring.
pub fn symbol_doc(name: &str, signature: &str, docstring: &str) -> String {
    let mut s = String::with_capacity(name.len() + signature.len() + docstring.len() + 2);
    s.push_str(name);
    if !signature.is_empty() {
        s.push(' ');
        s.push_str(signature);
    }
    if !docstring.is_empty() {
        s.push(' ');
        s.push_str(docstring);
    }
    s
}

// ---------------------------------------------------------------------------
// Feature ON: real model2vec-rs + sqlite-vec implementation.
// ---------------------------------------------------------------------------
#[cfg(feature = "embeddings")]
mod imp {
    use super::*;
    use model2vec_rs::model::StaticModel;

    /// Register the sqlite-vec extension for every subsequent connection. Must be
    /// called once, before opening any connection that uses the `vec0` table.
    pub fn register_extension() {
        unsafe {
            #[allow(clippy::missing_transmute_annotations)]
            rusqlite::ffi::sqlite3_auto_extension(Some(std::mem::transmute(
                sqlite_vec::sqlite3_vec_init as *const (),
            )));
        }
    }

    /// Create the KNN table for `dim`-dimensional cosine vectors (idempotent).
    pub fn create_embedding_table(conn: &Connection, dim: usize) -> rusqlite::Result<()> {
        conn.execute_batch(&format!(
            "CREATE VIRTUAL TABLE IF NOT EXISTS embedding_vecs USING vec0(
                symbol_id INTEGER PRIMARY KEY,
                embedding FLOAT[{dim}] distance_metric=cosine
            );"
        ))
    }

    /// A loaded static embedding model.
    pub struct Embedder {
        model: StaticModel,
        dim: usize,
    }

    impl Embedder {
        /// Load `model_id` (a HuggingFace repo id or local path). Output is
        /// L2-normalised so cosine distance behaves well.
        pub fn load(model_id: &str, dim: usize) -> anyhow::Result<Self> {
            let model = StaticModel::from_pretrained(model_id, None, Some(true), None)
                .map_err(|e| anyhow::anyhow!("load embedding model '{model_id}': {e}"))?;
            Ok(Self { model, dim })
        }

        pub fn dim(&self) -> usize {
            self.dim
        }

        pub fn embed_one(&self, text: &str) -> Vec<f32> {
            self.model.encode_single(text)
        }

        pub fn embed_batch(&self, texts: &[String]) -> Vec<Vec<f32>> {
            self.model.encode(texts)
        }
    }

    fn vec_to_blob(v: &[f32]) -> Vec<u8> {
        let mut b = Vec::with_capacity(v.len() * 4);
        for f in v {
            b.extend_from_slice(&f.to_le_bytes());
        }
        b
    }

    pub fn store_embedding(conn: &Connection, symbol_id: i64, vec: &[f32]) -> rusqlite::Result<()> {
        conn.execute(
            "INSERT OR REPLACE INTO embedding_vecs(symbol_id, embedding) VALUES (?1, ?2)",
            rusqlite::params![symbol_id, vec_to_blob(vec)],
        )?;
        Ok(())
    }

    /// Embed every symbol that has no embedding yet; returns how many were added.
    pub fn embed_pending(conn: &Connection, embedder: &Embedder) -> rusqlite::Result<usize> {
        let rows: Vec<(i64, String, String, String)> = {
            let mut stmt = conn.prepare(
                "SELECT id, name, signature, docstring FROM symbols \
                 WHERE id NOT IN (SELECT symbol_id FROM embedding_vecs)",
            )?;
            stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?
                .collect::<rusqlite::Result<Vec<_>>>()?
        };
        if rows.is_empty() {
            return Ok(0);
        }
        let docs: Vec<String> = rows
            .iter()
            .map(|(_, n, sig, doc)| symbol_doc(n, sig, doc))
            .collect();
        let vecs = embedder.embed_batch(&docs);
        for ((id, ..), v) in rows.iter().zip(vecs.iter()) {
            store_embedding(conn, *id, v)?;
        }
        Ok(rows.len())
    }

    /// Nearest `k` symbol ids to `query` by cosine distance (ascending).
    pub fn knn(conn: &Connection, query: &[f32], k: usize) -> rusqlite::Result<Vec<(i64, f64)>> {
        let mut stmt = conn.prepare(
            "SELECT symbol_id, distance FROM embedding_vecs \
             WHERE embedding MATCH ?1 AND k = ?2 ORDER BY distance",
        )?;
        let rows = stmt
            .query_map(rusqlite::params![vec_to_blob(query), k as i64], |r| {
                Ok((r.get::<_, i64>(0)?, r.get::<_, f64>(1)?))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }
}

// ---------------------------------------------------------------------------
// Feature OFF: identical surface, every operation a no-op.
// ---------------------------------------------------------------------------
#[cfg(not(feature = "embeddings"))]
mod imp {
    use super::*;

    pub fn register_extension() {}

    pub fn create_embedding_table(_conn: &Connection, _dim: usize) -> rusqlite::Result<()> {
        Ok(())
    }

    /// Stub embedder — `load` always fails, so callers keep `None` and degrade.
    pub struct Embedder;

    impl Embedder {
        pub fn load(_model_id: &str, _dim: usize) -> anyhow::Result<Self> {
            anyhow::bail!("embeddings feature not enabled at build time")
        }
        pub fn dim(&self) -> usize {
            0
        }
        pub fn embed_one(&self, _text: &str) -> Vec<f32> {
            Vec::new()
        }
        pub fn embed_batch(&self, _texts: &[String]) -> Vec<Vec<f32>> {
            Vec::new()
        }
    }

    pub fn store_embedding(_c: &Connection, _id: i64, _v: &[f32]) -> rusqlite::Result<()> {
        Ok(())
    }
    pub fn embed_pending(_c: &Connection, _e: &Embedder) -> rusqlite::Result<usize> {
        Ok(0)
    }
    pub fn knn(_c: &Connection, _q: &[f32], _k: usize) -> rusqlite::Result<Vec<(i64, f64)>> {
        Ok(Vec::new())
    }
}

pub use imp::{
    Embedder, create_embedding_table, embed_pending, knn, register_extension, store_embedding,
};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn symbol_doc_joins_parts() {
        assert_eq!(
            symbol_doc("run", "fn run()", "does a thing"),
            "run fn run() does a thing"
        );
        assert_eq!(symbol_doc("run", "", ""), "run");
    }

    #[cfg(feature = "embeddings")]
    #[test]
    fn vec0_knn_with_synthetic_vectors() {
        use rusqlite::Connection;
        register_extension();
        let conn = Connection::open_in_memory().unwrap();
        crate::db::schema::init_db(&conn).unwrap();
        create_embedding_table(&conn, 3).unwrap();

        // Three unit-ish vectors; query is closest to id 2.
        store_embedding(&conn, 1, &[1.0, 0.0, 0.0]).unwrap();
        store_embedding(&conn, 2, &[0.0, 1.0, 0.0]).unwrap();
        store_embedding(&conn, 3, &[0.0, 0.0, 1.0]).unwrap();

        let hits = knn(&conn, &[0.1, 0.9, 0.0], 2).unwrap();
        assert_eq!(hits.len(), 2);
        assert_eq!(hits[0].0, 2, "nearest should be id 2");
    }
}
