//! T3 Verified Index Bundles (2026-08-07 roadmap,
//! docs/plans/2026-08-07-pecorino-adoption-roadmap.md §TIER 3): export a
//! consistent, checksummed snapshot of `index.db` + a manifest describing
//! what produced it, so a large repo can be onboarded without a cold
//! reindex, CI can seed from a prebuilt index, and a future federation
//! feature (T4) has a verified artifact to start from. `state.db` (project
//! memory, edit-transaction journal, audit ledger, HMAC keys) is NEVER
//! included -- it is durable per-machine state, not rebuildable index
//! data, and shipping it would leak secrets/keys across machines.
//!
//! # Design decisions
//!
//! Verified against a critique review of the original roadmap sketch
//! before implementing (not built from the sketch verbatim):
//!
//! **Snapshot consistency via `VACUUM INTO`, never a raw file copy.** The
//! roadmap sketch's `manifest.json + index.db` framing under-specified
//! HOW `index.db` gets read: naively `tar`-ing the live WAL file while a
//! writer/daemon is mid-transaction can capture an inconsistent, possibly
//! corrupt snapshot. `VACUUM INTO` is SQLite's own supported mechanism for
//! a consistent point-in-time copy, safe to run from a read-only
//! connection concurrently with WAL readers/writers on the source --
//! see `export_bundle`.
//!
//! **Verify-then-activate, never trust-on-extract.** `import_bundle`
//! checks, in order: schema_version compatibility, `index_db_sha256`
//! against the actually-extracted bytes, and `PRAGMA integrity_check` on
//! the extracted database -- all three must pass before the file is ever
//! renamed over the live `db_path`. A bundle failing any check is
//! rejected with no filesystem change.
//!
//! **`.tar.gz`, not the roadmap sketch's `.tar.zst`.** See the `tar`/
//! `flate2` dependency comment in Cargo.toml -- `zstd`'s Rust binding
//! needs a C compiler, the exact problem class this workspace has
//! repeatedly hit and removed on musl cross-compiles.
//!
//! **Import-as-seed triggers on ANY git-commit mismatch, not just "tree
//! doesn't match".** If the bundle's `git_commit` differs from the
//! importing repo's current HEAD (or either side has none), the bundle is
//! still activated -- but `ImportReport::force_full_reindex` is set,
//! telling the caller to run a full reindex afterward rather than trust
//! incremental reconciliation, which only re-touches files whose content
//! actually changed and so could never notice "the code that INTERPRETS
//! an unchanged file changed" (a binary-version mismatch). This is a
//! coarser response than the critique's own suggested "invalidate that
//! analyzer globally" -- CALM has no per-analyzer fingerprint
//! infrastructure yet (see `indexer::semantic_facts`'s own doc comment on
//! why a `SemanticFactExtractor` trait/enum abstraction wasn't built
//! either), so `calm_version` is used as the one coarse, honest signal
//! available: ANY version mismatch forces a full reindex rather than
//! silently risking stale extraction.
//!
//! **A live daemon does not hot-reload an imported bundle.** `rename()`
//! atomically swaps the directory entry, but a process that already has
//! the old `index.db` open keeps reading the OLD inode until it reopens --
//! same operational reality already documented elsewhere in this
//! codebase for any out-of-band file change the watcher doesn't cover.
//! Restarting/reconnecting the daemon after an import is the caller's
//! responsibility, not automated here.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::Command;

use rusqlite::{Connection, OpenFlags};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::config::Config;
use crate::db::schema::INDEX_DB_SCHEMA_VERSION;

pub const MANIFEST_FILE_NAME: &str = "manifest.json";
pub const INDEX_DB_FILE_NAME: &str = "index.db";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BundleManifest {
    pub calm_version: String,
    pub schema_version: i64,
    /// Seconds since UNIX epoch -- same convention `file_index.last_indexed`/
    /// `external_proofs.observed_at` already use, not an ISO8601 string, to
    /// avoid a date-formatting dependency for a value nothing here parses
    /// back out except for display.
    pub created_at_epoch: f64,
    pub git_commit: Option<String>,
    /// `None` when `git_commit` is also `None` (not a git repo, or `git`
    /// unavailable) -- never fabricated as `false`.
    pub git_tree_dirty: Option<bool>,
    pub languages: Vec<String>,
    /// `sha256(sorted languages ++ sorted ignore rules)` -- catches "bundle
    /// built with different `ignore`/`languages` config than the importing
    /// repo's current config.json", the case the critique flagged (a
    /// bundle built with `ignore = ["generated/**"]` imported where local
    /// config has no such rule). Does not fold in every `Config` field
    /// (e.g. hub thresholds, search weights) -- only the two that change
    /// WHICH files/languages get indexed at all, which is what a stale
    /// bundle's file coverage actually depends on.
    pub config_fingerprint: String,
    pub index_db_sha256: String,
    pub symbol_count: i64,
    pub file_count: i64,
    pub embeddings_enabled: bool,
    pub embedding_model_id: Option<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum BundleError {
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("sqlite error: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("bundle archive is missing {0}")]
    MissingEntry(&'static str),
    #[error(
        "index_db_sha256 mismatch: manifest says {expected}, extracted file hashes to {actual} -- bundle is corrupt or was tampered with"
    )]
    ChecksumMismatch { expected: String, actual: String },
    #[error("SQLite integrity_check failed on the extracted database: {0}")]
    IntegrityCheckFailed(String),
    #[error(
        "bundle schema_version {bundle} is newer than this binary supports ({binary}) -- upgrade CALM before importing this bundle"
    )]
    SchemaTooNew { bundle: i64, binary: i64 },
}

/// Report of what `import_bundle` did and what the caller should do next.
/// Nothing here is silently decided FOR the caller -- `force_full_reindex`
/// is a recommendation surfaced explicitly, not acted on inside this
/// function (this module does not itself know how to run the indexing
/// pipeline against a live daemon's connection).
#[derive(Debug, Clone)]
pub struct ImportReport {
    pub manifest: BundleManifest,
    pub commit_matches: bool,
    pub config_matches: bool,
    /// `true` when EITHER the git commit doesn't match OR `calm_version`
    /// differs from this binary's own version -- see the module doc
    /// comment's "Import-as-seed" section for why both trigger this.
    pub force_full_reindex: bool,
    pub activated_path: PathBuf,
}

/// Reads just `manifest.json` out of an archive without touching `db_path`
/// at all -- lets a caller preview a bundle (e.g. print a confirmation
/// prompt) before committing to a real `import_bundle` call.
pub fn inspect_bundle(archive_path: &Path) -> Result<BundleManifest, BundleError> {
    let file = std::fs::File::open(archive_path)?;
    let decoder = flate2::read::GzDecoder::new(file);
    let mut archive = tar::Archive::new(decoder);
    for entry in archive.entries()? {
        let mut entry = entry?;
        if entry.path()?.as_os_str() == MANIFEST_FILE_NAME {
            let mut buf = String::new();
            entry.read_to_string(&mut buf)?;
            return Ok(serde_json::from_str(&buf)?);
        }
    }
    Err(BundleError::MissingEntry(MANIFEST_FILE_NAME))
}

/// Exports a verified snapshot of `db_path` + a manifest into a fresh
/// `.tar.gz` at `output_path`. Read-only against `db_path` throughout --
/// this function can never write to the live database.
pub fn export_bundle(
    db_path: &Path,
    project_root: &Path,
    config: &Config,
    output_path: &Path,
) -> Result<BundleManifest, BundleError> {
    let work_dir = tempfile::tempdir()?;
    let snapshot_path = work_dir.path().join(INDEX_DB_FILE_NAME);

    {
        let src = Connection::open_with_flags(db_path, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
        src.execute("VACUUM INTO ?1", [snapshot_path.to_string_lossy().as_ref()])?;
    }

    // Re-open the FRESH snapshot for integrity_check and stats -- verifies
    // the exact bytes about to ship, not the live db that could keep
    // changing underneath this call.
    let (symbol_count, file_count) = {
        let snap = Connection::open_with_flags(&snapshot_path, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
        check_integrity(&snap)?;
        let symbol_count: i64 = snap.query_row("SELECT COUNT(*) FROM symbols", [], |r| r.get(0))?;
        let file_count: i64 =
            snap.query_row("SELECT COUNT(*) FROM file_index", [], |r| r.get(0))?;
        (symbol_count, file_count)
    };

    let index_db_sha256 = sha256_file(&snapshot_path)?;
    let (git_commit, git_tree_dirty) = git_head_status(project_root);
    let mut languages = config.languages.clone();
    languages.sort();

    let manifest = BundleManifest {
        calm_version: env!("CARGO_PKG_VERSION").to_string(),
        schema_version: INDEX_DB_SCHEMA_VERSION,
        created_at_epoch: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs_f64())
            .unwrap_or(0.0),
        git_commit,
        git_tree_dirty,
        languages,
        config_fingerprint: config_fingerprint(config),
        index_db_sha256,
        symbol_count,
        file_count,
        embeddings_enabled: config.semantic_search.enabled,
        embedding_model_id: config
            .semantic_search
            .enabled
            .then(|| config.semantic_search.model.clone()),
    };
    let manifest_json = serde_json::to_vec_pretty(&manifest)?;

    if let Some(parent) = output_path.parent()
        && !parent.as_os_str().is_empty()
    {
        std::fs::create_dir_all(parent)?;
    }
    let out_file = std::fs::File::create(output_path)?;
    let encoder = flate2::write::GzEncoder::new(out_file, flate2::Compression::default());
    let mut tar_builder = tar::Builder::new(encoder);
    tar_builder.append_path_with_name(&snapshot_path, INDEX_DB_FILE_NAME)?;
    let mut manifest_header = tar::Header::new_gnu();
    manifest_header.set_size(manifest_json.len() as u64);
    manifest_header.set_mode(0o644);
    manifest_header.set_cksum();
    tar_builder.append_data(
        &mut manifest_header,
        MANIFEST_FILE_NAME,
        manifest_json.as_slice(),
    )?;
    tar_builder.into_inner()?.finish()?.flush()?;

    Ok(manifest)
}

/// Extracts, verifies, then atomically activates a bundle over `db_path`.
/// Every check in the module doc comment's "Verify-then-activate" section
/// runs, in order, before any change to `db_path` -- a failing bundle
/// leaves the live database completely untouched.
pub fn import_bundle(
    archive_path: &Path,
    project_root: &Path,
    db_path: &Path,
    config: &Config,
) -> Result<ImportReport, BundleError> {
    // Extraction happens INSIDE db_path's own parent directory so the final
    // activation is a same-filesystem `rename()` -- atomic on POSIX. A
    // cross-filesystem rename (e.g. extracting under system /tmp when
    // db_path lives on a different mount) would either fail outright or
    // silently degrade to non-atomic copy+delete.
    let db_parent = db_path.parent().unwrap_or_else(|| Path::new("."));
    std::fs::create_dir_all(db_parent)?;
    let work_dir = tempfile::Builder::new()
        .prefix(".calm-bundle-import-")
        .tempdir_in(db_parent)?;

    let mut manifest: Option<BundleManifest> = None;
    let extracted_db_path = work_dir.path().join(INDEX_DB_FILE_NAME);
    {
        let file = std::fs::File::open(archive_path)?;
        let decoder = flate2::read::GzDecoder::new(file);
        let mut archive = tar::Archive::new(decoder);
        for entry in archive.entries()? {
            let mut entry = entry?;
            let name = entry.path()?.to_path_buf();
            if name.as_os_str() == MANIFEST_FILE_NAME {
                let mut buf = String::new();
                entry.read_to_string(&mut buf)?;
                manifest = Some(serde_json::from_str(&buf)?);
            } else if name.as_os_str() == INDEX_DB_FILE_NAME {
                entry.unpack(&extracted_db_path)?;
            }
        }
    }
    let manifest = manifest.ok_or(BundleError::MissingEntry(MANIFEST_FILE_NAME))?;
    if !extracted_db_path.exists() {
        return Err(BundleError::MissingEntry(INDEX_DB_FILE_NAME));
    }

    if manifest.schema_version > INDEX_DB_SCHEMA_VERSION {
        return Err(BundleError::SchemaTooNew {
            bundle: manifest.schema_version,
            binary: INDEX_DB_SCHEMA_VERSION,
        });
    }

    let actual_sha256 = sha256_file(&extracted_db_path)?;
    if actual_sha256 != manifest.index_db_sha256 {
        return Err(BundleError::ChecksumMismatch {
            expected: manifest.index_db_sha256.clone(),
            actual: actual_sha256,
        });
    }

    {
        let snap =
            Connection::open_with_flags(&extracted_db_path, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
        check_integrity(&snap)?;
    }

    let (current_commit, _) = git_head_status(project_root);
    let commit_matches = match (&manifest.git_commit, &current_commit) {
        (Some(a), Some(b)) => a == b,
        _ => false,
    };
    let config_matches = manifest.config_fingerprint == config_fingerprint(config);
    let version_matches = manifest.calm_version == env!("CARGO_PKG_VERSION");
    let force_full_reindex = !commit_matches || !version_matches;

    // Drop any stale WAL/SHM sidecars belonging to the file about to be
    // replaced -- VACUUM INTO's own output is always a single plain file
    // with none of its own, but a lingering `-wal`/`-shm` next to the OLD
    // `db_path` would otherwise make the next connection try to replay a
    // WAL against completely unrelated new page content. Best-effort:
    // absence is the common/expected case, not an error.
    let _ = std::fs::remove_file(sidecar_path(db_path, "-wal"));
    let _ = std::fs::remove_file(sidecar_path(db_path, "-shm"));

    std::fs::rename(&extracted_db_path, db_path)?;

    Ok(ImportReport {
        manifest,
        commit_matches,
        config_matches,
        force_full_reindex,
        activated_path: db_path.to_path_buf(),
    })
}

fn sidecar_path(db_path: &Path, suffix: &str) -> PathBuf {
    let mut s = db_path.as_os_str().to_os_string();
    s.push(suffix);
    PathBuf::from(s)
}

fn check_integrity(conn: &Connection) -> Result<(), BundleError> {
    // A file that isn't a SQLite database at all (garbage bytes, truncated)
    // fails at the query itself (`SqliteFailure`/`NotADatabase`), not by
    // returning a non-"ok" result row -- both are folded into the SAME
    // `IntegrityCheckFailed` variant so a caller has exactly one error kind
    // to check for "this bundle's database is unusable", regardless of
    // which of the two ways SQLite reports it.
    match conn.query_row("PRAGMA integrity_check", [], |r| r.get::<_, String>(0)) {
        Ok(result) if result == "ok" => Ok(()),
        Ok(result) => Err(BundleError::IntegrityCheckFailed(result)),
        Err(e) => Err(BundleError::IntegrityCheckFailed(e.to_string())),
    }
}

fn sha256_file(path: &Path) -> std::io::Result<String> {
    let mut file = std::fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buf = [0u8; 65536];
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

fn config_fingerprint(config: &Config) -> String {
    let mut languages = config.languages.clone();
    languages.sort();
    let mut ignore = config.ignore.clone();
    ignore.sort();
    let mut hasher = Sha256::new();
    hasher.update(languages.join(",").as_bytes());
    hasher.update(b"|");
    hasher.update(ignore.join(",").as_bytes());
    format!("{:x}", hasher.finalize())
}

/// `(HEAD commit sha, working tree has uncommitted changes)`. `(None, None)`
/// when `project_root` isn't a git repo or `git` isn't on PATH -- never
/// fabricated, matching every other absent-fact contract in this codebase.
fn git_head_status(project_root: &Path) -> (Option<String>, Option<bool>) {
    let commit = Command::new("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(project_root)
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string());
    let Some(commit) = commit else {
        return (None, None);
    };
    let dirty = Command::new("git")
        .args(["status", "--porcelain"])
        .current_dir(project_root)
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| !o.stdout.is_empty());
    (Some(commit), dirty)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::schema::init_db;

    fn seed_index_db(path: &Path) {
        let conn = Connection::open(path).unwrap();
        init_db(&conn).unwrap();
        conn.execute(
            "INSERT INTO symbols (qualified_name, name, kind, language, path, line_start, line_end) \
             VALUES ('a.py::f', 'f', 'function', 'python', 'a.py', 1, 2)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO file_index (path, hash, language, last_indexed) \
             VALUES ('a.py', 'h', 'python', 1.0)",
            [],
        )
        .unwrap();
    }

    #[test]
    fn export_then_import_round_trips_symbol_data() {
        let src_dir = tempfile::tempdir().unwrap();
        let db_path = src_dir.path().join("index.db");
        seed_index_db(&db_path);

        let config = Config::default();
        let archive_path = src_dir.path().join("bundle.tar.gz");
        let manifest = export_bundle(&db_path, src_dir.path(), &config, &archive_path).unwrap();
        assert_eq!(manifest.symbol_count, 1);
        assert_eq!(manifest.file_count, 1);
        assert!(archive_path.exists());

        let dest_dir = tempfile::tempdir().unwrap();
        let dest_db_path = dest_dir.path().join("index.db");
        let report = import_bundle(&archive_path, dest_dir.path(), &dest_db_path, &config).unwrap();
        assert_eq!(report.manifest.symbol_count, 1);
        assert!(dest_db_path.exists());

        let conn =
            Connection::open_with_flags(&dest_db_path, OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM symbols", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 1);
    }

    #[test]
    fn import_rejects_corrupted_archive_checksum() {
        let src_dir = tempfile::tempdir().unwrap();
        let db_path = src_dir.path().join("index.db");
        seed_index_db(&db_path);
        let config = Config::default();
        let archive_path = src_dir.path().join("bundle.tar.gz");
        export_bundle(&db_path, src_dir.path(), &config, &archive_path).unwrap();

        // Tamper with the manifest's checksum by re-writing a bad one --
        // rebuild the archive with a corrupted manifest so import must
        // reject it rather than activate mismatched bytes.
        let mut manifest = inspect_bundle(&archive_path).unwrap();
        manifest.index_db_sha256 = "0".repeat(64);
        let tampered_path = src_dir.path().join("tampered.tar.gz");
        {
            let out_file = std::fs::File::create(&tampered_path).unwrap();
            let encoder = flate2::write::GzEncoder::new(out_file, flate2::Compression::default());
            let mut builder = tar::Builder::new(encoder);
            // Re-extract the real index.db bytes from the original archive.
            let file = std::fs::File::open(&archive_path).unwrap();
            let decoder = flate2::read::GzDecoder::new(file);
            let mut archive = tar::Archive::new(decoder);
            let mut db_bytes = Vec::new();
            for entry in archive.entries().unwrap() {
                let mut entry = entry.unwrap();
                if entry.path().unwrap().as_os_str() == INDEX_DB_FILE_NAME {
                    entry.read_to_end(&mut db_bytes).unwrap();
                }
            }
            let mut db_header = tar::Header::new_gnu();
            db_header.set_size(db_bytes.len() as u64);
            db_header.set_mode(0o644);
            db_header.set_cksum();
            builder
                .append_data(&mut db_header, INDEX_DB_FILE_NAME, db_bytes.as_slice())
                .unwrap();
            let manifest_json = serde_json::to_vec_pretty(&manifest).unwrap();
            let mut manifest_header = tar::Header::new_gnu();
            manifest_header.set_size(manifest_json.len() as u64);
            manifest_header.set_mode(0o644);
            manifest_header.set_cksum();
            builder
                .append_data(
                    &mut manifest_header,
                    MANIFEST_FILE_NAME,
                    manifest_json.as_slice(),
                )
                .unwrap();
            builder
                .into_inner()
                .unwrap()
                .finish()
                .unwrap()
                .flush()
                .unwrap();
        }

        let dest_dir = tempfile::tempdir().unwrap();
        let dest_db_path = dest_dir.path().join("index.db");
        let err =
            import_bundle(&tampered_path, dest_dir.path(), &dest_db_path, &config).unwrap_err();
        assert!(
            matches!(err, BundleError::ChecksumMismatch { .. }),
            "{err:?}"
        );
        assert!(
            !dest_db_path.exists(),
            "a failed import must not touch the destination at all"
        );
    }

    #[test]
    fn import_rejects_schema_version_newer_than_binary() {
        let src_dir = tempfile::tempdir().unwrap();
        let db_path = src_dir.path().join("index.db");
        seed_index_db(&db_path);
        let config = Config::default();
        let archive_path = src_dir.path().join("bundle.tar.gz");
        export_bundle(&db_path, src_dir.path(), &config, &archive_path).unwrap();

        let mut manifest = inspect_bundle(&archive_path).unwrap();
        manifest.schema_version = INDEX_DB_SCHEMA_VERSION + 1;
        // Recompute nothing else -- schema_version is checked before the
        // checksum, so the (now-stale) index_db_sha256 never gets reached.
        let bumped_path = src_dir.path().join("bumped.tar.gz");
        {
            let out_file = std::fs::File::create(&bumped_path).unwrap();
            let encoder = flate2::write::GzEncoder::new(out_file, flate2::Compression::default());
            let mut builder = tar::Builder::new(encoder);
            let db_bytes = std::fs::read(work_dir_db_for_test(&archive_path)).unwrap();
            let mut db_header = tar::Header::new_gnu();
            db_header.set_size(db_bytes.len() as u64);
            db_header.set_mode(0o644);
            db_header.set_cksum();
            builder
                .append_data(&mut db_header, INDEX_DB_FILE_NAME, db_bytes.as_slice())
                .unwrap();
            let manifest_json = serde_json::to_vec_pretty(&manifest).unwrap();
            let mut manifest_header = tar::Header::new_gnu();
            manifest_header.set_size(manifest_json.len() as u64);
            manifest_header.set_mode(0o644);
            manifest_header.set_cksum();
            builder
                .append_data(
                    &mut manifest_header,
                    MANIFEST_FILE_NAME,
                    manifest_json.as_slice(),
                )
                .unwrap();
            builder
                .into_inner()
                .unwrap()
                .finish()
                .unwrap()
                .flush()
                .unwrap();
        }

        let dest_dir = tempfile::tempdir().unwrap();
        let dest_db_path = dest_dir.path().join("index.db");
        let err = import_bundle(&bumped_path, dest_dir.path(), &dest_db_path, &config).unwrap_err();
        assert!(matches!(err, BundleError::SchemaTooNew { .. }), "{err:?}");
    }

    // Re-extracts index.db bytes from an already-exported archive, for
    // tests that need to rebuild a tampered archive around the SAME real
    // db bytes without re-running a fresh export.
    fn work_dir_db_for_test(archive_path: &Path) -> PathBuf {
        let extract_dir = tempfile::tempdir().unwrap();
        let out_path = extract_dir.path().join(INDEX_DB_FILE_NAME);
        let file = std::fs::File::open(archive_path).unwrap();
        let decoder = flate2::read::GzDecoder::new(file);
        let mut archive = tar::Archive::new(decoder);
        for entry in archive.entries().unwrap() {
            let mut entry = entry.unwrap();
            if entry.path().unwrap().as_os_str() == INDEX_DB_FILE_NAME {
                entry.unpack(&out_path).unwrap();
            }
        }
        std::mem::forget(extract_dir);
        out_path
    }

    #[test]
    fn import_never_activates_when_integrity_check_would_fail() {
        // A archive whose index.db entry is truncated garbage must fail the
        // sha256 check (computed over the real bytes) before ever reaching
        // integrity_check -- this test locks in that checksum verification
        // itself is sufficient defense, since a hand-crafted archive with a
        // MATCHING (recomputed) checksum but corrupt SQLite content is what
        // integrity_check exists for specifically.
        let src_dir = tempfile::tempdir().unwrap();
        let garbage_db = src_dir.path().join(INDEX_DB_FILE_NAME);
        std::fs::write(&garbage_db, b"not a real sqlite file").unwrap();
        let config = Config::default();

        let manifest = BundleManifest {
            calm_version: env!("CARGO_PKG_VERSION").to_string(),
            schema_version: INDEX_DB_SCHEMA_VERSION,
            created_at_epoch: 0.0,
            git_commit: None,
            git_tree_dirty: None,
            languages: vec![],
            config_fingerprint: config_fingerprint(&config),
            index_db_sha256: sha256_file(&garbage_db).unwrap(),
            symbol_count: 0,
            file_count: 0,
            embeddings_enabled: false,
            embedding_model_id: None,
        };
        let archive_path = src_dir.path().join("garbage.tar.gz");
        {
            let out_file = std::fs::File::create(&archive_path).unwrap();
            let encoder = flate2::write::GzEncoder::new(out_file, flate2::Compression::default());
            let mut builder = tar::Builder::new(encoder);
            builder
                .append_path_with_name(&garbage_db, INDEX_DB_FILE_NAME)
                .unwrap();
            let manifest_json = serde_json::to_vec_pretty(&manifest).unwrap();
            let mut manifest_header = tar::Header::new_gnu();
            manifest_header.set_size(manifest_json.len() as u64);
            manifest_header.set_mode(0o644);
            manifest_header.set_cksum();
            builder
                .append_data(
                    &mut manifest_header,
                    MANIFEST_FILE_NAME,
                    manifest_json.as_slice(),
                )
                .unwrap();
            builder
                .into_inner()
                .unwrap()
                .finish()
                .unwrap()
                .flush()
                .unwrap();
        }

        let dest_dir = tempfile::tempdir().unwrap();
        let dest_db_path = dest_dir.path().join("index.db");
        let err =
            import_bundle(&archive_path, dest_dir.path(), &dest_db_path, &config).unwrap_err();
        assert!(
            matches!(err, BundleError::IntegrityCheckFailed(_)),
            "{err:?}"
        );
        assert!(!dest_db_path.exists());
    }

    #[test]
    fn import_activates_and_reports_force_full_reindex_when_commit_differs() {
        let src_dir = tempfile::tempdir().unwrap();
        let db_path = src_dir.path().join("index.db");
        seed_index_db(&db_path);
        let config = Config::default();
        let archive_path = src_dir.path().join("bundle.tar.gz");
        // src_dir is not a git repo -- manifest.git_commit will be None.
        export_bundle(&db_path, src_dir.path(), &config, &archive_path).unwrap();

        let dest_dir = tempfile::tempdir().unwrap();
        let dest_db_path = dest_dir.path().join("index.db");
        let report = import_bundle(&archive_path, dest_dir.path(), &dest_db_path, &config).unwrap();
        // Both sides have no git commit -> commit_matches is conservatively
        // false (never assume equality from two absences), which forces a
        // full reindex recommendation.
        assert!(!report.commit_matches);
        assert!(report.force_full_reindex);
        assert!(report.config_matches);
    }

    #[test]
    fn import_removes_stale_wal_sidecars_next_to_the_activated_path() {
        let src_dir = tempfile::tempdir().unwrap();
        let db_path = src_dir.path().join("index.db");
        seed_index_db(&db_path);
        let config = Config::default();
        let archive_path = src_dir.path().join("bundle.tar.gz");
        export_bundle(&db_path, src_dir.path(), &config, &archive_path).unwrap();

        let dest_dir = tempfile::tempdir().unwrap();
        let dest_db_path = dest_dir.path().join("index.db");
        // Simulate a prior live WAL-mode database's leftover sidecars at
        // the activation target.
        std::fs::write(sidecar_path(&dest_db_path, "-wal"), b"stale wal").unwrap();
        std::fs::write(sidecar_path(&dest_db_path, "-shm"), b"stale shm").unwrap();

        import_bundle(&archive_path, dest_dir.path(), &dest_db_path, &config).unwrap();

        assert!(!sidecar_path(&dest_db_path, "-wal").exists());
        assert!(!sidecar_path(&dest_db_path, "-shm").exists());
    }
}
