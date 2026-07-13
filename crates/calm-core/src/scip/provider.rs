//! Data-driven description of one SCIP-capable language, so adding a new
//! provider (Go, Java, ...) means adding one `ScipProvider` value instead of
//! copying this whole `scip/` module — see the plan doc
//! `docs/superskills/plans/2026-07-07-eight-lang-formal-tier.md` §3 (P0.4).
//!
//! 9 entries exist today: `RUST`, `GO` (P2.1), `PYTHON` (P2.4), `TYPESCRIPT`
//! (P3.2, covers JS+TS), `JAVA` (P2.2, also covers Kotlin via a `dirty_langs`
//! piggyback — see its own doc comment), `CSHARP` (P2.3), `PHP` (P2.5),
//! `RUBY` (Phase D.1, 2026-07-11), and `CLANG` (P3.1, scaffold-only — see
//! its own doc comment). Fields sketched
//! in the plan's P0.4 design for multi-root marker-file discovery, prereq
//! gating, and refresh policy are still deliberately NOT here — `ScipProvider`
//! itself stays single-root/single-output on purpose. Go's `go.work`
//! multi-module case (a documented upstream `scip-go` limitation — no native
//! workspace flag, confirmed against Sourcegraph's own docs) is handled one
//! level up instead: `scip::mod::run_go_workspace_overlay` detects
//! `go.work` (`parse_go_work_modules` below) and runs this same
//! single-root `GO` provider once per member module, reusing
//! `parse::parse_index`'s `rebase_prefix` (which was already built with
//! exactly this case in mind) rather than widening the struct itself.
//! scip-python/scip-typescript still each index one `--cwd` tree per
//! invocation, unchanged. Add real multi-root fields to `ScipProvider`
//! itself only if a second provider needs the same treatment Go just got.
//!
//!
//! **Live-verification history is uneven across these 9 — "the code exists"
//! and "confirmed working right now" are two different claims, don't conflate
//! them** (audited 2026-07-10, `RUBY` added 2026-07-11). `GO`, `JAVA`,
//! `CSHARP`, and `PHP` were each manually verified live-passing exactly once,
//! by the session that implemented them, against a real indexer binary with
//! a real toolchain (see
//! `docs/superskills/plans/2026-07-07-eight-lang-formal-tier.md` §5's
//! table for the exact fixture results and match rates — e.g. Go 0.67,
//! Java 1.00). None of that has been *continuously* re-verified since,
//! though: `RUST` is the only provider with a real green run in GitHub
//! Actions CI history (`.github/workflows/scip-nightly.yml`, run
//! 2026-07-08) — every nightly run new enough to include the other
//! providers either predates the relevant commit or failed before reaching
//! the test step. `PHP`'s own `scip-php` install is currently, actively
//! broken in CI as of that audit: Composer refuses `davidrjenni/scip-php`
//! because its `google/protobuf` dependency range is blocked by security
//! advisory PKSA-tcfz-w4fm-hhk9 (see the pinned-version workaround in
//! `scip-nightly.yml`). `PYTHON` and `TYPESCRIPT` were independently
//! re-confirmed live-passing by hand in a plain dev sandbox during the same
//! 2026-07-10 audit (both run through `npx` with no extra toolchain install,
//! so friction — and therefore real-world reach — is far lower than
//! Go/Java/C#/PHP, which all need a locally installed toolchain). `RUBY`
//! was live end-to-end verified on 2026-07-11, the day it was added.
//! `CLANG` has no live-verification path at all, ever, by design — see its
//! own doc comment below for the two independent blockers.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

/// One row of the provider table.
pub struct ScipProvider {
    /// `file_index.language` value this provider's edges are primarily
    /// attributed to, e.g. `"rust"`. Used to label log lines and to name the
    /// cache file so a second provider's output isn't ambiguous with this
    /// one's. NOT used to scope `source_dirty_keys` — see `dirty_langs`,
    /// which exists precisely because `lang` alone can't represent a
    /// provider covering more than one `file_index.language` value.
    pub lang: &'static str,
    /// `file_index.language` values whose dirty (changed-since-last-run)
    /// files should invalidate this provider's cache key. A single-language
    /// provider (Rust/Go/Python) sets this to `&[lang]`; `TYPESCRIPT` needs
    /// both `"javascript"` and `"typescript"` because one `scip-typescript
    /// index` run covers both extensions in the same project — added
    /// alongside that provider rather than guessed up front, per P0.4's own
    /// "widen when a real 2nd case proves the shape" rule.
    pub dirty_langs: &'static [&'static str],
    /// Locate a usable indexer binary: explicit override first, then this
    /// language's own probe (Rust's is rustup/VS Code aware; a future
    /// simpler provider might just be a `PATH` lookup).
    pub resolve_binary: fn(Option<&str>, &Path) -> Option<PathBuf>,
    /// Build the (not-yet-spawned) command that writes a `.scip` index to
    /// `out` when run against `root` using binary `bin`.
    pub build_command: fn(bin: &Path, root: &Path, out: &Path) -> Command,
    /// Bounded wait before killing an overrunning indexer run (Java/clang
    /// providers will need a higher budget than Rust's).
    pub timeout: Duration,
    /// Full cache-key material for this language: binary version, toolchain
    /// fingerprint, manifest/lockfile hashes, and `dirty`, all folded into
    /// one opaque string. Owns its own probing so the generic pipeline
    /// never needs to know how many manifest files a given language has.
    pub cache_key: fn(bin: &Path, root: &Path, dirty: &[String]) -> String,
    /// `.calm/<this>` cache filename — kept distinct per provider so a
    /// future second language's overlay can't collide with Rust's existing
    /// `scip.cache`.
    pub cache_file_name: &'static str,
}
/// The original entry (P0.4) — no longer the only one, see the module doc
/// comment above for the full 8-entry list and their live-verification status.
/// `mod.rs` are thin Rust-specific wrappers around the generic
/// `run_overlay_for`/`overlay_status_for` built against this value — see
/// those wrappers for why their signatures still take `RustConfig` directly
/// instead of `&ScipProvider` (backward compat for the 3 existing production
/// call sites, zero behavior change per P0.4's own DoD).
pub const RUST: ScipProvider = ScipProvider {
    lang: "rust",
    dirty_langs: &["rust"],
    resolve_binary: super::runner::resolve_binary,
    build_command: super::runner::rust_build_command,
    timeout: super::runner::SCIP_TIMEOUT,
    cache_key: rust_cache_key,
    cache_file_name: "scip.cache",
};

fn rust_cache_key(bin: &Path, root: &Path, dirty: &[String]) -> String {
    super::cache::overlay_cache_key(
        &super::runner::binary_version(bin),
        &super::runner::active_toolchain_fingerprint(root),
        &lockfile_hash(root),
        &cargo_toml_hash(root),
        dirty,
    )
}

fn lockfile_hash(root: &Path) -> String {
    std::fs::read_to_string(root.join("Cargo.lock"))
        .map(|s| crate::indexer::pipeline::hash_content(&s))
        .unwrap_or_default()
}

fn cargo_toml_hash(root: &Path) -> String {
    std::fs::read_to_string(root.join("Cargo.toml"))
        .map(|s| crate::indexer::pipeline::hash_content(&s))
        .unwrap_or_default()
}

/// The 2nd entry in the table (Phase 2 / P2.1) — validates that P0.4's
/// `ScipProvider` shape actually generalizes past Rust, per the plan doc's
/// own note that widening the struct should wait for a real 2nd provider
/// instead of being guessed up front.
pub const GO: ScipProvider = ScipProvider {
    lang: "go",
    dirty_langs: &["go"],
    resolve_binary: super::runner::go_resolve_binary,
    build_command: super::runner::go_build_command,
    timeout: super::runner::GO_SCIP_TIMEOUT,
    cache_key: go_cache_key,
    cache_file_name: "scip-go.cache",
};

/// Detects and parses `go.work` at `root`, returning the member module
/// directories declared in its `use` directives (relative to `root`), or
/// `None` when no `go.work` file exists — the common, single-module case,
/// where every caller must fall back to the existing single-root path with
/// zero behavior change. Handles both `use ./path` (single-line) and
/// `use (\n    ./a\n    ./b\n)` (grouped) forms, which is everything module
/// enumeration needs; `go`/`toolchain`/`replace` directives are ignored.
/// `scip-go` has no native workspace/`go.work` flag of its own — a
/// documented upstream limitation (Sourcegraph's own docs: run the indexer
/// once per module root, then rebase paths) — this is what lets the
/// orchestration in `scip/mod.rs` (`run_go_workspace_overlay`) enumerate
/// modules and run/ingest each one separately, reusing the exact
/// `sub_root`/`rebase_prefix` mechanism `parse::parse_index` already had
/// for precisely this case.
pub(crate) fn parse_go_work_modules(root: &Path) -> Option<Vec<std::path::PathBuf>> {
    let text = std::fs::read_to_string(root.join("go.work")).ok()?;
    let mut modules = Vec::new();
    let mut in_use_block = false;
    for raw_line in text.lines() {
        let line = raw_line.split("//").next().unwrap_or("").trim();
        if line.is_empty() {
            continue;
        }
        if in_use_block {
            if line == ")" {
                in_use_block = false;
            } else {
                modules.push(std::path::PathBuf::from(line));
            }
            continue;
        }
        if let Some(rest) = line.strip_prefix("use ") {
            let rest = rest.trim();
            if rest == "(" {
                in_use_block = true;
            } else if !rest.is_empty() {
                modules.push(std::path::PathBuf::from(rest));
            }
        }
    }
    if modules.is_empty() {
        None
    } else {
        Some(modules)
    }
}

fn go_cache_key(bin: &Path, root: &Path, dirty: &[String]) -> String {
    // Workspace (go.work) case: fold every member module's go.sum/go.mod
    // into the material so a change in *any* module invalidates the whole
    // cached run — Phase 1 doesn't attempt per-module incremental skip (see
    // `run_go_workspace_overlay`'s doc comment for why that's deliberately
    // out of scope). Single-module case (the common one) is byte-for-byte
    // the same computation as before this function grew this branch.
    let (sum_material, mod_material) = match parse_go_work_modules(root) {
        Some(modules) => {
            let mut sums = String::new();
            let mut mods = String::new();
            for m in &modules {
                sums.push_str(&go_sum_hash(&root.join(m)));
                sums.push(',');
                mods.push_str(&go_mod_hash(&root.join(m)));
                mods.push(',');
            }
            (sums, mods)
        }
        None => (go_sum_hash(root), go_mod_hash(root)),
    };
    super::cache::overlay_cache_key(
        &super::runner::binary_version(bin),
        &super::runner::go_toolchain_fingerprint(root),
        &sum_material,
        &mod_material,
        dirty,
    )
}

fn go_mod_hash(root: &Path) -> String {
    std::fs::read_to_string(root.join("go.mod"))
        .map(|s| crate::indexer::pipeline::hash_content(&s))
        .unwrap_or_default()
}

fn go_sum_hash(root: &Path) -> String {
    std::fs::read_to_string(root.join("go.sum"))
        .map(|s| crate::indexer::pipeline::hash_content(&s))
        .unwrap_or_default()
}

/// The 3rd entry in the table (Phase 2 / P2.4) — upgrades Python from
/// stack-graphs (archived upstream, per-file name-set matching only) to real
/// exact-(file,line) SCIP resolution. The two coexist via the provenance
/// mechanism P0.3 already built for exactly this: `ingest_occurrences` is
/// allowed to override a `formal_source = 'stack_graphs'` edge but never its
/// own prior `'scip'` verdict — no new code needed here for that part.
pub const PYTHON: ScipProvider = ScipProvider {
    lang: "python",
    dirty_langs: &["python"],
    resolve_binary: super::runner::python_resolve_binary,
    build_command: super::runner::python_build_command,
    timeout: super::runner::PYTHON_SCIP_TIMEOUT,
    cache_key: python_cache_key,
    cache_file_name: "scip-python.cache",
};

fn python_cache_key(bin: &Path, root: &Path, dirty: &[String]) -> String {
    super::cache::overlay_cache_key(
        &super::runner::python_binary_version(bin),
        &super::runner::python_toolchain_fingerprint(root),
        &python_requirements_hash(root),
        &python_pyproject_hash(root),
        dirty,
    )
}

/// V1 simplification (noted in the plan): only `requirements.txt` is hashed
/// as "the lockfile" — `poetry.lock`/`Pipfile.lock` aren't consulted yet.
/// Missing entirely is not an error (plenty of real Python projects have
/// neither) — an absent file just contributes an empty, stable hash input.
fn python_requirements_hash(root: &Path) -> String {
    std::fs::read_to_string(root.join("requirements.txt"))
        .map(|s| crate::indexer::pipeline::hash_content(&s))
        .unwrap_or_default()
}

fn python_pyproject_hash(root: &Path) -> String {
    std::fs::read_to_string(root.join("pyproject.toml"))
        .map(|s| crate::indexer::pipeline::hash_content(&s))
        .unwrap_or_default()
}

/// The 4th entry in the table (Phase 3 / P3.2) — one `scip-typescript index`
/// pass covers both `.js`/`.jsx` and `.ts`/`.tsx` (it infers a `tsconfig.json`
/// for plain-JS projects via `--infer-tsconfig` when none exists), hence
/// `dirty_langs` carrying both `file_index.language` values while `lang`
/// stays a single display string for logs/cache-file naming. Also the exit
/// ramp from the archived `stack-graphs` JS/TS formal tier (P1.1/pre-existing
/// TS support): this provider runs after the base pipeline, and `scip`
/// provenance is allowed to override a prior `stack_graphs` verdict via the
/// same P0.3 mechanism Python's provider already relies on — no new code
/// needed here for that part either.
pub const TYPESCRIPT: ScipProvider = ScipProvider {
    lang: "javascript",
    dirty_langs: &["javascript", "typescript"],
    resolve_binary: super::runner::js_resolve_binary,
    build_command: super::runner::js_build_command,
    timeout: super::runner::JS_SCIP_TIMEOUT,
    cache_key: js_cache_key,
    cache_file_name: "scip-ts.cache",
};

fn js_cache_key(bin: &Path, root: &Path, dirty: &[String]) -> String {
    super::cache::overlay_cache_key(
        &super::runner::js_binary_version(bin),
        &super::runner::js_toolchain_fingerprint(root),
        &js_lockfile_hash(root),
        &js_manifest_hash(root),
        dirty,
    )
}

fn js_manifest_hash(root: &Path) -> String {
    std::fs::read_to_string(root.join("package.json"))
        .map(|s| crate::indexer::pipeline::hash_content(&s))
        .unwrap_or_default()
}

/// First lockfile that actually exists wins (a checkout has at most one) —
/// npm/yarn/pnpm are mutually exclusive in practice, so there's no need to
/// hash all three and no ambiguity from checking in this fixed order.
fn js_lockfile_hash(root: &Path) -> String {
    for name in ["package-lock.json", "yarn.lock", "pnpm-lock.yaml"] {
        if let Ok(s) = std::fs::read_to_string(root.join(name)) {
            return crate::indexer::pipeline::hash_content(&s);
        }
    }
    String::new()
}

/// The 5th entry in the table (Phase 2 / P2.2) — unlike Go/Python/TS,
/// `scip-java` isn't a standalone binary a package manager installs; it's a
/// Maven Central artifact (`com.sourcegraph:scip-java_2.13`) meant to be run
/// via `cs bootstrap ... -o scip-java` (creating a launcher script on
/// `PATH`) or the `sourcegraph/scip-java` Docker image — confirmed by
/// actually running it that way (Maven-resolved classpath, no `coursier`
/// binary needed: `mvn dependency:build-classpath` against a throwaway pom
/// pulls the exact same jars `cs launch` would, since both just resolve
/// Maven Central). `java_resolve_binary` therefore only ever looks for a
/// `scip-java` launcher already on `PATH` — same shape as `GO`'s
/// `go_resolve_binary`, deliberately simpler than Python/TS's `npx` fallback
/// (there's no equivalently ubiquitous "run this artifact on demand" tool
/// bundled with a JDK the way `npx` ships with `npm`).
///
/// `dirty_langs` also carries `"kotlin"` (Phase D.2, 2026-07-11) — the same
/// `scip-java index` invocation indexes a mixed Java+Kotlin Gradle/Maven
/// module in one pass (`scip-java` bundles Kotlin support via its own
/// `kotlinc` compiler-plugin integration, confirmed by the tool's own
/// `--help`/docs — it is not a Java-only indexer), mirroring `TYPESCRIPT`'s
/// established precedent for one provider spanning more than one
/// `file_index.language` value. `lang` deliberately stays `"java"` (the
/// existing display/cache-filename string) rather than gaining a second
/// selector — `scip_refresh`/`refresh_language` already dispatch this whole
/// provider under the single `"java"` name the same way `"javascript"`
/// alone dispatches `TYPESCRIPT` for both JS and TS; no new config struct
/// exists for Kotlin either — it rides along under `Config.java.scip`,
/// exactly like TS rides along under `Config.js.scip`. `ingest_occurrences`
/// (`scip/ingest.rs`) is purely path/line-driven with no per-provider
/// language filter, so Kotlin occurrences flow through unchanged once a
/// `.scip` index containing them exists — confirmed by reading the
/// function signature directly (`&[ScipOccurrence]`, no `provider` param at
/// all).
pub const JAVA: ScipProvider = ScipProvider {
    lang: "java",
    dirty_langs: &["java", "kotlin"],
    resolve_binary: super::runner::java_resolve_binary,
    build_command: super::runner::java_build_command,
    timeout: super::runner::JAVA_SCIP_TIMEOUT,
    cache_key: java_cache_key,
    cache_file_name: "scip-java.cache",
};

fn java_cache_key(bin: &Path, root: &Path, dirty: &[String]) -> String {
    super::cache::overlay_cache_key(
        &super::runner::binary_version(bin),
        &super::runner::java_toolchain_fingerprint(root),
        "", // no reliable Java lockfile equivalent (V1 simplification, see java_build_file_hash)
        &java_build_file_hash(root),
        dirty,
    )
}

/// First build descriptor that actually exists wins — Maven (`pom.xml`) and
/// Gradle (`build.gradle[.kts]`) are mutually exclusive build systems in
/// practice, mirroring `js_lockfile_hash`'s same reasoning. V1
/// simplification: a multi-module Maven/Gradle aggregator's child-module
/// build files aren't hashed individually (matches Python's V1 cut of only
/// hashing `requirements.txt`, not every possible dependency-pinning file).
fn java_build_file_hash(root: &Path) -> String {
    for name in ["pom.xml", "build.gradle.kts", "build.gradle"] {
        if let Ok(s) = std::fs::read_to_string(root.join(name)) {
            return crate::indexer::pipeline::hash_content(&s);
        }
    }
    String::new()
}

/// The 6th entry in the table (Phase 2 / P2.3) — `scip-dotnet` is a real
/// `dotnet tool` published on NuGet (`dotnet tool install --global
/// scip-dotnet`), so unlike `scip-java` it needs no bespoke bootstrap:
/// NuGet is reachable the same way Maven Central is, and `dotnet tool
/// install` is the officially documented install path (no `coursier`/Docker
/// workaround needed here). `csharp_resolve_binary` only ever looks for a
/// `scip-dotnet` launcher already on `PATH` — same shape as `GO`'s
/// `go_resolve_binary`.
pub const CSHARP: ScipProvider = ScipProvider {
    lang: "csharp",
    dirty_langs: &["csharp"],
    resolve_binary: super::runner::csharp_resolve_binary,
    build_command: super::runner::csharp_build_command,
    timeout: super::runner::CSHARP_SCIP_TIMEOUT,
    cache_key: csharp_cache_key,
    cache_file_name: "scip-dotnet.cache",
};

fn csharp_cache_key(bin: &Path, root: &Path, dirty: &[String]) -> String {
    super::cache::overlay_cache_key(
        &super::runner::binary_version(bin),
        &super::runner::csharp_toolchain_fingerprint(root),
        "", // no reliable lockfile analog (packages.lock.json is opt-in, not universal — V1 cut)
        &csharp_project_file_hash(root),
        dirty,
    )
}

/// Hashes whichever project/solution file `csharp_build_command` will
/// actually index (see `runner::find_csharp_project`) — a `.sln`/`.csproj`
/// change (new project reference, target framework bump) can change
/// `scip-dotnet`'s output even with nothing else different, so this has to
/// be part of the cache key, not just a manifest presence check.
fn csharp_project_file_hash(root: &Path) -> String {
    super::runner::find_csharp_project(root)
        .and_then(|p| std::fs::read_to_string(p).ok())
        .map(|s| crate::indexer::pipeline::hash_content(&s))
        .unwrap_or_default()
}

/// The 7th entry in the table (Phase 2 / P2.5) — `scip-php` is a Composer
/// package (`davidrjenni/scip-php`), confirmed by actually installing it
/// via `composer install` in the session that added this entry — an
/// earlier session's `go install` attempts were chasing the wrong
/// ecosystem entirely (the module only exists on Go's proxy because that
/// proxy mirrors any git tag on request; there's no real Go package
/// underneath). `php_resolve_binary` prefers `vendor/bin/scip-php` (the
/// project-local Composer bin, matching the "Prereq" column's own stated
/// preference for a per-project pinned version) before falling back to a
/// global `PATH` lookup.
///
/// Neither this table's shared `binary_runs`/`binary_version` (both drive
/// `<bin> --version`) is used for PHP — confirmed via the real CLI that
/// `scip-php`'s `getopt('h', ['help', 'memory-limit:'])` silently ignores
/// an unrecognized `--version` flag instead of erroring, so that probe
/// falls through and actually **runs the full indexer** against whatever
/// the current directory happens to be (reproduced: `scip-php --version`
/// in `/tmp` crashed trying to read `/tmp/composer.json`, proving it had
/// already started indexing, not printing a version). `php_resolve_binary`/
/// `php_binary_version` use `--help` instead, which the CLI does handle
/// safely.
pub const PHP: ScipProvider = ScipProvider {
    lang: "php",
    dirty_langs: &["php"],
    resolve_binary: super::runner::php_resolve_binary,
    build_command: super::runner::php_build_command,
    timeout: super::runner::PHP_SCIP_TIMEOUT,
    cache_key: php_cache_key,
    cache_file_name: "scip-php.cache",
};

fn php_cache_key(bin: &Path, root: &Path, dirty: &[String]) -> String {
    super::cache::overlay_cache_key(
        &super::runner::php_binary_version(bin),
        &super::runner::php_toolchain_fingerprint(root),
        &php_composer_lock_hash(root),
        &php_composer_json_hash(root),
        dirty,
    )
}

fn php_composer_lock_hash(root: &Path) -> String {
    std::fs::read_to_string(root.join("composer.lock"))
        .map(|s| crate::indexer::pipeline::hash_content(&s))
        .unwrap_or_default()
}

fn php_composer_json_hash(root: &Path) -> String {
    std::fs::read_to_string(root.join("composer.json"))
        .map(|s| crate::indexer::pipeline::hash_content(&s))
        .unwrap_or_default()
}

/// The 9th entry in the table (Phase D.1, 2026-07-11) — `scip-ruby`
/// (Sourcegraph's fork of Sorbet, https://github.com/sourcegraph/scip-ruby).
/// Live-verified end to end on a genuinely `# typed: false` (untyped)
/// fixture, the exact scenario the plan flagged as needing real evidence
/// before committing to a "formal tier" claim for Ruby: Sorbet performs
/// real flow-sensitive type narrowing inside a `case handler when X ...
/// when Y` even without any `sig` annotations (confirmed by inspecting the
/// raw `.scip` output's hover-type strings — `handler (AlphaHandler)` /
/// `handler (BetaHandler)` narrowed per-branch, `handler
/// (T.any(AlphaHandler, BetaHandler))` before the narrowing point), which
/// is exactly the kind of ambiguity CALM's syntactic resolver can't
/// follow. See `scip::runner::ruby_resolve_binary`'s doc comment for a
/// real, load-bearing gotcha found during this verification (the
/// `gem install`-installed wrapper script refuses to run standalone).
pub const RUBY: ScipProvider = ScipProvider {
    lang: "ruby",
    dirty_langs: &["ruby"],
    resolve_binary: super::runner::ruby_resolve_binary,
    build_command: super::runner::ruby_build_command,
    timeout: super::runner::RUBY_SCIP_TIMEOUT,
    cache_key: ruby_cache_key,
    cache_file_name: "scip-ruby.cache",
};

fn ruby_cache_key(bin: &Path, root: &Path, dirty: &[String]) -> String {
    super::cache::overlay_cache_key(
        &super::runner::ruby_binary_version(bin),
        &super::runner::ruby_toolchain_fingerprint(root),
        &ruby_gemfile_lock_hash(root),
        "", // no separate manifest analog beyond Gemfile.lock itself
        dirty,
    )
}

fn ruby_gemfile_lock_hash(root: &Path) -> String {
    std::fs::read_to_string(root.join("Gemfile.lock"))
        .map(|s| crate::indexer::pipeline::hash_content(&s))
        .unwrap_or_default()
}

/// The 8th entry in the table (Phase 3 / P3.1) — **scaffolding only, no
/// live verification**, unlike every other provider in this table. Two
/// independent blockers made `scip-clang` unobtainable in this sandbox:
/// prebuilt binaries are published as GitHub Release assets, and this
/// environment's egress policy returns a hard 403 for both
/// `api.github.com` and `github.com/*/releases/*` (confirmed via the
/// proxy's own status endpoint — a real policy denial, not a transient
/// network failure); building from source requires Bazel, and only
/// `bazel-bootstrap` is apt-installable here, which itself requires
/// building Bazel from source first. Neither path is realistic inside one
/// session, so this entry follows the plan's own P3.1 spec exactly
/// (`docs/superskills/plans/2026-07-07-eight-lang-formal-tier.md` §P3.1)
/// but has unit tests only — no `#[ignore]`d live-binary integration test
/// exists for this provider the way `csharp_overlay_upgrades_a_real_edge_*`/
/// `php_overlay_upgrades_a_real_edge_*` do for C#/PHP.
///
/// Deliberate cut from the plan's literal `ClangConfig { scip: ScipConfig,
/// compile_commands: Option<String> }` shape: `ClangConfig` below carries
/// only `scip`, not a second `compile_commands` override field. Every
/// `ScipProvider::build_command`/`resolve_binary` function signature is
/// generic across all 8 providers and carries no per-provider config
/// payload beyond `cfg.binary` — wiring a `ClangConfig`-specific override
/// through that shared, fixed-signature plumbing would mean changing the
/// `ScipProvider` struct's fn-pointer types (and, with them, all 7 other
/// providers' functions) for a provider that cannot be exercised end to
/// end here regardless. `clang_resolve_binary`/`find_compile_commands`
/// (`runner.rs`) instead auto-detect `compile_commands.json` at `root` or
/// `root/build/` only, exactly satisfying the plan's own DoD wording
/// ("auto-detect ... ở root/`build/`; absent → silent no-op") without a
/// half-wired config field pointing nowhere. A future session adding real
/// coverage (once a `scip-clang` binary is actually obtainable) is the
/// right time to decide whether an override is worth the shared-signature
/// change.
///
/// `dirty_langs`/`lang` follow `TYPESCRIPT`'s established precedent for a
/// provider spanning more than one `file_index.language` value: one
/// `scip-clang` run covers both `.c` and `.cpp` sources via the same
/// `compile_commands.json`, so `dirty_langs` lists both and `lang` takes
/// the first (`"c"`) as the single display/cache-filename string.
///
/// Policy default (see `ClangConfig` below): `MinInterval(900)`, same as
/// `JAVA` — `scip-clang` compiles real translation units, exactly the
/// "heavy future provider (Java/clang)" case `ScipConfig::policy`'s own
/// doc comment named up front, and the plan's own risk note is explicit:
/// "tuyệt đối không nối scip-java/scip-clang vào on-save".
pub const CLANG: ScipProvider = ScipProvider {
    lang: "c",
    dirty_langs: &["c", "cpp"],
    resolve_binary: super::runner::clang_resolve_binary,
    build_command: super::runner::clang_build_command,
    timeout: super::runner::CLANG_SCIP_TIMEOUT,
    cache_key: clang_cache_key,
    cache_file_name: "scip-clang.cache",
};

fn clang_cache_key(bin: &Path, root: &Path, dirty: &[String]) -> String {
    super::cache::overlay_cache_key(
        &super::runner::binary_version(bin),
        &super::runner::clang_toolchain_fingerprint(root),
        "", // no lockfile analog — compile_commands.json below is the real dependency-shape input
        &clang_compile_commands_hash(root),
        dirty,
    )
}

/// Hashes whichever `compile_commands.json` `clang_build_command` will
/// actually pass to `--compdb-path` (see `runner::find_compile_commands`)
/// — a regenerated compilation database (new source file, changed compiler
/// flags, new include path) can change `scip-clang`'s output even with the
/// toolchain and `dirty` files otherwise unchanged, so it has to be part
/// of the cache key, not just a presence check.
fn clang_compile_commands_hash(root: &Path) -> String {
    super::runner::find_compile_commands(root)
        .and_then(|p| std::fs::read_to_string(p).ok())
        .map(|s| crate::indexer::pipeline::hash_content(&s))
        .unwrap_or_default()
}
