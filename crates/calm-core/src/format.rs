//! Rust source formatting via `rustfmt`, invoked over stdin only — never as
//! a positional file argument.
//!
//! 2026-07-14 self-audit finding: an agent working on this exact repo ran
//! `rustfmt <files...>` directly via a shell (CALM has no format tool of
//! its own, so that was the only option), and it silently reformatted a
//! file that was never in the argument list. Root cause, verified live:
//! `rustfmt` resolves the owning Cargo *package* for any positional file
//! argument and then walks that package's `mod` tree from its crate root,
//! reformatting every file it discovers along the way — not just the
//! files actually passed on the command line. Confirmed by reproducing it
//! (`rustfmt tools.rs tools/common.rs` also silently rewrote the unrelated
//! sibling `tools/edit.rs`, reachable via `tools.rs`'s own `mod edit;`).
//!
//! `rustfmt` with **no positional file argument at all** — content piped
//! in over stdin, formatted text read back from stdout — has no package to
//! resolve and does zero filesystem discovery; it can only ever affect the
//! exact bytes it was handed. This is the same invocation editor
//! integrations (rustfmt.el, vim-rustfmt, format-on-paste plugins) already
//! rely on for "format this buffer, nothing else". `format_rust_source`
//! below is that invocation; the caller (calm-server's `format_files` tool)
//! is responsible for writing the result back to exactly the one file it
//! came from via the same atomic-write path every other CALM edit uses.

use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

/// Formats `content` (the full text of one Rust source file) via `rustfmt`
/// over stdin/stdout — see the module doc comment for why this specific
/// invocation shape (no file argument) is load-bearing, not incidental.
/// Returns the formatted text, or an error string (rustfmt not on PATH,
/// non-zero exit — most commonly a genuine syntax error rustfmt itself
/// can't parse, or non-UTF8 output).
pub fn format_rust_source(content: &str, edition: &str) -> Result<String, String> {
    let mut child = Command::new("rustfmt")
        .arg("--edition")
        .arg(edition)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("failed to spawn rustfmt: {e} (is it installed and on PATH?)"))?;

    {
        let stdin = child
            .stdin
            .as_mut()
            .ok_or_else(|| "failed to open rustfmt stdin".to_string())?;
        stdin
            .write_all(content.as_bytes())
            .map_err(|e| format!("failed to write to rustfmt stdin: {e}"))?;
    }

    let output = child
        .wait_with_output()
        .map_err(|e| format!("failed to wait for rustfmt: {e}"))?;
    if !output.status.success() {
        return Err(format!(
            "rustfmt exited with {}: {}",
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    String::from_utf8(output.stdout).map_err(|e| format!("rustfmt produced non-UTF8 output: {e}"))
}

const DEFAULT_EDITION: &str = "2021";

/// Best-effort Rust edition detection for `file_path`, walking upward
/// through its ancestor directories (stopping at `project_root`) looking
/// for the nearest `Cargo.toml`. Line-based scanning, not a real TOML
/// parser — every case this needs to distinguish (`edition = "…"`,
/// `edition.workspace = true`, `[workspace.package]`) is a single
/// top-level-ish line in practice, and a heuristic miss only costs a
/// wrong `--edition` guess (caught immediately as a real rustfmt parse
/// error on genuinely edition-gated syntax, per this module's own
/// `format_rust_source`), never a silent wrong *write* — nothing here
/// touches file content.
///
/// Resolution order, mirroring how `cargo` itself resolves a crate's
/// edition: (1) the nearest ancestor `Cargo.toml`'s own bare
/// `edition = "…"`; (2) if that same file instead has
/// `edition.workspace = true`, keep walking upward for a `[workspace]`
/// root and read `[workspace.package]`'s `edition = "…"` there; (3)
/// `DEFAULT_EDITION` if nothing resolves either way (no Cargo.toml found,
/// or the workspace root itself doesn't pin one).
pub fn detect_rust_edition(file_path: &Path, project_root: &Path) -> String {
    let mut dir = file_path.parent();
    while let Some(d) = dir {
        let cargo_toml = d.join("Cargo.toml");
        if let Ok(text) = std::fs::read_to_string(&cargo_toml) {
            if let Some(edition) = bare_edition_field(&text) {
                return edition;
            }
            if inherits_workspace_edition(&text) {
                return find_workspace_root_edition(d, project_root)
                    .unwrap_or_else(|| DEFAULT_EDITION.to_string());
            }
        }
        if d == project_root {
            break;
        }
        dir = d.parent();
    }
    DEFAULT_EDITION.to_string()
}

/// Continues walking upward from `start` (already known to have a
/// workspace-member Cargo.toml) to find the `[workspace]` root and read
/// its `[workspace.package]` edition.
fn find_workspace_root_edition(start: &Path, project_root: &Path) -> Option<String> {
    let mut dir = start.parent();
    while let Some(d) = dir {
        let cargo_toml = d.join("Cargo.toml");
        if let Ok(text) = std::fs::read_to_string(&cargo_toml)
            && text.contains("[workspace")
        {
            return workspace_package_edition_field(&text);
        }
        if d == project_root {
            break;
        }
        dir = d.parent();
    }
    None
}

/// Matches a top-level `edition = "…"` line (not indented under a
/// `[workspace...]` table — those are handled separately by
/// `workspace_package_edition_field`). Deliberately requires the line to
/// start at column 0 (after trimming only leading whitespace from the
/// line itself, not requiring zero indentation) — good enough for every
/// real Cargo.toml this needs to handle; a pathological file that quotes
/// `edition = ` inside a string value elsewhere would need a real TOML
/// parser to handle correctly, which this deliberately isn't (see this
/// module's doc comment on why a heuristic miss here is low-stakes).
fn bare_edition_field(text: &str) -> Option<String> {
    for line in text.lines() {
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix("edition") {
            let rest = rest.trim_start();
            if let Some(rest) = rest.strip_prefix('=') {
                let value = rest.trim();
                if let Some(v) = value.strip_prefix('"').and_then(|v| v.strip_suffix('"')) {
                    return Some(v.to_string());
                }
            }
        }
    }
    None
}

fn inherits_workspace_edition(text: &str) -> bool {
    text.lines()
        .any(|l| l.trim().starts_with("edition.workspace") && l.contains("true"))
}

/// Same idea as `bare_edition_field`, scoped to inside a `[workspace.package]`
/// table specifically (a workspace root's OWN `edition = "…"` outside that
/// table wouldn't mean anything to cargo).
fn workspace_package_edition_field(text: &str) -> Option<String> {
    let mut in_workspace_package = false;
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('[') {
            in_workspace_package = trimmed == "[workspace.package]";
            continue;
        }
        if in_workspace_package && let Some(rest) = trimmed.strip_prefix("edition") {
            let rest = rest.trim_start();
            if let Some(rest) = rest.strip_prefix('=') {
                let value = rest.trim();
                if let Some(v) = value.strip_prefix('"').and_then(|v| v.strip_suffix('"')) {
                    return Some(v.to_string());
                }
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_rust_source_reformats_ugly_input() {
        let ugly = "fn   main( ) { let x=1  ;println!(\"{}\",x);}\n";
        let formatted = format_rust_source(ugly, "2021").expect("rustfmt should succeed");
        assert!(formatted.contains("fn main() {"));
        assert_ne!(formatted, ugly);
    }

    #[test]
    fn format_rust_source_is_idempotent() {
        let ugly = "fn   main( ) { let x=1  ;println!(\"{}\",x);}\n";
        let once = format_rust_source(ugly, "2021").unwrap();
        let twice = format_rust_source(&once, "2021").unwrap();
        assert_eq!(
            once, twice,
            "formatting already-formatted code must be a no-op"
        );
    }

    #[test]
    fn format_rust_source_rejects_genuine_syntax_errors() {
        let broken = "fn main( { this is not rust";
        assert!(format_rust_source(broken, "2021").is_err());
    }

    #[test]
    fn format_rust_source_never_touches_files_outside_its_input() {
        // Regression for the exact incident this module exists to prevent:
        // format a snippet that LOOKS like it declares sibling modules —
        // rustfmt must format only the given text, never attempt to
        // resolve or touch any file on disk based on `mod` statements in
        // it, since it was invoked with no file argument at all.
        let dir = tempfile::tempdir().unwrap();
        let canary = dir.path().join("canary.rs");
        std::fs::write(&canary, "fn untouched(){}\n").unwrap();
        let before = std::fs::read_to_string(&canary).unwrap();

        let snippet = "mod canary;\nmod nonexistent_module;\nfn main(){}\n";
        let formatted = format_rust_source(snippet, "2021").unwrap();
        assert!(formatted.contains("mod canary;"));

        let after = std::fs::read_to_string(&canary).unwrap();
        assert_eq!(
            before, after,
            "format_rust_source must never touch any file on disk"
        );
    }

    #[test]
    fn detect_rust_edition_reads_bare_edition_field() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("Cargo.toml"),
            "[package]\nname = \"x\"\nedition = \"2018\"\n",
        )
        .unwrap();
        let src = dir.path().join("src/lib.rs");
        std::fs::create_dir_all(src.parent().unwrap()).unwrap();
        assert_eq!(detect_rust_edition(&src, dir.path()), "2018");
    }

    #[test]
    fn detect_rust_edition_follows_workspace_inheritance() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(
            root.path().join("Cargo.toml"),
            "[workspace]\nmembers = [\"member\"]\n\n[workspace.package]\nedition = \"2024\"\n",
        )
        .unwrap();
        let member_dir = root.path().join("member");
        std::fs::create_dir_all(member_dir.join("src")).unwrap();
        std::fs::write(
            member_dir.join("Cargo.toml"),
            "[package]\nname = \"member\"\nedition.workspace = true\n",
        )
        .unwrap();
        let src = member_dir.join("src/lib.rs");
        assert_eq!(detect_rust_edition(&src, root.path()), "2024");
    }

    #[test]
    fn detect_rust_edition_defaults_when_nothing_found() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("src/lib.rs");
        std::fs::create_dir_all(src.parent().unwrap()).unwrap();
        assert_eq!(detect_rust_edition(&src, dir.path()), DEFAULT_EDITION);
    }

    #[test]
    fn detect_rust_edition_matches_this_workspace_real_config() {
        // Real end-to-end check against THIS repo's actual Cargo.toml
        // files, not synthetic fixtures — calm-server inherits the
        // workspace's 2024 edition via `edition.workspace = true`.
        let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
        let workspace_root = manifest_dir.parent().and_then(Path::parent).unwrap();
        let calm_server_src = workspace_root.join("crates/calm-server/src/tools.rs");
        assert_eq!(
            detect_rust_edition(&calm_server_src, workspace_root),
            "2024"
        );
    }
}
