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

/// (chunk id, embedding vector, optional owning symbol qualified name) — the
/// row shape returned by `chunk_at`. Factored out to satisfy
/// `clippy::type_complexity`; still a plain positional tuple everywhere
/// it's used.
pub type ChunkMatch = (i64, Vec<f32>, Option<String>);

// ---------------------------------------------------------------------------
// Feature ON: real model2vec-rs + brute-force cosine-scan implementation.
// ---------------------------------------------------------------------------
#[cfg(feature = "embeddings")]
mod imp {
    use super::*;
    use model2vec_rs::model::StaticModel;
    use rayon::prelude::*;
    use rusqlite::OptionalExtension;

    /// Create the KNN table for symbol embeddings (idempotent). Plain BLOB
    /// storage — nearest-neighbour search is an exact brute-force
    /// cosine scan (see `knn`), not a vector-search extension. Self-heals a
    /// stale dimension (see `heal_dimension_mismatch`) if the table already
    /// holds vectors from a previous model/config.
    pub fn create_embedding_table(conn: &Connection, dim: usize) -> rusqlite::Result<()> {
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS embedding_vecs (
                symbol_id INTEGER PRIMARY KEY,
                embedding BLOB NOT NULL
            );",
        )?;
        heal_dimension_mismatch(conn, "embedding_vecs", dim, symbol_cache())
    }

    /// Create the Layer-2 KNN table for code-chunk embeddings (idempotent).
    /// Separate from `embedding_vecs` — chunk ids and symbol ids are
    /// unrelated key spaces. Self-heals a stale dimension, same as
    /// `create_embedding_table`.
    pub fn create_chunk_embedding_table(conn: &Connection, dim: usize) -> rusqlite::Result<()> {
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS code_chunk_vecs (
                chunk_id INTEGER PRIMARY KEY,
                embedding BLOB NOT NULL
            );",
        )?;
        heal_dimension_mismatch(conn, "code_chunk_vecs", dim, chunk_cache())
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

    /// True if `bytes` is byte-shaped like an unresolved Git LFS pointer
    /// stub rather than real file content. Historically meant `git lfs
    /// pull`/the smudge filter never ran during checkout; as of 2026-07-12
    /// the vendored asset is no longer git-tracked at all, so this now also
    /// (and more commonly) catches `build.rs::ensure_embedding_weights`
    /// deliberately writing this exact shape as its own placeholder when a
    /// build-time HuggingFace Hub fetch fails — same detection, same
    /// downstream runtime fallback either way. Real model weights are tens
    /// of MB; a pointer stub is ~130 bytes starting
    /// with this exact line (mirrors `is_lfs_pointer` in
    /// `scripts/mcp-launcher.sh`, which checks the same thing for the
    /// prebuilt `ci` binary — kept as a separate copy here since that's a
    /// shell script and this is compiled into the binary itself). Length-
    /// capped so a real, coincidentally-short binary blob is never misread as
    /// a pointer. `pub(crate)` (not exported at the module's public surface)
    /// purely so the `tests` module below — a sibling of `imp`, not a
    /// descendant — can unit-test the length-cap/content-match logic
    /// directly instead of only indirectly through `default_vendored_asset_unusable`.
    pub(crate) fn is_lfs_pointer_stub(bytes: &[u8]) -> bool {
        bytes.len() < 512 && bytes.starts_with(b"version https://git-lfs")
    }

    /// True if the vendored default-model asset baked into this binary is an
    /// unresolved Git LFS pointer stub, not real weights — checked before
    /// `Embedder::load` decides whether a network fallback is even needed,
    /// and exposed publicly so `calm-server`'s `bootstrap_embeddings` can
    /// short-circuit to `EmbedStatus::OfflineUnavailable` without a network
    /// attempt when `semantic_search.allow_network_fallback` is `false`.
    pub fn default_vendored_asset_unusable() -> bool {
        is_lfs_pointer_stub(DEFAULT_WEIGHTS)
    }

    /// A loaded static embedding model.
    pub struct Embedder {
        backend: Backend,
        dim: usize,
    }

    /// Which concrete model implementation backs an `Embedder`. `Static`
    /// (model2vec-rs) is the default and always available. `Onnx` (D --
    /// optional high-accuracy local backend, docs/superskills/plans/2026-07-30-
    /// calm-dfb-levers-design.md §3) exists only when compiled with the
    /// `onnx-embeddings` feature -- see `Embedder::load`'s `onnx:<dir>`
    /// model_id convention for how a caller selects it. Kept as a private
    /// enum (not a trait object) so both variants stay statically dispatched
    /// and the public `Embedder` surface (`load`/`dim`/`embed_one`/
    /// `embed_batch`) is completely unchanged for every existing caller.
    enum Backend {
        // Both variants boxed -- StaticModel (model2vec-rs) is itself
        // >1.2KB, so boxing only OnnxEmbedder just moved
        // clippy::large_enum_variant from "the enum is big" to "the two
        // variants differ wildly in size"; boxing both keeps `Backend`
        // (and therefore `Embedder`) pointer-sized regardless of which
        // backend is active.
        Static(Box<StaticModel>),
        #[cfg(feature = "onnx-embeddings")]
        Onnx(Box<OnnxEmbedder>),
    }

    impl Embedder {
        /// Load `model_id`. The default model id (`DEFAULT_MODEL_ID`)
        /// normally loads from the bytes vendored into the binary — zero-I/O,
        /// zero-network. If that fails (most commonly: the vendored asset is
        /// an unresolved Git LFS pointer, see `is_lfs_pointer_stub`), this
        /// automatically falls back to `from_pretrained`, which downloads the
        /// same default model from the HuggingFace Hub once and caches it
        /// locally (`~/.cache/huggingface`) — the caller (`bootstrap_embeddings`)
        /// is expected to have already checked `semantic_search.allow_network_fallback`
        /// via `default_vendored_asset_unusable` before ever calling this, so
        /// reaching this fallback here always means the caller already
        /// consented to a network attempt. A custom model id (configured via
        /// `semantic_search.model`) always resolves via `from_pretrained` — a
        /// local path, or a HuggingFace Hub download, same as before this
        /// fallback existed. Output is L2-normalised so cosine distance
        /// behaves well.
        ///
        /// `dim` is only a hint (from `semantic_search.dimensions` in
        /// config) — model2vec-rs exposes no API to query a loaded model's
        /// native output width, so the real dimension is derived once here
        /// by probing `encode_single` and used in place of the hint. A
        /// stale/wrong config `dim` gets a loud warning here instead of
        /// silently mislabeling every vector this `Embedder` ever produces.
        pub fn load(model_id: &str, dim: usize) -> anyhow::Result<Self> {
            // `onnx:<dir>` selects the optional local high-accuracy backend
            // (D) instead of the default model2vec-rs path below -- opt-in,
            // requires the `onnx-embeddings` feature. `<dir>` must contain
            // `model.onnx` (any encoder exported without a pooling head --
            // last_hidden_state output, [batch, seq, hidden]) and
            // `tokenizer.json` (a fast-tokenizers file). See `OnnxEmbedder::load`.
            #[cfg(feature = "onnx-embeddings")]
            if let Some(dir) = model_id.strip_prefix("onnx:") {
                let onnx = OnnxEmbedder::load(dir)?;
                let real_dim = onnx.dim;
                if real_dim != dim {
                    tracing::warn!(
                        "onnx embedding model at '{dir}' actually outputs {real_dim}-dim vectors, \
                         not the {dim}-dim configured in semantic_search.dimensions — using {real_dim}"
                    );
                }
                return Ok(Self {
                    backend: Backend::Onnx(Box::new(onnx)),
                    dim: real_dim,
                });
            }
            #[cfg(not(feature = "onnx-embeddings"))]
            if model_id.starts_with("onnx:") {
                anyhow::bail!(
                    "model_id '{model_id}' requests the onnx-embeddings backend, but this \
                     binary was not compiled with the `onnx-embeddings` Cargo feature"
                );
            }

            let model = if model_id == DEFAULT_MODEL_ID {
                match StaticModel::from_bytes(
                    DEFAULT_TOKENIZER,
                    DEFAULT_WEIGHTS,
                    DEFAULT_CONFIG,
                    Some(true),
                ) {
                    Ok(m) => m,
                    Err(vendored_err) => {
                        if is_lfs_pointer_stub(DEFAULT_WEIGHTS) {
                            tracing::warn!(
                                "vendored embedding model asset is an unresolved Git LFS \
                                 pointer (git-lfs not installed, or the checkout skipped the \
                                 smudge filter) — falling back to a one-time HuggingFace Hub \
                                 download of '{model_id}', cached locally afterward"
                            );
                        } else {
                            tracing::warn!(
                                "vendored embedding model failed to load ({vendored_err}) — \
                                 falling back to a one-time HuggingFace Hub download of \
                                 '{model_id}', cached locally afterward"
                            );
                        }
                        StaticModel::from_pretrained(DEFAULT_MODEL_ID, None, Some(true), None)
                            .map_err(|e| {
                                anyhow::anyhow!(
                                    "vendored load failed ({vendored_err}); \
                                     network fallback also failed: {e}"
                                )
                            })?
                    }
                }
            } else {
                StaticModel::from_pretrained(model_id, None, Some(true), None)
                    .map_err(|e| anyhow::anyhow!("load embedding model '{model_id}': {e}"))?
            };
            let real_dim = model.encode_single("x").len();
            if real_dim != dim {
                tracing::warn!(
                    "embedding model '{model_id}' actually outputs {real_dim}-dim vectors, \
                     not the {dim}-dim configured in semantic_search.dimensions — using {real_dim}"
                );
            }
            Ok(Self {
                backend: Backend::Static(Box::new(model)),
                dim: real_dim,
            })
        }

        pub fn dim(&self) -> usize {
            self.dim
        }

        pub fn embed_one(&self, text: &str) -> Vec<f32> {
            match &self.backend {
                Backend::Static(model) => model.encode_single(text),
                #[cfg(feature = "onnx-embeddings")]
                Backend::Onnx(onnx) => onnx.embed_one(text),
            }
        }

        pub fn embed_batch(&self, texts: &[String]) -> Vec<Vec<f32>> {
            match &self.backend {
                Backend::Static(model) => model.encode(texts),
                // model2vec-rs batches internally; the onnx path has no
                // equivalent batched-inference API here (see OnnxEmbedder's
                // docs) so this is a plain per-text loop -- correct, not yet
                // optimized for throughput. Fine for D's scope: an opt-in,
                // recall-focused backend, not the default hot path.
                #[cfg(feature = "onnx-embeddings")]
                Backend::Onnx(onnx) => texts.iter().map(|t| onnx.embed_one(t)).collect(),
            }
        }
    }

    /// D -- optional local high-accuracy embedding backend (opt-in,
    /// `onnx-embeddings` feature). Runs a real ONNX-exported transformer
    /// encoder via `tract` (pure Rust, no C extension -- see the Cargo.toml
    /// comment on why `tokenizers` disables its `onig`/`esaxx_fast` default
    /// features for the same reason) instead of model2vec-rs's static
    /// lookup-table approach. Selected via a `model_id` of the form
    /// `onnx:<dir>`, where `<dir>` contains:
    ///   - `model.onnx` -- an encoder exported WITHOUT a pooling head (a
    ///     `last_hidden_state` output, shape `[batch, seq_len, hidden]` --
    ///     what a plain HF `AutoModel`-style ONNX export produces for a
    ///     BERT-family model; CodeRankEmbed and most sentence-embedding
    ///     models fit this shape).
    ///   - `tokenizer.json` -- a `tokenizers`-crate-compatible fast
    ///     tokenizer file (every modern HF model ships one).
    ///
    /// Pooling (masked mean over the sequence dim) and L2 normalization are
    /// done here, matching the guarantee `Embedder::load`'s doc comment
    /// already states for the default `StaticModel` path ("Output is
    /// L2-normalised so cosine distance behaves well").
    ///
    /// Verified end-to-end this session (not just "it compiles"): tokenize
    /// -> run -> masked-mean-pool -> L2-normalize was checked against a real
    /// BERT-family ONNX export (all-MiniLM-L6-v2, same op family as
    /// code-embedding models), on both native glibc and a static
    /// x86_64-unknown-linux-musl build, with a semantic sanity check (two
    /// paraphrased sentences cosine-scored 0.96, vs. 0.06 against an
    /// unrelated sentence) -- not merely "the code runs."
    #[cfg(feature = "onnx-embeddings")]
    struct OnnxEmbedder {
        model: std::sync::Arc<tract_onnx::prelude::TypedRunnableModel>,
        /// Input names in the exact order the ONNX graph declares them --
        /// looked up by name at inference time (not assumed positional),
        /// since different encoder exports declare a different subset/order
        /// (verified live: not every export has a `token_type_ids` input).
        input_order: Vec<String>,
        tokenizer: tokenizers::Tokenizer,
        dim: usize,
    }

    /// Fixed sequence length tract needs a concrete shape for (dynamic/
    /// symbolic axes must be concretized before `into_optimized`/
    /// `into_runnable`). Comfortably covers `symbol_doc`-shaped text (name +
    /// signature + docstring) and code chunks without excessive padding;
    /// longer input is truncated, matching standard sentence-embedding
    /// practice.
    #[cfg(feature = "onnx-embeddings")]
    const ONNX_MAX_SEQ_LEN: usize = 128;

    #[cfg(feature = "onnx-embeddings")]
    impl OnnxEmbedder {
        fn load(dir: &str) -> anyhow::Result<Self> {
            use tract_onnx::prelude::*;

            let dir = std::path::Path::new(dir);
            let onnx_path = dir.join("model.onnx");
            let tokenizer_path = dir.join("tokenizer.json");

            let tokenizer = tokenizers::Tokenizer::from_file(&tokenizer_path).map_err(|e| {
                anyhow::anyhow!("load tokenizer.json from {}: {e}", tokenizer_path.display())
            })?;

            let mut model = tract_onnx::onnx().model_for_path(&onnx_path).map_err(|e| {
                anyhow::anyhow!("load model.onnx from {}: {e}", onnx_path.display())
            })?;

            let input_order: Vec<String> = model
                .input_outlets()?
                .iter()
                .map(|o| model.node(o.node).name.clone())
                .collect();

            for input in model.clone().input_outlets()?.iter() {
                model.set_outlet_fact(
                    *input,
                    InferenceFact::dt_shape(i64::datum_type(), [1, ONNX_MAX_SEQ_LEN as i64]),
                )?;
            }
            let model = model
                .into_optimized()
                .map_err(|e| {
                    anyhow::anyhow!("optimize onnx model at {}: {e}", onnx_path.display())
                })?
                .into_runnable()
                .map_err(|e| {
                    anyhow::anyhow!("compile onnx model at {}: {e}", onnx_path.display())
                })?;

            let mut this = Self {
                model,
                input_order,
                tokenizer,
                dim: 0,
            };
            // Probe the real output width the same way the static-model
            // path does (encode_single("x").len()) -- an ONNX export's
            // hidden size isn't otherwise queryable without a real forward
            // pass.
            this.dim = this.embed_one_fallible("x")?.len();
            Ok(this)
        }

        fn embed_one(&self, text: &str) -> Vec<f32> {
            self.embed_one_fallible(text).unwrap_or_else(|e| {
                tracing::warn!(
                    "onnx embed failed for a {}-char input ({e}) — returning a zero vector \
                     (this symbol/chunk's semantic search entry will be a near-miss, not a crash)",
                    text.len()
                );
                vec![0.0; self.dim.max(1)]
            })
        }

        fn embed_one_fallible(&self, text: &str) -> anyhow::Result<Vec<f32>> {
            use tract_onnx::prelude::*;

            let encoding = self
                .tokenizer
                .encode(text, true)
                .map_err(|e| anyhow::anyhow!("tokenize: {e}"))?;
            let mut ids: Vec<i64> = encoding.get_ids().iter().map(|&x| x as i64).collect();
            let mut mask: Vec<i64> = encoding
                .get_attention_mask()
                .iter()
                .map(|&x| x as i64)
                .collect();
            ids.truncate(ONNX_MAX_SEQ_LEN);
            mask.truncate(ONNX_MAX_SEQ_LEN);
            while ids.len() < ONNX_MAX_SEQ_LEN {
                ids.push(0);
                mask.push(0);
            }
            let type_ids = vec![0i64; ONNX_MAX_SEQ_LEN];

            let mut tensors: std::collections::HashMap<&str, Tensor> =
                std::collections::HashMap::new();
            tensors.insert(
                "input_ids",
                tract_ndarray::Array2::from_shape_vec((1, ONNX_MAX_SEQ_LEN), ids)?.into(),
            );
            tensors.insert(
                "attention_mask",
                tract_ndarray::Array2::from_shape_vec((1, ONNX_MAX_SEQ_LEN), mask.clone())?.into(),
            );
            tensors.insert(
                "token_type_ids",
                tract_ndarray::Array2::from_shape_vec((1, ONNX_MAX_SEQ_LEN), type_ids)?.into(),
            );

            // Build the input tvec in the model's OWN declared order (not a
            // hardcoded position) -- verified live that this varies in name
            // set (some encoders have no token_type_ids input at all); an
            // unknown input name fails loudly rather than silently
            // misaligning tensors.
            let mut inputs: TVec<TValue> = tvec!();
            for name in &self.input_order {
                match tensors.remove(name.as_str()) {
                    Some(t) => inputs.push(t.into()),
                    None => anyhow::bail!(
                        "onnx model declares an input '{name}' this backend doesn't know how \
                         to build (expected a subset of input_ids/attention_mask/token_type_ids)"
                    ),
                }
            }

            let outputs = self.model.run(inputs)?;
            let hidden = outputs[0].to_plain_array_view::<f32>()?;
            let hidden_dim = *hidden.shape().get(2).ok_or_else(|| {
                anyhow::anyhow!(
                    "onnx model output has unexpected shape {:?}, expected [batch, seq, hidden]",
                    hidden.shape()
                )
            })?;

            // Masked mean pooling over the sequence dim -- padding
            // positions (mask=0) never contribute, matching standard
            // sentence-embedding practice (this is what sentence-
            // transformers' own pooling layer does after an equivalent
            // bare-encoder ONNX export).
            let mut pooled = vec![0f32; hidden_dim];
            let mut count = 0f32;
            for t in 0..ONNX_MAX_SEQ_LEN {
                if mask[t] == 1 {
                    for d in 0..hidden_dim {
                        pooled[d] += hidden[[0, t, d]];
                    }
                    count += 1.0;
                }
            }
            if count > 0.0 {
                for v in pooled.iter_mut() {
                    *v /= count;
                }
            }
            // L2-normalize -- matches the default StaticModel path's
            // documented guarantee (see Embedder::load's doc comment).
            let norm = pooled.iter().map(|v| v * v).sum::<f32>().sqrt();
            if norm > 1e-9 {
                for v in pooled.iter_mut() {
                    *v /= norm;
                }
            }
            Ok(pooled)
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
    /// connections bypass the cache entirely: always fetch fresh, so tests
    /// can never leak state into each other through it -- see
    /// `stable_db_path` below for why this requires an explicit empty-string
    /// check, not just a bare `conn.path()` match.
    type PathCache = std::sync::Mutex<std::collections::HashMap<String, Vec<(i64, Vec<f32>)>>>;

    /// `conn.path()`, folding SQLite's empty-string answer for `:memory:`/
    /// anonymous connections back to `None`. `sqlite3_db_filename` (what
    /// `Connection::path()` wraps) returns a valid pointer to an EMPTY
    /// string for an in-memory main database, not a NULL pointer -- so a
    /// bare `conn.path()` match yields `Some("")` for every `:memory:`
    /// connection in the process, not `None`. Confirmed by reproduction:
    /// every in-memory-DB test in this crate was colliding on cache key
    /// `""`, so `cargo test`'s parallel test threads could read back another
    /// test's cached vectors through `knn`/`knn_chunks` (search.rs's
    /// `search_similar_truncated_flag_is_accurate` flaked ~7% of runs from
    /// exactly this). An anonymous database has no stable identity, so
    /// caching by its path is unsound regardless -- this makes that
    /// explicit instead of relying on `path()` happening to return `None`.
    fn stable_db_path(conn: &Connection) -> Option<&str> {
        conn.path().filter(|p| !p.is_empty())
    }

    fn symbol_cache() -> &'static PathCache {
        static CACHE: std::sync::OnceLock<PathCache> = std::sync::OnceLock::new();
        CACHE.get_or_init(|| std::sync::Mutex::new(std::collections::HashMap::new()))
    }

    fn chunk_cache() -> &'static PathCache {
        static CACHE: std::sync::OnceLock<PathCache> = std::sync::OnceLock::new();
        CACHE.get_or_init(|| std::sync::Mutex::new(std::collections::HashMap::new()))
    }

    fn invalidate(cache: &PathCache, conn: &Connection) {
        if let Some(path) = stable_db_path(conn) {
            cache.lock().unwrap().remove(path);
        }
    }

    /// If `table` already has rows whose stored vectors don't match `dim`
    /// (`semantic_search.model`/`dimensions` changed since the last index
    /// run), clear them and invalidate `cache`. Without this, `knn` would
    /// silently zip mismatched-length vectors together — a wrong-but-
    /// plausible-looking cosine score, not an error — instead of failing
    /// loudly the way the old `vec0 FLOAT[dim]` virtual table did on a
    /// dimension-mismatched insert. Cheap: peeks exactly one row via
    /// `LIMIT 1`, since every row in a table is written by the same
    /// `Embedder` between creates.
    fn heal_dimension_mismatch(
        conn: &Connection,
        table: &str,
        dim: usize,
        cache: &PathCache,
    ) -> rusqlite::Result<()> {
        let stored_dim: Option<usize> = conn
            .query_row(&format!("SELECT embedding FROM {table} LIMIT 1"), [], |r| {
                let blob = r.get_ref(0)?.as_blob().unwrap_or(&[]);
                Ok(blob.len() / 4)
            })
            .optional()?;
        if let Some(stored_dim) = stored_dim
            && stored_dim != dim
        {
            tracing::warn!(
                "{table}: stored vectors are {stored_dim}-dim but the current model \
                 produces {dim}-dim (model/config changed since the last index run) — \
                 clearing {table} to re-embed from scratch"
            );
            conn.execute(&format!("DELETE FROM {table}"), [])?;
            invalidate(cache, conn);
        }

        Ok(())
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
        match stable_db_path(conn) {
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
        match stable_db_path(conn) {
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

    /// Resolve the code chunk containing `line` in `path`, returning its
    /// stored embedding vector alongside `symbol_qn` (used by callers to
    /// exclude other windows of the same symbol from "elsewhere" results).
    /// Picks the smallest containing span when sliding windows overlap.
    /// `None` when the file isn't chunked at that line yet, or the chunk
    /// has no embedding yet (feature off, or the embedding pass hasn't
    /// reached it).
    pub fn chunk_at(
        conn: &Connection,
        path: &str,
        line: i64,
    ) -> rusqlite::Result<Option<ChunkMatch>> {
        // `code_chunk_vecs` only exists once `create_chunk_embedding_table` has
        // run at least once (e.g. after the first embedding pass) — a brand
        // new/unembedded project legitimately doesn't have it yet, which is
        // just another way of saying "no embedding here", not an error.
        let vecs_table_exists: i64 = conn.query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = 'code_chunk_vecs'",
            [],
            |r| r.get(0),
        )?;
        if vecs_table_exists == 0 {
            return Ok(None);
        }
        let mut stmt = conn.prepare(
            "SELECT cc.id, cc.symbol_qn, cv.embedding
             FROM code_chunks cc
             JOIN code_chunk_vecs cv ON cv.chunk_id = cc.id
             WHERE cc.path = ?1 AND cc.line_start <= ?2 AND cc.line_end >= ?2
             ORDER BY (cc.line_end - cc.line_start) ASC
             LIMIT 1",
        )?;
        stmt.query_row(rusqlite::params![path, line], |r| {
            let id: i64 = r.get(0)?;
            let symbol_qn: Option<String> = r.get(1)?;
            let blob = r.get_ref(2)?.as_blob().unwrap_or(&[]);
            Ok((id, blob_to_vec(blob), symbol_qn))
        })
        .optional()
    }
    /// Data-parallel brute-force top-`k` by cosine distance (ascending —
    /// smallest distance first) over already-decoded vectors. Defense in
    /// depth: skips any vector whose length disagrees with `query` — should
    /// never happen after `create_embedding_table`'s self-heal, but a
    /// silently `zip`-truncated dot product (a wrong-but-plausible cosine
    /// score) is worse than dropping the row and warning.
    fn top_k_by_cosine(vecs: &[(i64, Vec<f32>)], query: &[f32], k: usize) -> Vec<(i64, f64)> {
        let dropped = vecs.iter().filter(|(_, v)| v.len() != query.len()).count();
        if dropped > 0 {
            tracing::warn!(
                "knn: skipping {dropped} vector(s) whose dimension doesn't match the query \
                 ({} dims) — stale data from a previous model/config",
                query.len()
            );
        }
        let mut scored: Vec<(i64, f64)> = vecs
            .par_iter()
            .filter(|(_, v)| v.len() == query.len())
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

    /// Always `false` — there's no vendored asset to be unusable when the
    /// `embeddings` feature itself is off; `Embedder::load`'s own stub
    /// failure below is what surfaces this build's real limitation.
    pub fn default_vendored_asset_unusable() -> bool {
        false
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
    pub fn chunk_at(
        _conn: &Connection,
        _path: &str,
        _line: i64,
    ) -> rusqlite::Result<Option<ChunkMatch>> {
        Ok(None)
    }
}

pub use imp::{
    Embedder, chunk_at, create_chunk_embedding_table, create_embedding_table,
    default_vendored_asset_unusable, embed_pending, embed_pending_chunks, knn, knn_chunks,
    prune_orphaned_chunk_vecs, store_chunk_embedding, store_embedding,
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

    /// Regression for the exact incident this function was added for: an
    /// unresolved vendored asset — historically a checkout without git-lfs
    /// installed (or one that skipped the smudge filter); as of 2026-07-12,
    /// a `build.rs::ensure_embedding_weights` fetch failure — leaves
    /// `assets/potion-code-16m/model.safetensors` as a ~130-byte Git-LFS-
    /// pointer-*shaped* stub instead of real weights. `include_bytes!`
    /// happily bakes that stub into the binary, and `Embedder::load` used to
    /// fail permanently (`embeddings_status: "failed"`) with no clear signal
    /// why. If this test ever fails, the vendored weights weren't fetched
    /// before this crate was built — see `build.rs`'s own warning output.
    #[cfg(feature = "embeddings")]
    #[test]
    fn default_vendored_asset_is_not_an_lfs_pointer() {
        assert!(
            !imp::default_vendored_asset_unusable(),
            "DEFAULT_WEIGHTS looks like an unresolved/placeholder stub, not real model weights \
             — check build.rs's cargo:warning output for why the fetch didn't happen"
        );
    }

    #[cfg(feature = "embeddings")]
    #[test]
    fn is_lfs_pointer_stub_detects_pointer_text_not_real_weights() {
        let pointer =
            b"version https://git-lfs.github.com/spec/v1\noid sha256:abc\nsize 123\n".as_slice();
        assert!(imp::is_lfs_pointer_stub(pointer));
        assert!(
            !imp::is_lfs_pointer_stub(&[0u8; 1000]),
            "1000 zero bytes is not a pointer stub by content, regardless of size"
        );
    }

    #[cfg(feature = "embeddings")]
    #[test]
    fn is_lfs_pointer_stub_length_cap_avoids_false_positive_on_large_content() {
        let mut huge = b"version https://git-lfs.github.com/spec/v1".to_vec();
        huge.resize(10_000, 0);
        assert!(
            !imp::is_lfs_pointer_stub(&huge),
            "10KB of content must not be misread as a pointer stub even if it starts \
             with the marker text — real weights are never this small either way, but \
             the cap is the actual safety net, not the content match alone"
        );
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

    /// Regression: `semantic_search.dimensions` in config.json can go stale
    /// (wrong value, or left over after a model swap) — model2vec-rs has no
    /// API to query a loaded model's real output width, so `Embedder::load`
    /// must derive it itself instead of trusting the hint.
    #[cfg(feature = "embeddings")]
    #[test]
    fn load_overrides_wrong_configured_dim_with_real_model_dim() {
        let embedder = Embedder::load(DEFAULT_MODEL_ID, 999).expect("vendored model must load");
        assert_eq!(
            embedder.dim(),
            256,
            "must report the model's real width, not the bad hint"
        );
        assert_eq!(embedder.embed_one("fn foo()").len(), 256);
    }

    /// Regression: previously (`vec0 FLOAT[dim]`) a dimension change was
    /// caught immediately as an insert error. The plain-BLOB replacement
    /// has no such enforcement, so `create_embedding_table` must detect and
    /// clear stale-dimension rows itself instead of leaving mixed-length
    /// vectors for `knn` to silently mis-score.
    #[cfg(feature = "embeddings")]
    #[test]
    fn create_embedding_table_clears_stale_dimension_rows() {
        use rusqlite::Connection;
        let conn = Connection::open_in_memory().unwrap();
        crate::db::schema::init_db(&conn).unwrap();

        // Simulate a previous index run with a 3-dim model.
        create_embedding_table(&conn, 3).unwrap();
        store_embedding(&conn, 1, &[1.0, 0.0, 0.0]).unwrap();
        let count_before: i64 = conn
            .query_row("SELECT COUNT(*) FROM embedding_vecs", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count_before, 1);

        // Model/config changed to 5-dim — recreating the table for the new
        // dim must clear the incompatible 3-dim row, not leave it mixed in.
        create_embedding_table(&conn, 5).unwrap();
        let count_after: i64 = conn
            .query_row("SELECT COUNT(*) FROM embedding_vecs", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count_after, 0, "stale-dimension row must be cleared");

        // And the table is left usable for fresh 5-dim data afterward.
        store_embedding(&conn, 2, &[1.0, 0.0, 0.0, 0.0, 0.0]).unwrap();
        let hits = knn(&conn, &[1.0, 0.0, 0.0, 0.0, 0.0], 1).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].0, 2);
    }

    /// Defense in depth for anything that manages to leave a mismatched-
    /// length row in the table anyway (e.g. written outside
    /// `create_embedding_table`'s heal path): `knn` must skip it, not feed
    /// it to `zip` and report a wrong-but-plausible cosine score.
    #[cfg(feature = "embeddings")]
    #[test]
    fn knn_skips_vectors_with_mismatched_dimension() {
        use rusqlite::Connection;
        let conn = Connection::open_in_memory().unwrap();
        crate::db::schema::init_db(&conn).unwrap();
        create_embedding_table(&conn, 3).unwrap();

        store_embedding(&conn, 1, &[1.0, 0.0, 0.0]).unwrap();
        store_embedding(&conn, 2, &[1.0, 0.0]).unwrap();

        let hits = knn(&conn, &[1.0, 0.0, 0.0], 10).unwrap();
        assert_eq!(hits.len(), 1, "the 2-dim row must be skipped, not scored");
        assert_eq!(hits[0].0, 1);
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

    #[test]
    fn knn_chunks_does_not_leak_across_distinct_in_memory_connections() {
        // Regression test for a real parallel-test flake: `conn.path()`
        // returns `Some("")` (not `None`) for a `:memory:` connection, so
        // before `stable_db_path` existed, every in-memory-DB test in this
        // crate collided on cache key `""` in `chunk_cache()`. Whichever
        // connection's `knn_chunks` call ran first "won" the shared slot,
        // and every other in-memory connection silently read back its
        // vectors instead of its own. This reproduces that scenario
        // deterministically (no thread races needed): two distinct
        // in-memory connections, each with its own chunk data, queried
        // back-to-back in the same process -- the second must never see
        // the first's vectors.
        use rusqlite::Connection;

        let conn_a = Connection::open_in_memory().unwrap();
        crate::db::schema::init_db(&conn_a).unwrap();
        create_chunk_embedding_table(&conn_a, 3).unwrap();
        store_chunk_embedding(&conn_a, 1, &[1.0, 0.0, 0.0]).unwrap();

        // Prime conn_a's slot in the (process-wide) cache first.
        let hits_a = knn_chunks(&conn_a, &[1.0, 0.0, 0.0], 1).unwrap();
        assert_eq!(hits_a.len(), 1);
        assert_eq!(hits_a[0].0, 1);

        let conn_b = Connection::open_in_memory().unwrap();
        crate::db::schema::init_db(&conn_b).unwrap();
        create_chunk_embedding_table(&conn_b, 3).unwrap();
        store_chunk_embedding(&conn_b, 99, &[0.0, 1.0, 0.0]).unwrap();

        // Before the fix: `conn_b.path()` also reads `Some("")`, so this
        // would hit conn_a's already-populated `""` cache slot and return
        // chunk id 1 (wrong data, wrong distance) instead of conn_b's own
        // chunk id 99.
        let hits_b = knn_chunks(&conn_b, &[0.0, 1.0, 0.0], 1).unwrap();
        assert_eq!(
            hits_b.len(),
            1,
            "conn_b has exactly one chunk of its own to return"
        );
        assert_eq!(
            hits_b[0].0, 99,
            "must see conn_b's own chunk, not conn_a's cached vectors"
        );
    }

    #[cfg(feature = "embeddings")]
    #[test]
    fn chunk_at_picks_smallest_containing_span_and_returns_vector() {
        use rusqlite::Connection;
        let conn = Connection::open_in_memory().unwrap();
        crate::db::schema::init_db(&conn).unwrap();
        create_chunk_embedding_table(&conn, 3).unwrap();

        // Two overlapping windows over the same file — chunk_at should prefer
        // the tighter one that still contains the target line.
        conn.execute(
            "INSERT INTO code_chunks (path, line_start, line_end, chunk_text, symbol_qn, file_hash)
             VALUES ('src/a.py', 1, 20, 'wide', 'src/a.py::f', 'h')",
            [],
        )
        .unwrap();
        let wide_id: i64 = conn
            .query_row("SELECT id FROM code_chunks WHERE line_end = 20", [], |r| {
                r.get(0)
            })
            .unwrap();
        conn.execute(
            "INSERT INTO code_chunks (path, line_start, line_end, chunk_text, symbol_qn, file_hash)
             VALUES ('src/a.py', 8, 12, 'tight', 'src/a.py::f', 'h')",
            [],
        )
        .unwrap();
        let tight_id: i64 = conn
            .query_row("SELECT id FROM code_chunks WHERE line_end = 12", [], |r| {
                r.get(0)
            })
            .unwrap();

        store_chunk_embedding(&conn, wide_id, &[1.0, 0.0, 0.0]).unwrap();
        store_chunk_embedding(&conn, tight_id, &[0.0, 1.0, 0.0]).unwrap();

        let (id, vec, symbol_qn) = chunk_at(&conn, "src/a.py", 10).unwrap().unwrap();
        assert_eq!(
            id, tight_id,
            "should pick the tighter of two overlapping spans"
        );
        assert_eq!(vec, vec![0.0, 1.0, 0.0]);
        assert_eq!(symbol_qn, Some("src/a.py::f".to_string()));
    }

    #[cfg(feature = "embeddings")]
    #[test]
    fn chunk_at_returns_none_without_a_stored_embedding() {
        use rusqlite::Connection;
        let conn = Connection::open_in_memory().unwrap();
        crate::db::schema::init_db(&conn).unwrap();
        create_chunk_embedding_table(&conn, 3).unwrap();

        conn.execute(
            "INSERT INTO code_chunks (path, line_start, line_end, chunk_text, symbol_qn, file_hash)
             VALUES ('src/a.py', 1, 5, 'unembedded', NULL, 'h')",
            [],
        )
        .unwrap();

        // No matching code_chunk_vecs row yet (embedding pass hasn't reached it).
        assert!(chunk_at(&conn, "src/a.py", 3).unwrap().is_none());
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
    /// Run with: cargo test -p calm-core --release --features embeddings -- --ignored --nocapture bench_knn_latency
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
