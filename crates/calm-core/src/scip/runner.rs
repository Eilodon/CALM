//! Detect a `rust-analyzer` binary and drive its batch `scip` subcommand.
//! Detect-once, fail-silent (ADR-0004 §2): any failure returns None/Err and the
//! caller keeps the syntactic graph untouched.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use super::provider::ScipProvider;

/// Total wall-clock budget for one `rust-analyzer scip` pass. Measured cost on
/// the `ci` workspace itself (~44k occurrences) was 21.5s; ripgrep 20s. 120s
/// leaves generous headroom; overrun → kill and keep whatever the syntactic
/// tier already produced.
pub const SCIP_TIMEOUT: Duration = Duration::from_secs(120);

/// Resolve a usable rust-analyzer binary path. Tries, in order: an explicit
/// override, `PATH`, `rustup which`, and the VS Code extension bundle.
/// `root` scopes the `rustup which` probe to the project directory so a
/// `rust-toolchain.toml` override there is honored instead of whatever
/// toolchain happens to be active in the server process's own cwd.
pub fn resolve_binary(override_bin: Option<&str>, root: &Path) -> Option<PathBuf> {
    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Some(b) = override_bin {
        candidates.push(PathBuf::from(b));
    }
    candidates.push(PathBuf::from("rust-analyzer")); // PATH lookup via which-style probe
    if let Some(path) = rustup_which(root) {
        candidates.push(path);
    }
    if let Some(home) = dirs_home()
        && let Some(p) = newest_vscode_ra(&home)
    {
        candidates.push(p);
    }
    candidates.into_iter().find(|c| binary_runs(c))
}

/// Run `<bin> scip <root> --output <out>` under the time budget. Returns the
/// output path on success. Never propagates a panic; a non-zero exit or timeout
/// is an `Err` the caller swallows.
pub fn rust_build_command(bin: &Path, root: &Path, out: &Path) -> Command {
    let mut cmd = Command::new(bin);
    cmd.arg("scip")
        .arg(root)
        .arg("--output")
        .arg(out)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    cmd
}

/// Spawn `provider.build_command(bin, root, out)` and poll it to completion,
/// killing on overrun. Generic over every provider — this is the one place
/// that owns "run an external indexer binary, bounded", so a Java/clang
/// provider with a much larger `timeout` doesn't need its own copy of this
/// poll loop.
pub fn run_indexer(
    provider: &ScipProvider,
    bin: &Path,
    root: &Path,
    out: &Path,
) -> anyhow::Result<()> {
    let mut child = (provider.build_command)(bin, root, out).spawn()?;
    // Poll with a deadline; kill on overrun (Command has no built-in timeout —
    // same pattern as analysis/diff_impact.rs's bounded wait).
    let deadline = std::time::Instant::now() + provider.timeout;
    loop {
        if let Some(status) = child.try_wait()? {
            if status.success() {
                return Ok(());
            }
            anyhow::bail!("{} indexer exited with {status}", provider.lang);
        }
        if std::time::Instant::now() >= deadline {
            let _ = child.kill();
            anyhow::bail!(
                "{} indexer exceeded {}s budget",
                provider.lang,
                provider.timeout.as_secs()
            );
        }
        std::thread::sleep(Duration::from_millis(200));
    }
}

/// Total wall-clock budget for one `scip-go index` pass. Go's compiler-driven
/// analysis (`go/packages` loading + typechecking) is typically slower per
/// LOC than rust-analyzer's incremental LSP pass, so this gets a larger
/// budget than `SCIP_TIMEOUT`; still bounded so a pathological module graph
/// can't hang the watcher/CLI indefinitely.
pub const GO_SCIP_TIMEOUT: Duration = Duration::from_secs(180);

/// Resolve a usable `scip-go` binary: an explicit override, then `PATH`, then
/// `$HOME/go/bin` and `$GOBIN` (where `go install .../scip-go@latest` lands
/// when neither is already on `PATH` — the common case for a freshly
/// installed toolchain).
pub fn go_resolve_binary(override_bin: Option<&str>, _root: &Path) -> Option<PathBuf> {
    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Some(b) = override_bin {
        candidates.push(PathBuf::from(b));
    }
    candidates.push(PathBuf::from("scip-go")); // PATH lookup via which-style probe
    if let Some(gobin) = std::env::var_os("GOBIN") {
        candidates.push(PathBuf::from(gobin).join("scip-go"));
    }
    if let Some(home) = dirs_home() {
        candidates.push(home.join("go").join("bin").join("scip-go"));
    }
    candidates.into_iter().find(|c| binary_runs(c))
}

/// `scip-go index --module-root <root> --output <out> --quiet`. `module-root`
/// (rather than relying on cwd) is what lets `run_indexer`'s spawn work
/// regardless of the calling process's own working directory.
pub fn go_build_command(bin: &Path, root: &Path, out: &Path) -> Command {
    let mut cmd = Command::new(bin);
    cmd.arg("index")
        .arg("--module-root")
        .arg(root)
        .arg("--output")
        .arg(out)
        .arg("--quiet")
        .current_dir(root)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    cmd
}

/// `go version`, trimmed, or `""` if it can't be run. Part of the Go overlay
/// cache key alongside `binary_version` (the scip-go binary itself) — a
/// different active Go toolchain can change typechecking results even with
/// the same scip-go version.
pub fn go_toolchain_fingerprint(root: &Path) -> String {
    Command::new("go")
        .arg("version")
        .current_dir(root)
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_default()
}

/// Total wall-clock budget for one `scip-python index` pass. Pyright's
/// (the type checker scip-python wraps) full-project inference is typically
/// slower than Go's compiler-driven pass, so this gets the largest budget of
/// the three providers; still bounded so a pathological dependency graph
/// can't hang the watcher/CLI indefinitely.
pub const PYTHON_SCIP_TIMEOUT: Duration = Duration::from_secs(240);

/// Resolve a way to run `scip-python`: an explicit override, then a
/// standalone binary on `PATH` (e.g. `npm install -g @sourcegraph/scip-python`),
/// then `npx` itself as a proxy for "run the npm package on demand" — the
/// plan's own "npm package (cần node) — probe cả binary lẫn npx" requirement,
/// since most checkouts won't have it installed globally. When the returned
/// path's filename is `npx` (not a real `scip-python` binary),
/// `python_build_command`/`python_binary_version` know to prepend the
/// package name themselves.
pub fn python_resolve_binary(override_bin: Option<&str>, _root: &Path) -> Option<PathBuf> {
    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Some(b) = override_bin {
        candidates.push(PathBuf::from(b));
    }
    candidates.push(PathBuf::from("scip-python")); // PATH lookup via which-style probe
    if let Some(c) = candidates.into_iter().find(|c| binary_runs(c)) {
        return Some(c);
    }
    let npx = PathBuf::from("npx");
    npx_can_run_scip_python(&npx).then_some(npx)
}

/// Whether `npx --yes @sourcegraph/scip-python --version` succeeds — `--yes`
/// auto-confirms the on-demand download (npx would otherwise prompt
/// interactively on a first run, hanging a non-interactive agent/CI
/// process). Real network/npm-cache round trip, not a cheap check, but this
/// is exactly the "does this actually work" probe the plan calls for.
fn npx_can_run_scip_python(npx: &Path) -> bool {
    probe_succeeds(Command::new(npx).args(["--yes", "@sourcegraph/scip-python", "--version"]))
}

/// `scip-python index --cwd <root> --project-name <name> --project-version
/// 0.0.0 --output <out> --quiet`. `--project-name`/`--project-version` are
/// NOT optional in practice — confirmed experimentally against this exact
/// package version: omitting either one crashes indexing entirely
/// (`TypeError: Cannot read properties of undefined (reading 'indexOf')` in
/// `normalizeNameOrVersion`) whenever `root` isn't a git repository scip-python
/// can infer a version from. The actual name/version values don't matter for
/// call-graph purposes, only their presence — `--project-version` is a fixed
/// placeholder rather than trying to infer a real one.
pub fn python_build_command(bin: &Path, root: &Path, out: &Path) -> Command {
    let mut cmd = Command::new(bin);
    if is_npx(bin) {
        // Fail-closed, not `--yes` — same rationale as js_build_command
        // above (audit-design finding #4a): don't let npx silently reach
        // the npm registry for @sourcegraph/scip-python.
        cmd.args(["--no-install", "@sourcegraph/scip-python"]);
    }
    let project_name = root
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("project");
    cmd.arg("index")
        .arg("--cwd")
        .arg(root)
        .arg("--project-name")
        .arg(project_name)
        .arg("--project-version")
        .arg("0.0.0")
        .arg("--output")
        .arg(out)
        .arg("--quiet")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    cmd
}

/// scip-python's own `--version` output, trimmed, or `""` if it can't be
/// run — routed through `npx` (same package-name prefix as
/// `python_build_command`) when `bin` is the `npx` proxy, so the cache key
/// reflects the actual indexer version, not npx's own unrelated version.
pub fn python_binary_version(bin: &Path) -> String {
    let mut cmd = Command::new(bin);
    if is_npx(bin) {
        cmd.args(["--yes", "@sourcegraph/scip-python", "--version"]);
    } else {
        cmd.arg("--version");
    }
    cmd.output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_default()
}

fn is_npx(bin: &Path) -> bool {
    bin.file_name().and_then(|f| f.to_str()) == Some("npx")
}

/// `python3 --version` (falling back to `python --version`), trimmed, or
/// `""` if neither can be run. Part of the Python overlay cache key
/// alongside `python_binary_version` — a different active interpreter/
/// installed packages can change pyright's inference even with the same
/// scip-python version.
pub fn python_toolchain_fingerprint(root: &Path) -> String {
    for interpreter in ["python3", "python"] {
        if let Some(out) = Command::new(interpreter)
            .arg("--version")
            .current_dir(root)
            .output()
            .ok()
            .filter(|o| o.status.success())
        {
            return String::from_utf8_lossy(&out.stdout).trim().to_string();
        }
    }
    String::new()
}

/// Total wall-clock budget for one `scip-typescript index` pass. TS
/// type-checking (the `tsc`-driven pass scip-typescript wraps) is
/// comparable in cost to Go's compiler-driven pass, so this reuses Go's
/// budget rather than Python's larger one; still bounded so a pathological
/// project graph can't hang the watcher/CLI indefinitely.
pub const JS_SCIP_TIMEOUT: Duration = Duration::from_secs(180);

/// Resolve a way to run `scip-typescript`: an explicit override, then a
/// standalone binary on `PATH` (e.g. `npm install -g @sourcegraph/scip-typescript`),
/// then `npx` as a proxy for "run the npm package on demand" — same
/// reasoning and fallback shape as `python_resolve_binary`, since most
/// checkouts won't have it installed globally either.
pub fn js_resolve_binary(override_bin: Option<&str>, _root: &Path) -> Option<PathBuf> {
    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Some(b) = override_bin {
        candidates.push(PathBuf::from(b));
    }
    candidates.push(PathBuf::from("scip-typescript")); // PATH lookup via which-style probe
    if let Some(c) = candidates.into_iter().find(|c| binary_runs(c)) {
        return Some(c);
    }
    let npx = PathBuf::from("npx");
    npx_can_run_scip_typescript(&npx).then_some(npx)
}

/// Whether `npx --yes @sourcegraph/scip-typescript --version` succeeds —
/// same `--yes` auto-confirm reasoning as `npx_can_run_scip_python`.
fn npx_can_run_scip_typescript(npx: &Path) -> bool {
    probe_succeeds(Command::new(npx).args(["--yes", "@sourcegraph/scip-typescript", "--version"]))
}

/// `scip-typescript index --infer-tsconfig --cwd <root> --output <out>
/// --no-progress-bar`. `--infer-tsconfig` lets plain-JS projects (no
/// `tsconfig.json`) index without one, which is the common case for the
/// `MAX_CALLEE_CANDIDATES` fixture-style projects this overlay targets first
/// — confirmed experimentally against this exact package version on a
/// `tsconfig`-less fixture (a `tsconfig.json` is generated as a side effect,
/// harmless). Unlike `scip-go`/`scip-python`, this CLI has no `--quiet`
/// flag at all (confirmed via `--help` against this exact package version —
/// passing one makes the whole command fail with `unknown option
/// '--quiet'`, silently no-op'd by `run_indexer`'s fail-open error
/// handling); `--no-progress-bar` is the closest real flag, and stdout/
/// stderr are still redirected below regardless.
pub fn js_build_command(bin: &Path, root: &Path, out: &Path) -> Command {
    let mut cmd = Command::new(bin);
    if is_npx(bin) {
        // Fail-closed, not `--yes`: `--yes` lets npx silently fetch
        // @sourcegraph/scip-typescript from the npm registry on first use —
        // a real, CALM-controlled network path (audit-design finding #4a,
        // docs/superskills/specs/2026-07-11-superskills-inspired-features.md).
        // `--no-install` makes npx use an already-cached/installed copy only
        // and fail loudly otherwise, matching the "local-only unless the
        // user explicitly opted in" posture the rest of CALM already holds.
        cmd.args(["--no-install", "@sourcegraph/scip-typescript"]);
    }
    cmd.arg("index")
        .arg("--infer-tsconfig")
        .arg("--no-progress-bar")
        .arg("--cwd")
        .arg(root)
        .arg("--output")
        .arg(out)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    cmd
}

/// scip-typescript's own `--version` output, trimmed, or `""` if it can't be
/// run — routed through `npx` (same package-name prefix as
/// `js_build_command`) when `bin` is the `npx` proxy, mirroring
/// `python_binary_version`.
pub fn js_binary_version(bin: &Path) -> String {
    let mut cmd = Command::new(bin);
    if is_npx(bin) {
        cmd.args(["--yes", "@sourcegraph/scip-typescript", "--version"]);
    } else {
        cmd.arg("--version");
    }
    cmd.output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_default()
}

/// `node --version`, trimmed, or `""` if it can't be run. Part of the JS/TS
/// overlay cache key alongside `js_binary_version` — a different active
/// Node runtime can change scip-typescript's TS-compiler-driven inference
/// even with the same scip-typescript version.
pub fn js_toolchain_fingerprint(root: &Path) -> String {
    Command::new("node")
        .arg("--version")
        .current_dir(root)
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_default()
}

/// Total wall-clock budget for one `scip-java index` pass. `scip-java` drives
/// a real Maven/Gradle build (compiling the whole project through a
/// semanticdb compiler plugin) rather than an incremental analysis the way
/// rust-analyzer/scip-go/scip-typescript do, so this gets by far the largest
/// budget of any provider — matches the plan doc's own risk note that a
/// full-build indexer like this must never run on the hot edit-save path
/// (see `JavaConfig`'s default `MinInterval` policy, not `OnSave`).
pub const JAVA_SCIP_TIMEOUT: Duration = Duration::from_secs(600);

/// Resolve a usable `scip-java` launcher: an explicit override, then `PATH`.
/// `scip-java` has no standalone-binary release the way `scip-go`/rust-
/// analyzer do — real installs are `cs bootstrap com.sourcegraph:scip-java_2.13:<version>
/// -o scip-java` (coursier) or the `sourcegraph/scip-java` Docker image
/// wrapped in a shim script, both of which land a `scip-java` launcher on
/// `PATH` either way, so a single `PATH` probe covers both documented
/// install paths without this needing to know which one produced it.
pub fn java_resolve_binary(override_bin: Option<&str>, _root: &Path) -> Option<PathBuf> {
    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Some(b) = override_bin {
        candidates.push(PathBuf::from(b));
    }
    candidates.push(PathBuf::from("scip-java")); // PATH lookup via which-style probe
    candidates.into_iter().find(|c| binary_runs(c))
}

/// `scip-java index --output <out>`, run with `root` as the working
/// directory — confirmed via the real tool that `index` has no `--cwd`/
/// `--module-root` flag at all (unlike `scip-go`/`scip-typescript`); it
/// always indexes "the current working directory" (its own `--help` text),
/// so `current_dir(root)` is the only way to scope it, mirroring
/// `go_build_command`'s same use of `current_dir` as a belt-and-braces
/// measure alongside its own `--module-root` flag.
pub fn java_build_command(bin: &Path, root: &Path, out: &Path) -> Command {
    let mut cmd = Command::new(bin);
    cmd.arg("index")
        .arg("--output")
        .arg(out)
        .current_dir(root)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    cmd
}

/// `java -version`'s output, trimmed, or `""` if it can't be run. Part of the
/// Java overlay cache key alongside `binary_version` (the scip-java launcher
/// itself) — a different active JDK can change semanticdb compiler-plugin
/// output even with the same scip-java version. Unlike `go version`/`node
/// --version`, `java -version` writes to **stderr**, not stdout — confirmed
/// empirically (`java -version > out 2> err` puts the version text in
/// `err`); every other `*_toolchain_fingerprint` in this file reads stdout,
/// so this one deliberately doesn't share their pattern.
pub fn java_toolchain_fingerprint(root: &Path) -> String {
    Command::new("java")
        .arg("-version")
        .current_dir(root)
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stderr).trim().to_string())
        .unwrap_or_default()
}

/// Total wall-clock budget for one `scip-dotnet index` pass. Like
/// `scip-java`, this drives a real build (`dotnet restore` + Roslyn
/// compilation) rather than an incremental analysis, so it gets a
/// comparably large budget — smaller than Java's only because `dotnet
/// restore` is typically faster than a Maven/Gradle build resolving from
/// scratch.
pub const CSHARP_SCIP_TIMEOUT: Duration = Duration::from_secs(300);

/// Resolve a usable `scip-dotnet` launcher: an explicit override, then
/// `PATH`, then `$HOME/.dotnet/tools` (where `dotnet tool install --global
/// scip-dotnet` lands when that directory isn't already on `PATH` — the
/// common case for a freshly installed SDK, mirroring `go_resolve_binary`'s
/// `$HOME/go/bin` fallback for the same reason).
pub fn csharp_resolve_binary(override_bin: Option<&str>, _root: &Path) -> Option<PathBuf> {
    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Some(b) = override_bin {
        candidates.push(PathBuf::from(b));
    }
    candidates.push(PathBuf::from("scip-dotnet")); // PATH lookup via which-style probe
    if let Some(home) = dirs_home() {
        candidates.push(home.join(".dotnet").join("tools").join("scip-dotnet"));
    }
    candidates.into_iter().find(|c| binary_runs(c))
}

/// The `.sln`/`.csproj` file `scip-dotnet index` should be pointed at —
/// unlike every other provider here, `scip-dotnet`'s `index` subcommand
/// takes a project/solution *file* as a positional argument, not just a
/// directory (confirmed via `scip-dotnet index --help`: `<projects>` is
/// "Path to the .sln (solution) or .csproj/.vbproj file"). Prefers a `.sln`
/// over a bare `.csproj` when both exist at the top level — a solution
/// covers every project it references, a single `.csproj` covers only
/// itself; mirrors the "Markers" column's own `*.sln`/`*.csproj` ordering.
/// V1 simplification (matches Go's "V1 single-module" cut): only the
/// project root's own top level is scanned, not recursively — a checkout
/// with its `.sln` nested in a subdirectory needs `binary` override support
/// added later, not guessed at here.
pub fn find_csharp_project(root: &Path) -> Option<PathBuf> {
    let entries: Vec<PathBuf> = std::fs::read_dir(root)
        .ok()?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .collect();
    entries
        .iter()
        .find(|p| p.extension().is_some_and(|e| e == "sln"))
        .or_else(|| {
            entries
                .iter()
                .find(|p| p.extension().is_some_and(|e| e == "csproj"))
        })
        .cloned()
}

/// `scip-dotnet index <project> --output <out> --working-directory <root>`.
/// `--working-directory` (rather than relying on cwd) is what lets
/// `run_indexer`'s spawn work regardless of the calling process's own
/// working directory, mirroring `go_build_command`'s use of both an
/// explicit root flag and `current_dir`. When no `.sln`/`.csproj` is found,
/// points at a sentinel path inside `root` instead of skipping the run
/// entirely (this fn can't return `Option`/`Result` — `ScipProvider`'s
/// `build_command` field is infallible) — `scip-dotnet` fails fast on a
/// nonexistent project file, which `run_indexer` already treats as a
/// non-fatal warning, same outcome as every other provider's "nothing to
/// index here" case.
pub fn csharp_build_command(bin: &Path, root: &Path, out: &Path) -> Command {
    let project = find_csharp_project(root).unwrap_or_else(|| root.join("__no_project_found__"));
    let mut cmd = Command::new(bin);
    cmd.arg("index")
        .arg(&project)
        .arg("--output")
        .arg(out)
        .arg("--working-directory")
        .arg(root)
        .current_dir(root)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    cmd
}

/// `dotnet --version`, trimmed, or `""` if it can't be run. Part of the C#
/// overlay cache key alongside `binary_version` (the scip-dotnet tool
/// itself) — a different active SDK can change Roslyn compilation output
/// even with the same scip-dotnet version.
pub fn csharp_toolchain_fingerprint(root: &Path) -> String {
    Command::new("dotnet")
        .arg("--version")
        .current_dir(root)
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_default()
}

/// Total wall-clock budget for one `scip-php` pass. It's a pure static AST
/// walk (`nikic/php-parser`, no compiler/build-tool invocation) — the
/// lightest indexer of any provider here — but still gets a real budget
/// rather than Rust's 120s baseline, since a large PHP codebase's file
/// count (not compilation) is what drives its wall time.
pub const PHP_SCIP_TIMEOUT: Duration = Duration::from_secs(180);

/// Resolve a usable `scip-php` launcher: an explicit override, then
/// `<root>/vendor/bin/scip-php` (a project-local Composer dependency,
/// matching the plan's own stated preference for a per-project pinned
/// version — this is also where Composer's own bin-proxy mechanism places
/// it when `davidrjenni/scip-php` is required as a `require-dev`
/// dependency), then a global `PATH` lookup. Gated on `root` actually
/// having a usable Composer autoloader first (`vendor/autoload.php`) —
/// without one, `scip-php`'s `Indexer` can't resolve PSR-4 namespaces to
/// real files at all, so probing for the binary at all would be pointless
/// (matches the plan's own "Không autoload → silent skip" prereq).
pub fn php_resolve_binary(override_bin: Option<&str>, root: &Path) -> Option<PathBuf> {
    if override_bin.is_none() && !root.join("vendor").join("autoload.php").is_file() {
        return None;
    }
    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Some(b) = override_bin {
        candidates.push(PathBuf::from(b));
    }
    candidates.push(root.join("vendor").join("bin").join("scip-php"));
    candidates.push(PathBuf::from("scip-php")); // PATH lookup
    candidates.into_iter().find(|c| php_binary_runs(c))
}

/// `scip-php`'s own probe, deliberately **not** the shared `binary_runs`
/// (which drives `<bin> --version`) — confirmed via the real CLI that
/// `scip-php`'s `getopt('h', ['help', 'memory-limit:'])` silently ignores
/// an unrecognized `--version` flag rather than erroring on it, so that
/// probe falls through past the help/exit branch and **runs the real
/// indexer** against whatever the current directory happens to be
/// (reproduced: `scip-php --version` run from `/tmp` crashed trying to
/// read a nonexistent `/tmp/composer.json` — proof it had already started
/// indexing). `--help` is the one flag the CLI does handle safely.
fn php_binary_runs(path: &Path) -> bool {
    Command::new(path)
        .arg("--help")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Stand-in for `binary_version` (unusable here for the same `--version`
/// safety reason `php_binary_runs` documents): hashes the resolved
/// launcher file's own bytes. V1 limitation, stated plainly: when `bin` is
/// the thin `vendor/bin/scip-php` proxy Composer generates, this only
/// reliably changes when Composer regenerates that proxy (e.g. a version
/// bump via `composer update`), not on every conceivable upstream change —
/// there's no `--version` output to do better with.
pub fn php_binary_version(bin: &Path) -> String {
    std::fs::read(bin)
        .ok()
        .map(|bytes| crate::indexer::pipeline::hash_content(&String::from_utf8_lossy(&bytes)))
        .unwrap_or_default()
}

/// `scip-php` has no `--output` flag at all (confirmed via `--help` and its
/// own source — `bin/scip-php` hardcodes `file_put_contents('index.scip',
/// ...)` relative to `getcwd()`), unlike every other provider here. Run
/// through a shell wrapper that `cd`s to `root` (so `getcwd()` inside the
/// PHP process becomes the project root scip-php's `Metadata.project_root`
/// needs), runs the indexer, then moves its fixed `index.scip` output to
/// the caller-chosen `out` path — the only way to satisfy
/// `ScipProvider::build_command`'s "produces output at `out`" contract
/// against a tool that can't be told where to write. Paths are
/// single-quote-shell-escaped (`shell_quote`) since they're interpolated
/// into a `sh -c` string rather than passed as separate argv entries.
pub fn php_build_command(bin: &Path, root: &Path, out: &Path) -> Command {
    let script = format!("{} && mv index.scip {}", shell_quote(bin), shell_quote(out));
    let mut cmd = Command::new("sh");
    cmd.arg("-c")
        .arg(script)
        .current_dir(root)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    cmd
}

/// POSIX single-quote shell escaping: wrap in `'...'`, and turn any
/// embedded `'` into `'\''` (close quote, escaped literal quote, reopen
/// quote) — the standard technique, since single quotes admit no escape
/// sequences of their own.
fn shell_quote(path: &Path) -> String {
    format!("'{}'", path.to_string_lossy().replace('\'', r"'\''"))
}

/// `php --version`'s first line, trimmed, or `""` if it can't be run. Part
/// of the PHP overlay cache key alongside `php_binary_version` — a
/// different active PHP runtime can change parser behavior (new syntax
/// support) even with the same scip-php version.
pub fn php_toolchain_fingerprint(root: &Path) -> String {
    Command::new("php")
        .arg("--version")
        .current_dir(root)
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| {
            String::from_utf8_lossy(&o.stdout)
                .lines()
                .next()
                .unwrap_or("")
                .trim()
                .to_string()
        })
        .unwrap_or_default()
}

/// The 9th entry in the table (Phase D.1, 2026-07-11) — `scip-ruby`
/// (Sourcegraph's fork of Sorbet) has a real prebuilt gem
/// (`gem install scip-ruby`), but its INSTALLED `bin/scip-ruby` is a thin
/// RubyGems wrapper script that hard-requires `BUNDLE_GEMFILE` and refuses
/// to run standalone ("Missing BUNDLE_GEMFILE environment variable /
/// Expected to be invoked as 'bundle exec scip-ruby'") — confirmed live: it
/// exits 1 on a bare `--version` probe too, so `binary_runs` fails closed
/// (reports "not found") rather than silently misreporting a broken
/// install as usable. `ruby_resolve_binary` therefore only ever looks for a
/// `scip-ruby` launcher already on `PATH` that works standalone — the
/// README's own "download binary and index" method (a raw platform binary,
/// not the gem wrapper) is what actually satisfies this, same shape as
/// `GO`'s `go_resolve_binary`.
pub fn ruby_resolve_binary(override_bin: Option<&str>, _root: &Path) -> Option<PathBuf> {
    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Some(b) = override_bin {
        candidates.push(PathBuf::from(b));
    }
    candidates.push(PathBuf::from("scip-ruby")); // PATH lookup via which-style probe
    candidates.into_iter().find(|c| binary_runs(c))
}

/// `scip-ruby --gem-metadata <name@version> --index-file <out> .`, run with
/// `root` as the working directory. `--index-file` takes a FILE path
/// (confirmed against the real CLI reference docs, not the possibly-stale
/// `--help` text alone: "The path for emitting the SCIP index. Defaults to
/// `index.scip`" — a directory argument here silently emits nothing).
/// `--gem-metadata` is always passed explicitly: without it, a project with
/// no `Gemfile.lock`/`.gemspec` makes scip-ruby exit 1 ("Failed to find
/// .gemspec file for identifying gem version"), which `run_indexer` would
/// treat as a hard failure — passing a synthesized `name@version` (derived
/// from `root`'s own directory name, version pinned to a constant) sidesteps
/// this unconditionally, since the value only affects cross-repo navigation
/// metadata, not same-repo occurrence resolution.
pub fn ruby_build_command(bin: &Path, root: &Path, out: &Path) -> Command {
    let name = root
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "calm-indexed-project".to_string());
    let mut cmd = Command::new(bin);
    cmd.arg("--gem-metadata")
        .arg(format!("{name}@0.0.0"))
        .arg("--index-file")
        .arg(out)
        .arg(".")
        .current_dir(root)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    cmd
}

/// Sorbet performs a real (if best-effort on `# typed: false` files) type
/// check of the whole project on every run — heavier than a pure AST walk
/// like PHP's, but not a full external build-tool invocation like Java/
/// Clang's — `CSHARP_SCIP_TIMEOUT`'s budget is the closest existing match.
pub const RUBY_SCIP_TIMEOUT: Duration = Duration::from_secs(300);

/// `scip-ruby --version`'s first line, trimmed, or `""` if it can't be run.
pub fn ruby_binary_version(bin: &Path) -> String {
    Command::new(bin)
        .arg("--version")
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| {
            String::from_utf8_lossy(&o.stdout)
                .lines()
                .next()
                .unwrap_or("")
                .trim()
                .to_string()
        })
        .unwrap_or_default()
}

/// Cache-key input analogous to `php_toolchain_fingerprint`: which Ruby
/// runtime is active can matter for how scip-ruby's own embedded parser
/// behaves, though scip-ruby bundles its own Sorbet build rather than
/// shelling out to a system `ruby` the way scip-php's underlying tool does
/// — kept for parity with the other providers' cache-key shape regardless.
pub fn ruby_toolchain_fingerprint(root: &Path) -> String {
    Command::new("ruby")
        .arg("--version")
        .current_dir(root)
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| {
            String::from_utf8_lossy(&o.stdout)
                .lines()
                .next()
                .unwrap_or("")
                .trim()
                .to_string()
        })
        .unwrap_or_default()
}

/// The 8th entry's timeout (P3.1) — same 600s budget as Java's, the other
/// provider `ScipConfig::policy`'s own doc comment names as a "heavy future
/// provider": `scip-clang` compiles and indexes one translation unit at a
/// time across a whole `compile_commands.json`, comparable in cost to a
/// real build rather than a lightweight AST walk.
pub const CLANG_SCIP_TIMEOUT: Duration = Duration::from_secs(600);

/// Resolve a usable `scip-clang` binary — **unverified live** (see
/// `provider::CLANG`'s doc comment for why): this sandbox has no way to
/// obtain a real `scip-clang` binary at all (GitHub Releases, where
/// prebuilt binaries are published, returns a hard 403 from this
/// environment's egress policy; building from source needs Bazel, not
/// available either). Written to the same shape as every other
/// `PATH`-lookup provider (`GO`/`CSHARP`) and reusing the shared
/// `binary_runs` (`<bin> --version`) probe on the (unverified) assumption
/// `scip-clang` handles `--version` safely the conventional way — unlike
/// `scip-php`, which this session proved does *not* (see
/// `php_binary_runs`). Gated on two preconditions before even probing for
/// the binary, both silent no-ops per the plan's own DoD: the host
/// platform (`clang_platform_supported`) and a discoverable
/// `compile_commands.json` (`find_compile_commands`) — `scip-clang` is
/// unusable without either, so there is no reason to shell out at all when
/// either is missing.
pub fn clang_resolve_binary(override_bin: Option<&str>, root: &Path) -> Option<PathBuf> {
    if !clang_platform_supported() || find_compile_commands(root).is_none() {
        return None;
    }
    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Some(b) = override_bin {
        candidates.push(PathBuf::from(b));
    }
    candidates.push(PathBuf::from("scip-clang")); // PATH lookup
    candidates.into_iter().find(|c| binary_runs(c))
}

/// Per the plan's own P3.1 row: `scip-clang` (built on `clangd`'s
/// cross-translation-unit indexing) only ships prebuilt binaries for Linux
/// x86_64 and macOS arm64 — no Windows-native build. Checked before any
/// binary probe so an unsupported host gets a silent, immediate skip
/// (matching every other provider's "missing prerequisite" behavior)
/// instead of a doomed `PATH` lookup.
fn clang_platform_supported() -> bool {
    matches!(
        (std::env::consts::OS, std::env::consts::ARCH),
        ("linux", "x86_64") | ("macos", "aarch64")
    )
}

/// `<root>/compile_commands.json` first (the conventional location a build
/// system that honors `CMAKE_EXPORT_COMPILE_COMMANDS=ON` — or a manually
/// generated one via `bear` for Make-based projects — writes to), else
/// `<root>/build/compile_commands.json` (CMake's own common out-of-tree
/// build-directory convention). Neither is generated by this codebase —
/// per the plan, `calm` never invokes CMake/`bear` itself, only detects an
/// already-generated compilation database.
pub fn find_compile_commands(root: &Path) -> Option<PathBuf> {
    [
        root.join("compile_commands.json"),
        root.join("build").join("compile_commands.json"),
    ]
    .into_iter()
    .find(|p| p.is_file())
}

/// `scip-clang --compdb-path {compdb} --index-output-path {out} -j {n}`
/// (flag names per `scip-clang`'s published CLI usage text — **confirmed
/// against a real `scip-clang 0.4.0` binary 2026-07-15**, see
/// `clang_overlay_upgrades_a_real_edge_on_the_c_fixture` in `scip/mod.rs`;
/// this doc comment previously carried a "not confirmed in this sandbox"
/// caveat, same as `clang_resolve_binary`'s below — both are now stale in
/// the direction of "it works", left here as history rather than silently
/// dropped). `-j` is capped at 8 rather than left unbounded or tied to
/// `available_parallelism()` uncapped: an indexer running inside `calm`'s
/// own background watcher shouldn't be free to claim every core on a large
/// CI/dev box the way a foreground `ninja -j$(nproc)` build reasonably
/// would.
pub fn clang_build_command(bin: &Path, root: &Path, out: &Path) -> Command {
    let compdb =
        find_compile_commands(root).unwrap_or_else(|| root.join("__no_compile_commands_found__"));
    let jobs = std::thread::available_parallelism()
        .map(|n| n.get().min(8))
        .unwrap_or(1);
    let mut cmd = Command::new(bin);
    cmd.arg("--compdb-path")
        .arg(&compdb)
        .arg("--index-output-path")
        .arg(out)
        .arg("-j")
        .arg(jobs.to_string())
        .current_dir(root)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    cmd
}

/// `clang --version`'s first line, trimmed, or `""` if it can't be run —
/// same role as `active_toolchain_fingerprint`/`php_toolchain_fingerprint`:
/// a different active Clang/LLVM toolchain can change indexing output even
/// with the same `scip-clang` version and an unchanged
/// `compile_commands.json`.
pub fn clang_toolchain_fingerprint(root: &Path) -> String {
    Command::new("clang")
        .arg("--version")
        .current_dir(root)
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| {
            String::from_utf8_lossy(&o.stdout)
                .lines()
                .next()
                .unwrap_or("")
                .trim()
                .to_string()
        })
        .unwrap_or_default()
}

/// Wall-clock budget for a `--version`-style *availability probe* (not a real
/// indexing run) — `binary_runs`/`npx_can_run_scip_python`/
/// `npx_can_run_scip_typescript` all go through `probe_succeeds` below so a
/// stalled/offline `npx` registry lookup can't block a status-only tool call
/// (`indexing_status`/`repo_overview`) indefinitely the way an un-timed
/// `Command::status()` call did before. 3s comfortably covers the ~1-1.5s a
/// warm `npx --yes @sourcegraph/scip-<lang> --version` costs even over a fast
/// network (measured directly against this repo's own environment), while
/// still bounding the worst case instead of leaving it unbounded.
const PROBE_TIMEOUT: Duration = Duration::from_secs(3);

/// Spawns `cmd` and reports whether it exits successfully within
/// `PROBE_TIMEOUT` — same bounded-poll shape `run_indexer` already uses for a
/// real (much longer-running) indexer invocation, just with a much shorter
/// budget appropriate for a cheap presence check. An overrun kills the child
/// and counts as unavailable rather than hanging the caller.
fn probe_succeeds(cmd: &mut Command) -> bool {
    let Ok(mut child) = cmd
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
    else {
        return false;
    };
    let deadline = std::time::Instant::now() + PROBE_TIMEOUT;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return status.success(),
            Ok(None) => {}
            Err(_) => return false,
        }
        if std::time::Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return false;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

pub(crate) fn binary_runs(path: &Path) -> bool {
    probe_succeeds(Command::new(path).arg("--version"))
}
/// `<bin> --version` output, trimmed, or `""` if it can't be run. Used as part
/// of the overlay cache key — any version change invalidates the cache.
pub fn binary_version(bin: &Path) -> String {
    Command::new(bin)
        .arg("--version")
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_default()
}

/// `rustc --version --verbose` run from `root`, trimmed, or `""` if it can't
/// be run. Used alongside `binary_version` in the overlay cache key:
/// `binary_version` fingerprints the rust-analyzer binary doing the
/// analysis, this fingerprints the toolchain/edition semantics of the
/// project being analyzed — switching active toolchain (`rustup default`,
/// `rust-toolchain.toml`) without changing which rust-analyzer binary
/// resolves must still invalidate the cache. `current_dir(root)` is what
/// makes rustup's shim resolve the project's local override instead of
/// whatever's active for the server process's own cwd.
pub fn active_toolchain_fingerprint(root: &Path) -> String {
    Command::new("rustc")
        .args(["--version", "--verbose"])
        .current_dir(root)
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_default()
}

/// `rustup which rust-analyzer` — resolves the toolchain-managed binary when
/// it's not directly on `PATH`. Run from `root` (not the server process's own
/// cwd) and without a hardcoded `--toolchain`, so rustup's own override
/// resolution (`rust-toolchain.toml`, `RUSTUP_TOOLCHAIN`, `rustup override`)
/// picks the project's actual pinned toolchain instead of always `stable`.
/// Absent/failing `rustup` is not an error here; `binary_runs` is the real gate.
fn rustup_which(root: &Path) -> Option<PathBuf> {
    let out = Command::new("rustup")
        .args(["which", "rust-analyzer"])
        .current_dir(root)
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let path = String::from_utf8(out.stdout).ok()?;
    let path = path.trim();
    if path.is_empty() {
        None
    } else {
        Some(PathBuf::from(path))
    }
}

pub(crate) fn dirs_home() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from)
}
fn newest_vscode_ra(home: &Path) -> Option<PathBuf> {
    let ext_dir = home.join(".vscode/extensions");
    let mut hits: Vec<PathBuf> = std::fs::read_dir(&ext_dir)
        .ok()?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with("rust-lang.rust-analyzer-"))
        })
        .map(|p| p.join("server/rust-analyzer"))
        .filter(|p| p.exists())
        .collect();
    hits.sort();
    hits.pop()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detect_returns_none_when_binary_absent() {
        // `resolve_binary` cascades an override -> PATH -> rustup -> VS Code
        // fallback, so a fake override alone doesn't prove absence: on any dev
        // box with rust-analyzer actually installed (as this one is), a fake
        // override would still resolve via one of those fallbacks, making the
        // end-to-end assertion flaky based on what's installed. Pin down the
        // real invariant — an absent/non-executable binary path yields a
        // negative, non-panicking result — at the underlying probe instead.
        assert!(!binary_runs(Path::new(
            "definitely-not-a-real-ra-binary-xyz"
        )));
    }

    /// Same invariant as `detect_returns_none_when_binary_absent`, pinned at
    /// the underlying probe for the same reason: this sandbox has a real
    /// `scip-go` on `PATH` (P2.1 was verified against it), so asserting
    /// `go_resolve_binary(...).is_none()` end-to-end would be flaky based on
    /// what's installed rather than testing the actual invariant — an
    /// absent/non-executable path is rejected.
    #[test]
    fn go_binary_runs_rejects_a_nonexistent_path() {
        assert!(!binary_runs(Path::new(
            "definitely-not-a-real-scip-go-binary-xyz"
        )));
    }

    /// `is_npx` gates whether `python_build_command`/`python_binary_version`
    /// prepend the `@sourcegraph/scip-python` package name — must key off
    /// the resolved binary's filename only (`npx` vs a real `scip-python`
    /// binary), not assume one or the other.
    #[test]
    fn is_npx_true_only_for_a_path_named_npx() {
        assert!(is_npx(Path::new("npx")));
        assert!(is_npx(Path::new("/usr/local/bin/npx")));
        assert!(!is_npx(Path::new("scip-python")));
        assert!(!is_npx(Path::new("/usr/local/bin/scip-python")));
    }

    /// Same invariant as `go_binary_runs_rejects_a_nonexistent_path`, pinned
    /// at the underlying probe for the same reason: a sandbox with a real
    /// `scip-typescript` reachable via `npx` would make an end-to-end
    /// `js_resolve_binary(...).is_none()` assertion flaky.
    #[test]
    fn js_binary_runs_rejects_a_nonexistent_path() {
        assert!(!binary_runs(Path::new(
            "definitely-not-a-real-scip-typescript-binary-xyz"
        )));
    }

    /// Same invariant as `go_binary_runs_rejects_a_nonexistent_path`/
    /// `js_binary_runs_rejects_a_nonexistent_path`, pinned at the underlying
    /// probe for the same reason.
    #[test]
    fn java_binary_runs_rejects_a_nonexistent_path() {
        assert!(!binary_runs(Path::new(
            "definitely-not-a-real-scip-java-binary-xyz"
        )));
    }

    /// Same invariant, for the C# provider added in P2.3.
    #[test]
    fn csharp_binary_runs_rejects_a_nonexistent_path() {
        assert!(!binary_runs(Path::new(
            "definitely-not-a-real-scip-dotnet-binary-xyz"
        )));
    }

    #[test]
    fn find_csharp_project_prefers_sln_over_csproj() {
        let dir = std::env::temp_dir().join(format!("ci_find_csproj_sln_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("App.csproj"), "").unwrap();
        std::fs::write(dir.join("App.sln"), "").unwrap();
        let found = find_csharp_project(&dir).unwrap();
        assert_eq!(found.extension().unwrap(), "sln");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn find_csharp_project_falls_back_to_csproj_when_no_sln() {
        let dir = std::env::temp_dir().join(format!("ci_find_csproj_only_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("App.csproj"), "").unwrap();
        let found = find_csharp_project(&dir).unwrap();
        assert_eq!(found.extension().unwrap(), "csproj");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn find_csharp_project_returns_none_when_neither_exists() {
        let dir = std::env::temp_dir().join(format!("ci_find_csproj_none_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        assert!(find_csharp_project(&dir).is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Same invariant as the other providers' equivalents, but pinned at
    /// `php_binary_runs` (not the shared `binary_runs`) since PHP uses its
    /// own probe — see `php_binary_runs`'s doc comment for why.
    #[test]
    fn php_binary_runs_rejects_a_nonexistent_path() {
        assert!(!php_binary_runs(Path::new(
            "definitely-not-a-real-scip-php-binary-xyz"
        )));
    }

    /// The real bug this session found: `scip-php --version` is NOT a safe
    /// probe (silently falls through to a real indexing run instead of
    /// erroring/printing a version) — pinned here as a regression test
    /// against ever "simplifying" `php_resolve_binary`/`php_binary_version`
    /// back to the shared `binary_runs`/`binary_version` helpers.
    #[test]
    fn php_binary_runs_uses_help_not_version() {
        // A minimal script that succeeds on --help but fails (nonzero exit)
        // on anything else, including --version — mirrors scip-php's real
        // shape closely enough to prove `php_binary_runs` passes the right
        // flag, without needing a real PHP interpreter in this unit test.
        let dir = std::env::temp_dir().join(format!("ci_php_probe_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let script = dir.join("fake-scip-php");
        std::fs::write(
            &script,
            "#!/bin/sh\nif [ \"$1\" = \"--help\" ]; then exit 0; else exit 1; fi\n",
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = std::fs::metadata(&script).unwrap().permissions();
            perms.set_mode(0o755);
            std::fs::set_permissions(&script, perms).unwrap();
        }
        assert!(
            php_binary_runs(&script),
            "must probe with --help, which this fake binary accepts"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn shell_quote_escapes_embedded_single_quotes() {
        let p = Path::new("/tmp/it's a path/index.scip");
        let quoted = shell_quote(p);
        assert_eq!(quoted, r"'/tmp/it'\''s a path/index.scip'");
    }

    #[test]
    fn php_resolve_binary_returns_none_without_autoload_even_if_bin_exists() {
        let dir = std::env::temp_dir().join(format!("ci_php_no_autoload_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("vendor").join("bin")).unwrap();
        std::fs::write(
            dir.join("vendor").join("bin").join("scip-php"),
            "#!/bin/sh\nexit 0\n",
        )
        .unwrap();
        // No vendor/autoload.php written — the gate must reject this
        // regardless of the binary being present and runnable.
        assert!(php_resolve_binary(None, &dir).is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Same invariant as the other providers' equivalents (P3.1) — pinned
    /// even though the real `scip-clang --version` behavior is unverified
    /// in this sandbox (see `clang_resolve_binary`'s doc comment): the
    /// shared `binary_runs` probe must still reject an obviously
    /// nonexistent path regardless.
    #[test]
    fn clang_binary_runs_rejects_a_nonexistent_path() {
        assert!(!binary_runs(Path::new(
            "definitely-not-a-real-scip-clang-binary-xyz"
        )));
    }

    #[test]
    fn find_compile_commands_prefers_root_over_build_subdir() {
        let dir = std::env::temp_dir().join(format!("ci_compdb_root_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("build")).unwrap();
        std::fs::write(dir.join("compile_commands.json"), "[]").unwrap();
        std::fs::write(dir.join("build").join("compile_commands.json"), "[]").unwrap();
        let found = find_compile_commands(&dir).unwrap();
        assert_eq!(found, dir.join("compile_commands.json"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn find_compile_commands_falls_back_to_build_subdir() {
        let dir = std::env::temp_dir().join(format!("ci_compdb_build_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("build")).unwrap();
        std::fs::write(dir.join("build").join("compile_commands.json"), "[]").unwrap();
        let found = find_compile_commands(&dir).unwrap();
        assert_eq!(found, dir.join("build").join("compile_commands.json"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn find_compile_commands_returns_none_when_neither_exists() {
        let dir = std::env::temp_dir().join(format!("ci_compdb_none_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        assert!(find_compile_commands(&dir).is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// `clang_resolve_binary` must reject a project with no discoverable
    /// `compile_commands.json` before ever probing for the binary — same
    /// "cheap gate before any subprocess" shape `php_resolve_binary` uses
    /// for `vendor/autoload.php`. Deterministic regardless of platform: an
    /// absent compdb is a no-op on every OS/arch, so this doesn't need to
    /// special-case the platform gate.
    #[test]
    fn clang_resolve_binary_returns_none_without_compile_commands() {
        let dir = std::env::temp_dir().join(format!("ci_clang_no_compdb_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        assert!(clang_resolve_binary(None, &dir).is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn clang_build_command_caps_parallelism_at_eight() {
        let dir = std::env::temp_dir().join(format!("ci_clang_build_cmd_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("compile_commands.json"), "[]").unwrap();
        let out = dir.join("index.scip");
        let cmd = clang_build_command(Path::new("scip-clang"), &dir, &out);
        let args: Vec<String> = cmd
            .get_args()
            .map(|a| a.to_string_lossy().to_string())
            .collect();
        let j_pos = args.iter().position(|a| a == "-j").expect("must pass -j");
        let jobs: usize = args[j_pos + 1].parse().unwrap();
        assert!(
            (1..=8).contains(&jobs),
            "expected 1..=8, got {jobs} (args: {args:?})"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
