//! Opt-in semantic embeddings (Cargo feature `embeddings`).
//!
//! Pure-Rust static code embeddings via `model2vec-rs` (default
//! `minishlab/potion-code-16M`, 256-dim). Nearest-neighbour search is a
//! exact brute-force cosine scan (`knn`/`knn_chunks`) over plain SQLite BLOB
//! storage — no vector-search C extension, so this stays pure Rust and
//! portable on every target (previously used `sqlite-vec`, which failed to
//! compile on musl libc). This module exposes the *same* surface in both
//! builds — when the feature is off, every entry point is a no-op and
//! semantic search degrades to FTS.

use rusqlite::Connection;

/// True when the crate was built with the `embeddings` feature.
pub const ENABLED: bool = cfg!(feature = "embeddings");

/// The default embedding model id (matches `SemanticSearchConfig::default`).
/// `Embedder::load` special-cases this id to use weights vendored into the
/// binary (see `assets/potion-code-16m/`) instead of fetching from the
/// HuggingFace Hub — kept as one constant so the two can't drift apart.
pub const DEFAULT_MODEL_ID: &str = "minishlab/potion-code-16M";

/// The text embedded for a symbol: name + signature + docstring. This is
/// Layer 1 of semantic search — *symbol identity*. Layer 2 (`code_chunks` /
/// `code_chunk_vecs`, populated by `indexer::chunker`) embeds the raw code
/// body instead, so a query that only matches implementation vocabulary (not
/// reflected in the name/signature/docstring) still has something to match.
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
// Feature ON: real model2vec-rs + brute-force cosine-scan implementation.
// ---------------------------------------------------------------------------
#[cfg(feature = "embeddings")]
mod imp {
    use super::*;
    use model2vec_rs::model::StaticModel;
    use rayon::prelude::*;

    /// Create the KNN table for symbol embeddings (idempotent). Plain BLOB
    /// storage — nearest-neighbour search is an exact brute-force
    /// cosine scan (see `knn`), not a vector-search extension.
    pub fn create_embedding_table(conn: &Connection, _dim: usize) -> rusqlite::Result<()> {
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS embedding_vecs (
                symbol_id INTEGER PRIMARY KEY,
                embedding BLOB NOT NULL
            );",
        )
    }

    /// Create the Layer-2 KNN table for code-chunk embeddings (idempotent).
    /// Separate from `embedding_vecs` — chunk ids and symbol ids are
    /// unrelated key spaces.
    pub fn create_chunk_embedding_table(conn: &Connection, _dim: usize) -> rusqlite::Result<()> {
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS code_chunk_vecs (
                chunk_id INTEGER PRIMARY KEY,
                embedding BLOB NOT NULL
            );",
        )
    }

    /// Bytes for the default model, vendored into the repo (MIT-licensed;
    /// `minishlab/potion-code-16M`, distilled from `nomic-ai/CodeRankEmbed`,
    /// also MIT) and baked into the binary at compile time. Loading the
    /// default model is then zero-I/O and zero-network — no HuggingFace Hub
    /// round-trip on first run, which is otherwise required before semantic
    /// search works (see `bootstrap_embeddings`).
    static DEFAULT_CONFIG: &[u8] = include_bytes!("../assets/potion-code-16m/config.json");
    static DEFAULT_TOKENIZER: &[u8] = include_bytes!("../assets/potion-code-16m/tokenizer.json");
    static DEFAULT_WEIGHTS: &[u8] = include_bytes!("../assets/potion-code-16m/model.safetensors");

    /// A loaded static embedding model.
    pub struct Embedder {
        model: StaticModel,
        dim: usize,
    }

    impl Embedder {
        /// Load `model_id`. The default model id (`DEFAULT_MODEL_ID`) loads
        /// from the bytes vendored into the binary; any other id (a custom
        /// model configured via `semantic_search.model`) still resolves via
        /// `from_pretrained` — a local path, or a HuggingFace Hub download.
        /// Output is L2-normalised so cosine distance behaves well.
        pub fn load(model_id: &str, dim: usize) -> anyhow::Result<Self> {
            let model = if model_id == DEFAULT_MODEL_ID {
                StaticModel::from_bytes(
                    DEFAULT_TOKENIZER,
                    DEFAULT_WEIGHTS,
                    DEFAULT_CONFIG,
                    Some(true),
                )
                .map_err(|e| anyhow::anyhow!("load vendored embedding model: {e}"))?
            } else {
                StaticModel::from_pretrained(model_id, None, Some(true), None)
                    .map_err(|e| anyhow::anyhow!("load embedding model '{model_id}': {e}"))?
            };
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

    fn blob_to_vec(b: &[u8]) -> Vec<f32> {
        b.chunks_exact(4)
            .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
            .collect()
    }

    /// Process-wide cache of decoded embeddings, keyed by the on-disk DB
    /// file path. A full-table blob fetch is dominated by SQLite's per-row
    /// marshaling, not the dot-product arithmetic, so `knn`/`knn_chunks`
    /// decode once per (re)index cycle and reuse the result across queries
    /// instead of re-fetching on every call. In-memory `:memory:`
    /// connections (`Connection::path()` returns `None` — every test in
    /// this crate uses one) bypass the cache entirely: always fetch fresh,
    /// so tests can never leak state into each other through it.
    type PathCache = std::sync::Mutex<std::collections::HashMap<String, Vec<(i64, Vec<f32>)>>>;

    fn symbol_cache() -> &'static PathCache {
        static CACHE: std::sync::OnceLock<PathCache> = std::sync::OnceLock::new();
        CACHE.get_or_init(|| std::sync::Mutex::new(std::collections::HashMap::new()))
    }

    fn chunk_cache() -> &'static PathCache {
        static CACHE: std::sync::OnceLock<PathCache> = std::sync::OnceLock::new();
        CACHE.get_or_init(|| std::sync::Mutex::new(std::collections::HashMap::new()))
    }

    fn invalidate(cache: &PathCache, conn: &Connection) {
        if let Some(path) = conn.path() {
            cache.lock().unwrap().remove(path);
        }
    }

    pub fn store_embedding(conn: &Connection, symbol_id: i64, vec: &[f32]) -> rusqlite::Result<()> {
        conn.execute(
            "INSERT OR REPLACE INTO embedding_vecs(symbol_id, embedding) VALUES (?1, ?2)",
            rusqlite::params![symbol_id, vec_to_blob(vec)],
        )?;
        invalidate(symbol_cache(), conn);
        Ok(())
    }

    pub fn store_chunk_embedding(
        conn: &Connection,
        chunk_id: i64,
        vec: &[f32],
    ) -> rusqlite::Result<()> {
        conn.execute(
            "INSERT OR REPLACE INTO code_chunk_vecs(chunk_id, embedding) VALUES (?1, ?2)",
            rusqlite::params![chunk_id, vec_to_blob(vec)],
        )?;
        invalidate(chunk_cache(), conn);
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

    /// Nearest `k` symbol ids to `query` by cosine distance (ascending) —
    /// exact brute-force scan, data-parallel via `rayon` over a decoded
    /// cache (see `symbol_cache`) so repeat queries within one (re)index
    /// cycle skip the SQL fetch entirely — SQLite's per-row marshaling, not
    /// the arithmetic, is what dominates cost at table-scan scale. Vectors
    /// are L2-normalised (`Embedder::load`), so cosine distance reduces to
    /// `1.0 - dot_product`.
    pub fn knn(conn: &Connection, query: &[f32], k: usize) -> rusqlite::Result<Vec<(i64, f64)>> {
        match conn.path() {
            Some(path) => {
                let mut guard = symbol_cache().lock().unwrap();
                if !guard.contains_key(path) {
                    let vecs = fetch_symbol_vecs(conn)?;
                    guard.insert(path.to_string(), vecs);
                }
                Ok(top_k_by_cosine(&guard[path], query, k))
            }
            None => Ok(top_k_by_cosine(&fetch_symbol_vecs(conn)?, query, k)),
        }
    }

    fn fetch_symbol_vecs(conn: &Connection) -> rusqlite::Result<Vec<(i64, Vec<f32>)>> {
        let mut stmt = conn.prepare("SELECT symbol_id, embedding FROM embedding_vecs")?;
        stmt.query_map([], |r| {
            let id: i64 = r.get(0)?;
            let blob = r.get_ref(1)?.as_blob().unwrap_or(&[]);
            Ok((id, blob_to_vec(blob)))
        })?
        .collect()
    }

    /// Remove `code_chunk_vecs` rows whose chunk no longer exists in
    /// `code_chunks` (file changed or was deleted since the vector was
    /// written). Returns how many rows were pruned. Unlike `symbols` —
    /// `code_chunks` rows are always deleted-and-reinserted as a unit per file
    /// (see `indexer::pipeline::remove_file_rows`), so their ids never survive
    /// a reindex of that file; without this, stale orphans would accumulate
    /// forever and could crowd out real matches in `knn_chunks` (a KNN query
    /// has no way to know a returned id is dangling before doing this exact
    /// lookup).
    pub fn prune_orphaned_chunk_vecs(conn: &Connection) -> rusqlite::Result<usize> {
        let n = conn.execute(
            "DELETE FROM code_chunk_vecs WHERE chunk_id NOT IN (SELECT id FROM code_chunks)",
            [],
        )?;
        if n > 0 {
            invalidate(chunk_cache(), conn);
        }
        Ok(n)
    }

    /// Embed every Layer-2 code chunk that has no embedding yet; returns how
    /// many were added. Prunes orphaned vectors first — see
    /// `prune_orphaned_chunk_vecs`.
    pub fn embed_pending_chunks(conn: &Connection, embedder: &Embedder) -> rusqlite::Result<usize> {
        prune_orphaned_chunk_vecs(conn)?;

        let rows: Vec<(i64, String)> = {
            let mut stmt = conn.prepare(
                "SELECT id, chunk_text FROM code_chunks \
                 WHERE id NOT IN (SELECT chunk_id FROM code_chunk_vecs)",
            )?;
            stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
                .collect::<rusqlite::Result<Vec<_>>>()?
        };
        if rows.is_empty() {
            return Ok(0);
        }
        let texts: Vec<String> = rows.iter().map(|(_, text)| text.clone()).collect();
        let vecs = embedder.embed_batch(&texts);
        for ((id, _), v) in rows.iter().zip(vecs.iter()) {
            store_chunk_embedding(conn, *id, v)?;
        }
        Ok(rows.len())
    }

    /// Nearest `k` chunk ids to `query` by cosine distance (ascending) —
    /// same cached, data-parallel scan as `knn`.
    pub fn knn_chunks(
        conn: &Connection,
        query: &[f32],
        k: usize,
    ) -> rusqlite::Result<Vec<(i64, f64)>> {
        match conn.path() {
            Some(path) => {
                let mut guard = chunk_cache().lock().unwrap();
                if !guard.contains_key(path) {
                    let vecs = fetch_chunk_vecs(conn)?;
                    guard.insert(path.to_string(), vecs);
                }
                Ok(top_k_by_cosine(&guard[path], query, k))
            }
            None => Ok(top_k_by_cosine(&fetch_chunk_vecs(conn)?, query, k)),
        }
    }

    fn fetch_chunk_vecs(conn: &Connection) -> rusqlite::Result<Vec<(i64, Vec<f32>)>> {
        let mut stmt = conn.prepare("SELECT chunk_id, embedding FROM code_chunk_vecs")?;
        stmt.query_map([], |r| {
            let id: i64 = r.get(0)?;
            let blob = r.get_ref(1)?.as_blob().unwrap_or(&[]);
            Ok((id, blob_to_vec(blob)))
        })?
        .collect()
    }

    /// Data-parallel brute-force top-`k` by cosine distance (ascending —
    /// smallest distance first) over already-decoded vectors.
    fn top_k_by_cosine(vecs: &[(i64, Vec<f32>)], query: &[f32], k: usize) -> Vec<(i64, f64)> {
        let mut scored: Vec<(i64, f64)> = vecs
            .par_iter()
            .map(|(id, v)| {
                let dot: f32 = v.iter().zip(query).map(|(a, b)| a * b).sum();
                (*id, (1.0 - dot) as f64)
            })
            .collect();
        scored.sort_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal));
        scored.truncate(k);
        scored
    }
}

// ---------------------------------------------------------------------------
// Feature OFF: identical surface, every operation a no-op.
// ---------------------------------------------------------------------------
#[cfg(not(feature = "embeddings"))]
mod imp {
    use super::*;

    pub fn create_embedding_table(_conn: &Connection, _dim: usize) -> rusqlite::Result<()> {
        Ok(())
    }

    pub fn create_chunk_embedding_table(_conn: &Connection, _dim: usize) -> rusqlite::Result<()> {
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
    pub fn store_chunk_embedding(_c: &Connection, _id: i64, _v: &[f32]) -> rusqlite::Result<()> {
        Ok(())
    }
    pub fn prune_orphaned_chunk_vecs(_c: &Connection) -> rusqlite::Result<usize> {
        Ok(0)
    }
    pub fn embed_pending_chunks(_c: &Connection, _e: &Embedder) -> rusqlite::Result<usize> {
        Ok(0)
    }
    pub fn knn_chunks(_c: &Connection, _q: &[f32], _k: usize) -> rusqlite::Result<Vec<(i64, f64)>> {
        Ok(Vec::new())
    }
}

pub use imp::{
    Embedder, create_chunk_embedding_table, create_embedding_table, embed_pending,
    embed_pending_chunks, knn, knn_chunks, prune_orphaned_chunk_vecs, store_chunk_embedding,
    store_embedding,
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

    /// Regression for the vendored default model (`include_bytes!` in
    /// `Embedder::load`): loads with zero network, produces the right
    /// dimensionality, and is L2-normalised. Catches a bad asset path, a
    /// corrupted file, or an unexpected tensor layout at test time instead
    /// of at first-run in production.
    #[cfg(feature = "embeddings")]
    #[test]
    fn default_model_loads_from_vendored_bytes() {
        let embedder = Embedder::load(DEFAULT_MODEL_ID, 256).expect("vendored model must load");
        let v = embedder.embed_one("fn parse_config(path: &str) -> Config");
        assert_eq!(v.len(), 256);
        let norm: f32 = v.iter().map(|x| x * x).sum::<f32>().sqrt();
        assert!(
            (norm - 1.0).abs() < 1e-3,
            "output should be L2-normalised, got {norm}"
        );
    }

    #[cfg(feature = "embeddings")]
    #[test]
    fn knn_with_synthetic_vectors() {
        use rusqlite::Connection;
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

    #[cfg(feature = "embeddings")]
    #[test]
    fn knn_chunks_with_synthetic_vectors() {
        use rusqlite::Connection;
        let conn = Connection::open_in_memory().unwrap();
        crate::db::schema::init_db(&conn).unwrap();
        create_chunk_embedding_table(&conn, 3).unwrap();

        // Layer-2 chunk vectors live in their own table/key-space from symbols.
        store_chunk_embedding(&conn, 10, &[1.0, 0.0, 0.0]).unwrap();
        store_chunk_embedding(&conn, 20, &[0.0, 1.0, 0.0]).unwrap();
        store_chunk_embedding(&conn, 30, &[0.0, 0.0, 1.0]).unwrap();

        let hits = knn_chunks(&conn, &[0.0, 0.0, 0.9], 2).unwrap();
        assert_eq!(hits.len(), 2);
        assert_eq!(hits[0].0, 30, "nearest should be chunk id 30");
    }

    #[cfg(feature = "embeddings")]
    #[test]
    fn prune_orphaned_chunk_vecs_removes_only_dangling_rows() {
        use rusqlite::Connection;
        let conn = Connection::open_in_memory().unwrap();
        crate::db::schema::init_db(&conn).unwrap();
        create_chunk_embedding_table(&conn, 3).unwrap();

        // id 2 has a matching code_chunks row; id 1 is an orphan (e.g. left
        // over from a file that was since reindexed with new chunk ids).
        conn.execute(
            "INSERT INTO code_chunks (id, path, line_start, line_end, chunk_text, file_hash) \
             VALUES (2, 'a.py', 1, 1, 'pass', '')",
            [],
        )
        .unwrap();
        store_chunk_embedding(&conn, 1, &[1.0, 0.0, 0.0]).unwrap();
        store_chunk_embedding(&conn, 2, &[0.0, 1.0, 0.0]).unwrap();

        let pruned = prune_orphaned_chunk_vecs(&conn).unwrap();
        assert_eq!(pruned, 1, "exactly the dangling id-1 row must be pruned");

        let hits = knn_chunks(&conn, &[0.0, 1.0, 0.0], 10).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].0, 2);
    }

    /// KNN latency benchmark: 100k synthetic 256-dim vectors, topK=10, a
    /// *fresh connection per query* — matching real MCP usage
    /// (`make_read_conn` opens a new `Connection` per tool call). The cache
    /// is keyed by file path, not `Connection` identity, so this still
    /// exercises it.
    /// Run with: cargo test -p ci-core --release --features embeddings -- --ignored --nocapture bench_knn_latency
    #[cfg(feature = "embeddings")]
    #[test]
    #[ignore]
    fn bench_knn_latency_100k_256dim() {
        use rusqlite::Connection;
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("bench.db");
        let conn = Connection::open(&db_path).unwrap();
        crate::db::schema::init_db(&conn).unwrap();
        create_embedding_table(&conn, 256).unwrap();

        const N: usize = 100_000;
        const D: usize = 256;
        let insert_start = std::time::Instant::now();
        conn.execute_batch("BEGIN").unwrap();
        for i in 0..N {
            let v: Vec<f32> = (0..D).map(|j| ((i * D + j) % 997) as f32 / 997.0).collect();
            store_embedding(&conn, i as i64 + 1, &v).unwrap();
        }
        conn.execute_batch("COMMIT").unwrap();
        eprintln!("Inserted {N} vectors in {:?}", insert_start.elapsed());

        let query: Vec<f32> = (0..D).map(|j| j as f32 / D as f32).collect();
        let warmup_hits = knn(&conn, &query, 10).unwrap();
        assert!(!warmup_hits.is_empty(), "warmup KNN returned nothing");

        const RUNS: u32 = 20;
        let t = std::time::Instant::now();
        for _ in 0..RUNS {
            let fresh = Connection::open(&db_path).unwrap();
            let h = knn(&fresh, &query, 10).unwrap();
            assert_eq!(h.len(), 10);
        }
        let total = t.elapsed();
        eprintln!(
            "KNN {N}×{D} topK=10 (fresh connection/query): {RUNS} queries | total={total:?} | avg={:?}/query",
            total / RUNS
        );
    }
}
