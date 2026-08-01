//! Refresh planning shared by file-watch and explicit refresh entry points.
//!
//! The watcher is only an accelerator: it reports filesystem observations, but
//! this module decides their safe, normalized effect on the index.  Keeping
//! that policy in `calm-core` means an explicit refresh and a watcher refresh
//! cannot silently diverge.

use std::collections::{BTreeSet, VecDeque};
use std::path::{Component, Path, PathBuf};

use crate::analysis::coverage::COVERAGE_SEARCH_PATHS;
use crate::indexer::lang_constants::{is_recognized_unparsed_extension, language_for_extension};
use crate::walk::{build_walker, is_ignored_dir_component, matches_ignore_pattern};

/// How one filesystem path affects the index.
///
/// Every path-carrying variant contains a normalized, UTF-8, root-relative
/// path using `/` separators.  A path that cannot meet that contract is
/// `Unsafe`, rather than being guessed at or joined to the project root.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChangeClass {
    Source(String),
    Context(String),
    /// A change to the indexer's own configuration can alter source discovery
    /// (not merely edge resolution), so it requires disk reconciliation.
    GlobalContext(String),
    Coverage(String),
    Ignored,
    Unsafe,
}

impl ChangeClass {
    pub fn is_source(&self) -> bool {
        matches!(self, Self::Source(_))
    }

    pub fn is_context(&self) -> bool {
        matches!(self, Self::Context(_))
    }

    pub fn is_coverage(&self) -> bool {
        matches!(self, Self::Coverage(_))
    }

    pub fn is_global_context(&self) -> bool {
        matches!(self, Self::GlobalContext(_))
    }

    pub fn is_unsafe(&self) -> bool {
        matches!(self, Self::Unsafe)
    }

    pub fn relative_path(&self) -> Option<&str> {
        match self {
            Self::Source(path)
            | Self::Context(path)
            | Self::GlobalContext(path)
            | Self::Coverage(path) => Some(path),
            Self::Ignored | Self::Unsafe => None,
        }
    }
}

/// A reason why exact-path refresh is not trustworthy enough.
///
/// Unlike an event-count threshold, each case here is an explicit loss of
/// observability.  The only correct response is reconciliation against disk.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FullReconciliationReason {
    NotifyRescan,
    WatcherStart,
    WatcherError,
    UnsafePath,
    UnsafeRename,
    ConfigurationChanged,
    PeriodicReconciliation,
}

/// How the persisted non-source input contract differs from the project now
/// visible on disk. `Unknown` is intentionally fail-closed: old databases and
/// future policy versions must earn a fresh baseline before a delta can run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IndexInputDrift {
    Clean,
    Context,
    Configuration,
    Unknown,
}

const INDEX_INPUT_STATE_POLICY_VERSION: i64 = 1;
const GLOBAL_CONFIGURATION_PATHS: [&str; 2] = ["config.json", ".calm/config.json"];

/// Normalized coalesced changes collected during one debounce interval.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ChangeSet {
    source_paths: BTreeSet<String>,
    context_paths: BTreeSet<String>,
    coverage_paths: BTreeSet<String>,
    full_reconciliation: Option<FullReconciliationReason>,
}

impl ChangeSet {
    pub fn record_class(&mut self, class: ChangeClass) {
        match class {
            ChangeClass::Source(path) => {
                self.source_paths.insert(path);
            }
            ChangeClass::Context(path) => {
                self.context_paths.insert(path);
            }
            ChangeClass::GlobalContext(_) => {
                self.require_full_reconciliation(FullReconciliationReason::ConfigurationChanged);
            }
            ChangeClass::Coverage(path) => {
                self.coverage_paths.insert(path);
            }
            ChangeClass::Ignored => {}
            ChangeClass::Unsafe => {
                self.require_full_reconciliation(FullReconciliationReason::UnsafePath)
            }
        }
    }

    pub fn record_path(&mut self, catalog: &InputCatalog, path: impl AsRef<Path>) -> ChangeClass {
        let class = catalog.classify(path);
        self.record_class(class.clone());
        class
    }

    pub fn require_full_reconciliation(&mut self, reason: FullReconciliationReason) {
        // Preserve the first concrete loss-of-observability reason.  It is
        // deterministic because watcher events are processed serially and it
        // remains useful in health/status output after the batch is consumed.
        self.full_reconciliation.get_or_insert(reason);
    }

    pub fn is_empty(&self) -> bool {
        self.source_paths.is_empty()
            && self.context_paths.is_empty()
            && self.coverage_paths.is_empty()
            && self.full_reconciliation.is_none()
    }

    pub fn source_paths(&self) -> impl Iterator<Item = &str> {
        self.source_paths.iter().map(String::as_str)
    }

    pub fn context_paths(&self) -> impl Iterator<Item = &str> {
        self.context_paths.iter().map(String::as_str)
    }

    pub fn coverage_paths(&self) -> impl Iterator<Item = &str> {
        self.coverage_paths.iter().map(String::as_str)
    }

    pub fn refresh_request(&self) -> RefreshRequest {
        if let Some(reason) = self.full_reconciliation {
            return RefreshRequest::FullReconciliation {
                reason,
                // A loss-of-observation means any coverage snapshot can be stale too:
                // a rescan must re-establish every derived input, not only paths named
                // by the incomplete event stream.
                reload_coverage: true,
            };
        }

        if self.is_empty() {
            return RefreshRequest::Noop;
        }

        RefreshRequest::Incremental {
            source_paths: self.source_paths.iter().cloned().collect(),
            context_paths: self.context_paths.iter().cloned().collect(),
            reload_coverage: !self.coverage_paths.is_empty(),
        }
    }
}

/// Work a refresh executor must perform for one coalesced [`ChangeSet`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RefreshRequest {
    Noop,
    Incremental {
        source_paths: Vec<String>,
        context_paths: Vec<String>,
        reload_coverage: bool,
    },
    FullReconciliation {
        reason: FullReconciliationReason,
        reload_coverage: bool,
    },
}

impl RefreshRequest {
    pub fn requires_graph_rebuild(&self) -> bool {
        match self {
            Self::Noop => false,
            Self::Incremental {
                source_paths,
                context_paths,
                ..
            } => !source_paths.is_empty() || !context_paths.is_empty(),
            Self::FullReconciliation { .. } => true,
        }
    }

    /// Whether completing this request can change the observed input closure.
    /// The watcher rebuilds its catalog only after these requests, keeping
    /// normal source-only saves on the cheap path without leaving a newly
    /// introduced TypeScript `extends` target invisible.
    pub fn refreshes_input_catalog(&self) -> bool {
        match self {
            Self::Noop => false,
            Self::Incremental { context_paths, .. } => !context_paths.is_empty(),
            Self::FullReconciliation { .. } => true,
        }
    }
}

/// Result of executing one [`RefreshRequest`] against the index database.
#[derive(Debug)]
pub struct RefreshOutcome {
    pub reindex_summary: Option<crate::indexer::pipeline::ReindexSummary>,
    pub graph_mode: Option<crate::indexer::pipeline::GraphMode>,
    pub graph_rebuilt: bool,
    pub full_reconciliation: Option<FullReconciliationReason>,
    pub reload_coverage: bool,
}

/// Completion state for a refresh that is allowed to observe cancellation.
/// Source-path work remains atomic once begun; full reconciliation delegates to
/// the pipeline's existing cancellation-aware tree walk.
#[derive(Debug)]
pub enum RefreshExecution {
    Completed(RefreshOutcome),
    Cancelled,
}

/// Execute the index-side portion of a normalized refresh request.
///
/// Coverage loading remains at the server boundary because it owns the
/// in-memory coverage snapshot.  All database writes remain serialized through
/// this one mutable connection, whether the trigger was a watcher, manual
/// refresh, or periodic reconciliation.
pub fn execute_refresh(
    conn: &mut rusqlite::Connection,
    project_root: &Path,
    request: &RefreshRequest,
) -> rusqlite::Result<RefreshOutcome> {
    match execute_refresh_cancellable(conn, project_root, request, &|| false)? {
        RefreshExecution::Completed(outcome) => Ok(outcome),
        RefreshExecution::Cancelled => {
            unreachable!("the non-cancellable refresh executor cannot be cancelled")
        }
    }
}

/// Cancellation-aware form of [`execute_refresh`] used by long-lived runtime
/// workers.  It shares all classification semantics with manual refreshes,
/// while preserving timely shutdown during an expensive reconciliation walk.
pub fn execute_refresh_cancellable(
    conn: &mut rusqlite::Connection,
    project_root: &Path,
    request: &RefreshRequest,
    cancelled: &dyn Fn() -> bool,
) -> rusqlite::Result<RefreshExecution> {
    if cancelled() {
        return Ok(RefreshExecution::Cancelled);
    }
    let mut outcome = RefreshOutcome {
        reindex_summary: None,
        graph_mode: None,
        graph_rebuilt: false,
        full_reconciliation: None,
        reload_coverage: false,
    };

    match request {
        RefreshRequest::Noop => {}
        RefreshRequest::Incremental {
            source_paths,
            context_paths,
            reload_coverage,
        } => {
            outcome.reload_coverage = *reload_coverage;
            if !source_paths.is_empty() {
                let summary =
                    crate::indexer::pipeline::reindex_paths(conn, project_root, source_paths)?;
                if !summary.is_noop() {
                    outcome.graph_rebuilt = true;
                    outcome.graph_mode = Some(summary.graph_mode.clone());
                }
                outcome.reindex_summary = Some(summary);
            }

            // A context input changes the interpretation of every already
            // indexed call/import edge.  Rebuild after any exact source work
            // as well, so the final committed graph never retains a
            // path-scoped incremental result under changed metadata.
            if !context_paths.is_empty() {
                outcome.graph_mode = Some(crate::indexer::pipeline::rebuild_graph_from_index(
                    conn,
                    project_root,
                )?);
                outcome.graph_rebuilt = true;
            }
        }
        RefreshRequest::FullReconciliation {
            reason,
            reload_coverage,
        } => {
            outcome.reload_coverage = *reload_coverage;
            outcome.full_reconciliation = Some(*reason);
            let current_inputs = InputCatalog::for_project(project_root);
            match index_input_drift(conn, &current_inputs)? {
                IndexInputDrift::Clean => {
                    let summary = match crate::indexer::pipeline::reindex_changed_cancellable(
                        conn,
                        project_root,
                        cancelled,
                    )? {
                        crate::indexer::pipeline::ReindexOutcome::Completed(summary) => summary,
                        crate::indexer::pipeline::ReindexOutcome::Cancelled => {
                            return Ok(RefreshExecution::Cancelled);
                        }
                    };
                    if !summary.is_noop() {
                        outcome.graph_rebuilt = true;
                        outcome.graph_mode = Some(summary.graph_mode.clone());
                    }
                    outcome.reindex_summary = Some(summary);
                }
                IndexInputDrift::Context => {
                    let summary = match crate::indexer::pipeline::reindex_changed_cancellable(
                        conn,
                        project_root,
                        cancelled,
                    )? {
                        crate::indexer::pipeline::ReindexOutcome::Completed(summary) => summary,
                        crate::indexer::pipeline::ReindexOutcome::Cancelled => {
                            return Ok(RefreshExecution::Cancelled);
                        }
                    };
                    outcome.reindex_summary = Some(summary);
                    outcome.graph_mode = Some(crate::indexer::pipeline::rebuild_graph_from_index(
                        conn,
                        project_root,
                    )?);
                    outcome.graph_rebuilt = true;
                }
                IndexInputDrift::Configuration | IndexInputDrift::Unknown => {
                    match crate::indexer::pipeline::reindex_all_cancellable(
                        conn,
                        project_root,
                        cancelled,
                    )? {
                        crate::indexer::pipeline::PipelineOutcome::Completed => {
                            outcome.graph_rebuilt = true;
                            outcome.graph_mode = Some(crate::indexer::pipeline::GraphMode::Full);
                        }
                        crate::indexer::pipeline::PipelineOutcome::Cancelled => {
                            return Ok(RefreshExecution::Cancelled);
                        }
                    }
                }
            }
        }
    }

    Ok(RefreshExecution::Completed(outcome))
}

/// The complete non-source input catalog for one project root.
///
/// This is deliberately built from the same root-relative form used by the
/// database.  It recognizes fixed resolver and provider inputs plus TypeScript
/// config `extends` chains, so an event that changes resolution semantics is
/// never mistaken for an irrelevant non-source file.
#[derive(Debug, Clone)]
pub struct InputCatalog {
    root: PathBuf,
    ignore_patterns: Vec<String>,
    context_paths: BTreeSet<String>,
    typescript_context_paths: BTreeSet<String>,
    coverage_paths: BTreeSet<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct IndexInputSnapshot {
    config_fingerprint: String,
    context_fingerprint: String,
}

impl InputCatalog {
    pub fn new(root: &Path, ignore_patterns: &[String]) -> Self {
        let mut catalog = Self {
            root: root.to_path_buf(),
            ignore_patterns: ignore_patterns.to_vec(),
            context_paths: fixed_context_paths(),
            typescript_context_paths: BTreeSet::new(),
            coverage_paths: COVERAGE_SEARCH_PATHS
                .iter()
                .map(|(path, _)| (*path).to_owned())
                .collect(),
        };
        catalog.discover_context_paths();
        catalog
    }

    /// Build the catalog with the project's effective ignore policy.
    ///
    /// Keeping this constructor here makes the watcher and every SCIP cache
    /// caller consume the same discovered configuration closure instead of
    /// re-encoding a smaller, subtly different manifest list.
    pub fn for_project(root: &Path) -> Self {
        let config = crate::config::load_config_or_warn(root);
        Self::new(root, &config.ignore)
    }

    /// Classify an absolute watcher path or a root-relative explicit path.
    pub fn classify(&self, path: impl AsRef<Path>) -> ChangeClass {
        let Some(relative) = self.normalize_relative_path(path.as_ref()) else {
            return ChangeClass::Unsafe;
        };
        let Some(relative) = slash_path(&relative) else {
            return ChangeClass::Unsafe;
        };

        // These exact non-source inputs must win over directory ignores: some
        // coverage outputs and C compilation databases deliberately live in
        // directories that the source walker never indexes.
        if self.coverage_paths.contains(&relative) {
            return ChangeClass::Coverage(relative);
        }
        if is_global_configuration_path(&relative) {
            return ChangeClass::GlobalContext(relative);
        }
        if self.context_paths.contains(&relative) || is_tsconfig_name(&relative) {
            return ChangeClass::Context(relative);
        }
        if self.is_ignored(Path::new(&relative)) {
            return ChangeClass::Ignored;
        }
        if is_source_path(Path::new(&relative)) {
            ChangeClass::Source(relative)
        } else {
            ChangeClass::Ignored
        }
    }

    pub fn context_paths(&self) -> impl Iterator<Item = &str> {
        self.context_paths.iter().map(String::as_str)
    }

    /// Stable fingerprint of only the non-source inputs a SCIP provider can
    /// interpret.  It supplements, rather than replaces, the provider's
    /// legacy toolchain/build-key material: the catalog is the shared
    /// invalidation contract for graph and overlay semantics, while a provider
    /// may still have a more precise tool-specific input (for example Go
    /// workspace members).
    pub fn provider_context_fingerprint(&self, provider_lang: &str) -> String {
        const INPUT_CATALOG_FINGERPRINT_VERSION: u32 = 1;

        let mut material = format!(
            "input-catalog-v{INPUT_CATALOG_FINGERPRINT_VERSION}\nprovider={provider_lang}\n"
        );
        for relative in self.context_paths.iter().filter(|relative| {
            provider_uses_context(provider_lang, relative, &self.typescript_context_paths)
        }) {
            material.push_str(relative);
            material.push('=');
            material.push_str(&input_file_fingerprint(&self.root.join(relative)));
            material.push('\n');
        }
        crate::indexer::pipeline::hash_content(&material)
    }

    fn index_input_snapshot(&self) -> IndexInputSnapshot {
        let mut config_material =
            format!("index-input-config-v{INDEX_INPUT_STATE_POLICY_VERSION}\n");
        for (position, pattern) in self.ignore_patterns.iter().enumerate() {
            append_input_fingerprint(
                &mut config_material,
                &format!("ignore[{position}]"),
                pattern,
            );
        }
        for relative in GLOBAL_CONFIGURATION_PATHS {
            append_input_fingerprint(
                &mut config_material,
                relative,
                &input_file_fingerprint(&self.root.join(relative)),
            );
        }

        let mut context_material =
            format!("index-input-context-v{INDEX_INPUT_STATE_POLICY_VERSION}\n");
        for relative in &self.context_paths {
            append_input_fingerprint(
                &mut context_material,
                relative,
                &input_file_fingerprint(&self.root.join(relative)),
            );
        }

        IndexInputSnapshot {
            config_fingerprint: crate::indexer::pipeline::hash_content(&config_material),
            context_fingerprint: crate::indexer::pipeline::hash_content(&context_material),
        }
    }

    fn normalize_relative_path(&self, path: &Path) -> Option<PathBuf> {
        let relative = if path.is_absolute() {
            path.strip_prefix(&self.root).ok()?
        } else {
            path
        };
        normalize_relative(relative)
    }

    fn is_ignored(&self, relative: &Path) -> bool {
        let components = relative
            .components()
            .filter_map(|component| match component {
                Component::Normal(component) => component.to_str(),
                _ => None,
            })
            .collect::<Vec<_>>();

        components.iter().enumerate().any(|(index, name)| {
            matches_ignore_pattern(name, &self.ignore_patterns)
                || (index + 1 < components.len() && is_ignored_dir_component(name, false))
        })
    }

    fn discover_context_paths(&mut self) {
        let mut pending = VecDeque::new();

        for entry in build_walker(&self.root, &self.ignore_patterns, false).flatten() {
            if !entry.file_type().is_some_and(|kind| kind.is_file()) {
                continue;
            }
            let path = entry.into_path();
            let Some(relative) = self.normalize_relative_path(&path) else {
                continue;
            };
            let Some(relative) = slash_path(&relative) else {
                continue;
            };
            if is_context_candidate_path(&relative) {
                self.context_paths.insert(relative.clone());
            }
            if is_tsconfig_name(&relative) && self.typescript_context_paths.insert(relative) {
                pending.push_back(path);
            }
        }

        while let Some(config_path) = pending.pop_front() {
            let Ok(content) = std::fs::read_to_string(&config_path) else {
                continue;
            };
            let Some(extends) = typescript_extends(&content) else {
                continue;
            };
            let Some(extended) = resolve_typescript_extends(&config_path, &extends) else {
                continue;
            };
            let Some(relative) = self.normalize_relative_path(&extended) else {
                continue;
            };
            let Some(relative) = slash_path(&relative) else {
                continue;
            };
            let is_new_typescript_context = self.typescript_context_paths.insert(relative.clone());
            self.context_paths.insert(relative);
            if is_new_typescript_context {
                pending.push_back(extended);
            }
        }
    }
}

/// Persist the non-source input contract after a successful baseline or
/// metadata refresh. It lets a later process distinguish a cheap source delta
/// from a configuration change that requires every source file to be parsed
/// under new semantics.
pub fn persist_index_input_snapshot(
    conn: &rusqlite::Connection,
    catalog: &InputCatalog,
) -> rusqlite::Result<()> {
    let snapshot = catalog.index_input_snapshot();
    conn.execute(
        "INSERT INTO index_input_state \
             (id, policy_version, config_fingerprint, context_fingerprint, recorded_at) \
         VALUES (1, ?1, ?2, ?3, unixepoch('now')) \
         ON CONFLICT(id) DO UPDATE SET \
             policy_version = excluded.policy_version, \
             config_fingerprint = excluded.config_fingerprint, \
             context_fingerprint = excluded.context_fingerprint, \
             recorded_at = excluded.recorded_at",
        rusqlite::params![
            INDEX_INPUT_STATE_POLICY_VERSION,
            snapshot.config_fingerprint,
            snapshot.context_fingerprint,
        ],
    )?;
    Ok(())
}

/// Compare a persisted index-input contract to the current filesystem.
///
/// Source hashes are deliberately absent: the delta indexer already owns them.
/// This tracks only inputs whose changes can alter extraction or resolution
/// while every source hash remains identical.
pub fn index_input_drift(
    conn: &rusqlite::Connection,
    catalog: &InputCatalog,
) -> rusqlite::Result<IndexInputDrift> {
    use rusqlite::OptionalExtension;

    let stored: Option<(i64, String, String)> = conn
        .query_row(
            "SELECT policy_version, config_fingerprint, context_fingerprint \
             FROM index_input_state WHERE id = 1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()?;
    let Some((policy_version, config_fingerprint, context_fingerprint)) = stored else {
        return Ok(IndexInputDrift::Unknown);
    };
    if policy_version != INDEX_INPUT_STATE_POLICY_VERSION {
        return Ok(IndexInputDrift::Unknown);
    }

    let current = catalog.index_input_snapshot();
    if config_fingerprint != current.config_fingerprint {
        Ok(IndexInputDrift::Configuration)
    } else if context_fingerprint != current.context_fingerprint {
        Ok(IndexInputDrift::Context)
    } else {
        Ok(IndexInputDrift::Clean)
    }
}

fn append_input_fingerprint(material: &mut String, label: &str, fingerprint: &str) {
    material.push_str(label);
    material.push('=');
    material.push_str(&fingerprint.len().to_string());
    material.push(':');
    material.push_str(fingerprint);
    material.push('\n');
}

fn is_global_configuration_path(relative: &str) -> bool {
    GLOBAL_CONFIGURATION_PATHS.contains(&relative)
}

fn fixed_context_paths() -> BTreeSet<String> {
    [
        // Resolution maps.
        "Cargo.toml",
        "Cargo.lock",
        "rust-toolchain",
        "rust-toolchain.toml",
        "composer.json",
        "composer.lock",
        "go.mod",
        "go.sum",
        "go.work",
        "go.work.sum",
        // SCIP provider inputs.
        "pyproject.toml",
        "requirements.txt",
        "poetry.lock",
        "Pipfile",
        "Pipfile.lock",
        "setup.cfg",
        "setup.py",
        "package.json",
        "package-lock.json",
        "yarn.lock",
        "pnpm-lock.yaml",
        "pom.xml",
        "build.gradle",
        "build.gradle.kts",
        "settings.gradle",
        "settings.gradle.kts",
        "gradle.properties",
        "global.json",
        "NuGet.config",
        "Directory.Build.props",
        "Directory.Build.targets",
        "Directory.Packages.props",
        "packages.lock.json",
        "Gemfile",
        "Gemfile.lock",
        "compile_commands.json",
        "build/compile_commands.json",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect()
}

/// Whether a path is a fixed or convention-based non-source input.  This is
/// intentionally broader than the current resolver implementations: a false
/// positive costs a graph rebuild, while a false negative can leave a graph or
/// external evidence cache silently stale.
fn is_context_candidate_path(relative: &str) -> bool {
    let path = Path::new(relative);
    let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
        return false;
    };

    if is_tsconfig_name(relative) || (name.starts_with("requirements") && name.ends_with(".txt")) {
        return true;
    }

    if matches!(
        name,
        "Cargo.toml"
            | "Cargo.lock"
            | "rust-toolchain"
            | "rust-toolchain.toml"
            | "composer.json"
            | "composer.lock"
            | "go.mod"
            | "go.sum"
            | "go.work"
            | "go.work.sum"
            | "pyproject.toml"
            | "poetry.lock"
            | "Pipfile"
            | "Pipfile.lock"
            | "setup.cfg"
            | "setup.py"
            | "package.json"
            | "package-lock.json"
            | "yarn.lock"
            | "pnpm-lock.yaml"
            | "pom.xml"
            | "build.gradle"
            | "build.gradle.kts"
            | "settings.gradle"
            | "settings.gradle.kts"
            | "gradle.properties"
            | "global.json"
            | "NuGet.config"
            | "Directory.Build.props"
            | "Directory.Build.targets"
            | "Directory.Packages.props"
            | "packages.lock.json"
            | "Gemfile"
            | "Gemfile.lock"
            | "compile_commands.json"
    ) {
        return true;
    }

    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| matches!(extension, "sln" | "csproj"))
}

fn provider_uses_context(
    provider_lang: &str,
    relative: &str,
    typescript_context_paths: &BTreeSet<String>,
) -> bool {
    let name = Path::new(relative)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or_default();
    let extension = Path::new(relative)
        .extension()
        .and_then(|extension| extension.to_str());

    match provider_lang {
        "rust" => matches!(
            name,
            "Cargo.toml" | "Cargo.lock" | "rust-toolchain" | "rust-toolchain.toml"
        ),
        "go" => matches!(name, "go.mod" | "go.sum" | "go.work" | "go.work.sum"),
        "python" => {
            (name.starts_with("requirements") && name.ends_with(".txt"))
                || matches!(
                    name,
                    "pyproject.toml"
                        | "poetry.lock"
                        | "Pipfile"
                        | "Pipfile.lock"
                        | "setup.cfg"
                        | "setup.py"
                )
        }
        "javascript" | "typescript" => {
            typescript_context_paths.contains(relative)
                || matches!(
                    name,
                    "package.json" | "package-lock.json" | "yarn.lock" | "pnpm-lock.yaml"
                )
        }
        "java" => matches!(
            name,
            "pom.xml"
                | "build.gradle"
                | "build.gradle.kts"
                | "settings.gradle"
                | "settings.gradle.kts"
                | "gradle.properties"
        ),
        "csharp" => {
            matches!(extension, Some("sln") | Some("csproj"))
                || matches!(
                    name,
                    "global.json"
                        | "NuGet.config"
                        | "Directory.Build.props"
                        | "Directory.Build.targets"
                        | "Directory.Packages.props"
                        | "packages.lock.json"
                )
        }
        "php" => matches!(name, "composer.json" | "composer.lock"),
        "ruby" => matches!(name, "Gemfile" | "Gemfile.lock"),
        "c" | "cpp" => name == "compile_commands.json",
        // Unknown providers must fail safe: include every observed context
        // input rather than caching against an underspecified subset.
        _ => true,
    }
}

fn input_file_fingerprint(path: &Path) -> String {
    match std::fs::read(path) {
        Ok(bytes) => format!("present:{}", stable_bytes_hash(&bytes)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => "missing".to_owned(),
        Err(error) => format!("unreadable:{:?}", error.kind()),
    }
}

fn stable_bytes_hash(bytes: &[u8]) -> String {
    const FNV_OFFSET_BASIS: u64 = 0xcbf29ce484222325;
    const FNV_PRIME: u64 = 0x100000001b3;
    let mut hash = FNV_OFFSET_BASIS;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(FNV_PRIME);
    }
    format!("{hash:016x}")
}

fn is_source_path(path: &Path) -> bool {
    let extension = path.extension().and_then(|extension| extension.to_str());
    extension.is_some_and(|extension| {
        language_for_extension(extension).is_some() || is_recognized_unparsed_extension(extension)
    })
}

fn is_tsconfig_name(relative: &str) -> bool {
    Path::new(relative)
        .file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.starts_with("tsconfig") && name.ends_with(".json"))
}

fn normalize_relative(path: &Path) -> Option<PathBuf> {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::Normal(component) => normalized.push(component),
            // Any root, prefix, or parent traversal either leaves the
            // project or makes its intended target ambiguous after a rename.
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => return None,
        }
    }
    (!normalized.as_os_str().is_empty()).then_some(normalized)
}

fn slash_path(path: &Path) -> Option<String> {
    let components = path
        .components()
        .map(|component| component.as_os_str().to_str())
        .collect::<Option<Vec<_>>>()?;
    (!components.is_empty()).then(|| components.join("/"))
}

fn typescript_extends(content: &str) -> Option<String> {
    serde_json::from_str::<serde_json::Value>(content)
        .ok()
        .and_then(|value| value.get("extends")?.as_str().map(str::to_owned))
        .or_else(|| jsonc_string_property(content, "extends"))
}

/// Lightweight JSONC fallback used only to find TypeScript's `extends` value.
///
/// `tsconfig.json` accepts comments and trailing commas, which strict JSON
/// parsers reject.  This intentionally over-accepts a nested `extends` key:
/// extra context invalidation is safe, whereas missing a dependency is not.
fn jsonc_string_property(content: &str, property: &str) -> Option<String> {
    let bytes = content.as_bytes();
    let mut cursor = 0;
    while cursor < bytes.len() {
        skip_jsonc_space_and_comments(bytes, &mut cursor);
        if bytes.get(cursor) != Some(&b'\"') {
            cursor += 1;
            continue;
        }
        let key = parse_json_string(bytes, &mut cursor)?;
        skip_jsonc_space_and_comments(bytes, &mut cursor);
        if bytes.get(cursor) != Some(&b':') {
            continue;
        }
        cursor += 1;
        skip_jsonc_space_and_comments(bytes, &mut cursor);
        if key == property && bytes.get(cursor) == Some(&b'\"') {
            return parse_json_string(bytes, &mut cursor);
        }
    }
    None
}

fn skip_jsonc_space_and_comments(bytes: &[u8], cursor: &mut usize) {
    loop {
        while bytes.get(*cursor).is_some_and(u8::is_ascii_whitespace) {
            *cursor += 1;
        }
        match (bytes.get(*cursor), bytes.get(*cursor + 1)) {
            (Some(b'/'), Some(b'/')) => {
                *cursor += 2;
                while bytes.get(*cursor).is_some_and(|byte| *byte != b'\n') {
                    *cursor += 1;
                }
            }
            (Some(b'/'), Some(b'*')) => {
                *cursor += 2;
                while !matches!(
                    (bytes.get(*cursor), bytes.get(*cursor + 1)),
                    (Some(b'*'), Some(b'/')) | (None, _)
                ) {
                    *cursor += 1;
                }
                if bytes.get(*cursor).is_some() {
                    *cursor += 2;
                }
            }
            _ => return,
        }
    }
}

fn parse_json_string(bytes: &[u8], cursor: &mut usize) -> Option<String> {
    if bytes.get(*cursor) != Some(&b'\"') {
        return None;
    }
    *cursor += 1;
    let mut value = String::new();
    while let Some(byte) = bytes.get(*cursor) {
        *cursor += 1;
        match byte {
            b'\"' => return Some(value),
            b'\\' => {
                let escaped = *bytes.get(*cursor)?;
                *cursor += 1;
                value.push(match escaped {
                    b'\"' => '\"',
                    b'\\' => '\\',
                    b'/' => '/',
                    b'b' => '\u{0008}',
                    b'f' => '\u{000C}',
                    b'n' => '\n',
                    b'r' => '\r',
                    b't' => '\t',
                    // An escaped path character is still conservatively
                    // represented; unsupported unicode escapes simply fail
                    // closed rather than inventing a filesystem path.
                    b'u' => return None,
                    _ => return None,
                });
            }
            byte if byte.is_ascii_control() => return None,
            byte => value.push(*byte as char),
        }
    }
    None
}

fn resolve_typescript_extends(config_path: &Path, extends: &str) -> Option<PathBuf> {
    // Package specifiers resolve through node_modules.  The lockfile remains
    // their watched input; only project-relative extends can safely be mapped
    // to an exact local event path.
    if !extends.starts_with('.') {
        return None;
    }
    let mut extended = config_path.parent()?.join(extends);
    if extended.extension().is_none() {
        extended.set_extension("json");
    }
    Some(extended)
}

#[cfg(test)]
mod tests {
    use std::path::Path;
    use std::sync::{Arc, RwLock};

    use rusqlite::Connection;

    use super::{
        ChangeClass, ChangeSet, FullReconciliationReason, InputCatalog, RefreshRequest,
        execute_refresh,
    };

    #[test]
    fn classifies_source_context_coverage_and_unsafe_paths() {
        let root = tempfile::tempdir().unwrap();
        let catalog = InputCatalog::new(root.path(), &[]);

        assert!(
            catalog
                .classify(root.path().join("src/main.rs"))
                .is_source()
        );
        assert!(
            catalog
                .classify(root.path().join("Cargo.toml"))
                .is_context()
        );
        assert!(
            catalog
                .classify(root.path().join(".calm/config.json"))
                .is_global_context()
        );
        assert!(
            catalog
                .classify(root.path().join(".coverage"))
                .is_coverage()
        );
        assert!(
            catalog
                .classify(root.path().parent().unwrap().join("outside.rs"))
                .is_unsafe()
        );
    }

    #[test]
    fn tracks_jsonc_typescript_extends_even_when_the_base_is_not_a_tsconfig() {
        let root = tempfile::tempdir().unwrap();
        let configs = root.path().join("configs");
        std::fs::create_dir_all(&configs).unwrap();
        std::fs::write(
            root.path().join("tsconfig.json"),
            "{ // JSONC is valid for TypeScript configs\n  \"extends\": \"./configs/base\",\n}",
        )
        .unwrap();
        std::fs::write(configs.join("base.json"), "{}").unwrap();

        let catalog = InputCatalog::new(root.path(), &[]);
        assert!(catalog.classify(configs.join("base.json")).is_context());
        let first_fingerprint = catalog.provider_context_fingerprint("javascript");

        std::fs::write(configs.join("base.json"), "{\"compilerOptions\": {}}").unwrap();
        let second_fingerprint =
            InputCatalog::new(root.path(), &[]).provider_context_fingerprint("javascript");
        assert_ne!(first_fingerprint, second_fingerprint);
    }

    #[test]
    fn changeset_never_uses_event_count_as_a_full_rescan_proxy() {
        let mut changes = ChangeSet::default();
        for index in 0..1000 {
            changes.record_class(ChangeClass::Source(format!("src/{index}.rs")));
        }

        assert!(matches!(
            changes.refresh_request(),
            RefreshRequest::Incremental { source_paths, .. } if source_paths.len() == 1000
        ));

        changes.require_full_reconciliation(FullReconciliationReason::NotifyRescan);
        assert!(matches!(
            changes.refresh_request(),
            RefreshRequest::FullReconciliation {
                reason: FullReconciliationReason::NotifyRescan,
                reload_coverage: true,
            }
        ));
    }

    #[test]
    fn metadata_only_refresh_matches_a_fresh_full_index() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(root.path().join("src")).unwrap();
        std::fs::write(
            root.path().join("Cargo.toml"),
            "[package]\nname = \"refresh-fixture\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
        )
        .unwrap();
        std::fs::write(
            root.path().join("src/lib.rs"),
            "pub fn target() {}\npub fn caller() { target(); }\n",
        )
        .unwrap();

        let mut continued = index_root(root.path());
        std::fs::write(
            root.path().join("Cargo.toml"),
            "[package]\nname = \"refresh-fixture\"\nversion = \"0.1.0\"\nedition = \"2024\"\n",
        )
        .unwrap();

        let catalog = InputCatalog::new(root.path(), &[]);
        let mut changes = ChangeSet::default();
        assert!(
            changes
                .record_path(&catalog, root.path().join("Cargo.toml"))
                .is_context()
        );

        let outcome = execute_refresh(&mut continued, root.path(), &changes.refresh_request())
            .expect("metadata refresh should rebuild the graph from existing index rows");
        assert!(outcome.graph_rebuilt);

        let fresh = index_root(root.path());
        assert_eq!(graph_rows(&continued), graph_rows(&fresh));
    }

    #[test]
    fn configuration_reconciliation_reparses_unchanged_sources() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(root.path().join("src")).unwrap();
        std::fs::write(
            root.path().join("src/lib.rs"),
            "pub fn target() {}\npub fn caller() { target(); }\n",
        )
        .unwrap();

        let mut continued = index_root(root.path());
        let initial_catalog = InputCatalog::for_project(root.path());
        super::persist_index_input_snapshot(&continued, &initial_catalog).unwrap();
        let before: i64 = continued
            .query_row(
                "SELECT is_entry_point FROM symbols WHERE name = 'caller'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(before, 0);

        std::fs::write(
            root.path().join("config.json"),
            r#"{"entry_points":["caller"]}"#,
        )
        .unwrap();

        let outcome = execute_refresh(
            &mut continued,
            root.path(),
            &RefreshRequest::FullReconciliation {
                reason: FullReconciliationReason::ConfigurationChanged,
                reload_coverage: true,
            },
        )
        .expect("configuration reconciliation should reparse source under the new config");

        assert!(outcome.graph_rebuilt);
        let after: i64 = continued
            .query_row(
                "SELECT is_entry_point FROM symbols WHERE name = 'caller'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(after, 1);
    }

    #[test]
    fn persisted_input_snapshot_distinguishes_context_from_configuration_drift() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(root.path().join("src")).unwrap();
        std::fs::write(root.path().join("src/lib.rs"), "pub fn target() {}\n").unwrap();

        let conn = index_root(root.path());
        super::persist_index_input_snapshot(&conn, &InputCatalog::for_project(root.path()))
            .unwrap();
        assert_eq!(
            super::index_input_drift(&conn, &InputCatalog::for_project(root.path())).unwrap(),
            super::IndexInputDrift::Clean
        );

        std::fs::write(
            root.path().join("Cargo.toml"),
            "[package]\nname = \"snapshot-fixture\"\nversion = \"0.1.0\"\n",
        )
        .unwrap();
        assert_eq!(
            super::index_input_drift(&conn, &InputCatalog::for_project(root.path())).unwrap(),
            super::IndexInputDrift::Context
        );

        std::fs::write(
            root.path().join("config.json"),
            r#"{"entry_points":["target"]}"#,
        )
        .unwrap();
        assert_eq!(
            super::index_input_drift(&conn, &InputCatalog::for_project(root.path())).unwrap(),
            super::IndexInputDrift::Configuration
        );
    }

    fn index_root(root: &Path) -> Connection {
        let mut conn = Connection::open_in_memory().unwrap();
        crate::db::schema::init_db(&conn).unwrap();
        let phase = Arc::new(RwLock::new(crate::types::IndexingPhase::Scanning));
        crate::indexer::pipeline::run_indexing_pipeline(&mut conn, root, phase).unwrap();
        conn
    }

    fn graph_rows(conn: &Connection) -> Vec<(String, String, i64, String)> {
        let mut statement = conn
            .prepare("SELECT from_symbol, to_symbol, call_site_line, edge_confidence FROM call_edges ORDER BY from_symbol, to_symbol, call_site_line, edge_confidence")
            .unwrap();
        statement
            .query_map([], |row| {
                Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
            })
            .unwrap()
            .map(Result::unwrap)
            .collect()
    }
}
