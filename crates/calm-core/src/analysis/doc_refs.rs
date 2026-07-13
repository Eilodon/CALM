use std::sync::OnceLock;

use regex::Regex;

/// File extensions considered a "real reference" worth checking — kept to
/// source/config/doc extensions actually used in this repo (and most repos)
/// so the regex doesn't fire on version strings (`v2.7.2`), abbreviations
/// (`e.g.`), or plain decimals.
fn path_ref_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(
            r"\.?[A-Za-z0-9_][A-Za-z0-9_/.-]*\.(?:rs|py|ts|tsx|js|jsx|go|rb|java|kt|swift|toml|json|ya?ml|sh|md)\b",
        )
        .unwrap()
    })
}

/// Strips fenced (```) code blocks — illustrative tool-call examples like
/// `file_overview("src/auth/login.ts")` inside a how-to guide aren't claims
/// that a file exists, and are the single biggest source of false-positive
/// references in docs that teach by example.
fn strip_fenced_code_blocks(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut in_fence = false;
    for line in text.lines() {
        if line.trim_start().starts_with("```") {
            in_fence = !in_fence;
        } else if !in_fence {
            out.push_str(line);
        }
        out.push('\n');
    }
    out
}

/// Extracts file-path-like tokens from free-form doc text — e.g. `server.py`,
/// `tools/search.py`, `crates/calm-core/src/fitness.rs`, `.github/workflows/x.yml`
/// — whether or not they're backtick-wrapped, since real-world drift (a
/// stale `CONTRACTS.md` section) shows up as plain prose ("Owner:
/// server.py"), not just inline code spans. Skips matches immediately
/// preceded by `://` so URLs (`example.com/foo.py`) aren't treated as repo
/// file references, and skips fenced code blocks (see
/// `strip_fenced_code_blocks`). Does not dedup — callers that need unique
/// tokens should sort+dedup the result.
pub fn extract_path_refs(text: &str) -> Vec<String> {
    let text = strip_fenced_code_blocks(text);
    let re = path_ref_regex();
    let mut out = Vec::new();
    for m in re.find_iter(&text) {
        let start = m.start();
        // `start - 8` can land inside a multi-byte UTF-8 char (e.g. Vietnamese
        // prose) — walk back to the nearest real char boundary before slicing.
        let mut lookback = start.saturating_sub(8);
        while lookback > 0 && !text.is_char_boundary(lookback) {
            lookback -= 1;
        }
        let preceding = &text[lookback..start];
        if preceding.contains("://") {
            continue;
        }
        // audit H3: a `~/`-prefixed absolute home-dir path (e.g.
        // `~/.codeium/windsurf/mcp_config.json`) is, like a URL, never a
        // repo-relative reference. `path_ref_regex`'s char-class has no `~`
        // (and no `/` as a leading char either), so the match actually
        // starts right after the `~/` — check for that exact two-char
        // suffix, not a bare trailing `~`, or this would miss every real
        // case and only catch the never-occurring `~.foo` (no slash) form.
        if preceding.ends_with("~/") {
            continue;
        }
        out.push(m.as_str().trim_start_matches("./").to_string());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_bare_filename() {
        let refs = extract_path_refs("> **Owner:** server.py");
        assert_eq!(refs, vec!["server.py"]);
    }

    #[test]
    fn extracts_nested_path() {
        let refs = extract_path_refs("Owner: tools/search.py (_resolve_symbol)");
        assert_eq!(refs, vec!["tools/search.py"]);
    }

    #[test]
    fn extracts_deep_relative_path() {
        let refs = extract_path_refs("see `crates/calm-core/src/fitness.rs` for details");
        assert_eq!(refs, vec!["crates/calm-core/src/fitness.rs"]);
    }

    #[test]
    fn ignores_version_strings_and_decimals() {
        let refs = extract_path_refs("v2.7.2 compatible, e.g. 3.14 is not a path");
        assert!(refs.is_empty(), "got {refs:?}");
    }

    #[test]
    fn skips_urls() {
        let refs = extract_path_refs("see https://example.com/foo.py for reference");
        assert!(refs.is_empty(), "got {refs:?}");
    }
    /// audit H3: a `~/`-prefixed home-dir path must be skipped the same way
    /// a URL is — it's never a repo-relative reference. Regression test for
    /// the false positive `check_config_drift` reported against README.md's
    /// real, correct Windsurf/Antigravity setup instructions.
    #[test]
    fn skips_home_dir_paths() {
        let refs = extract_path_refs("edit `~/.codeium/windsurf/mcp_config.json`");
        assert!(refs.is_empty(), "got {refs:?}");
    }

    #[test]
    fn extracts_multiple_and_preserves_order() {
        let refs =
            extract_path_refs("db/schema.py (CREATE), indexer/indexer.py (WRITE), tools/* (READ)");
        assert_eq!(refs, vec!["db/schema.py", "indexer/indexer.py"]);
    }

    #[test]
    fn strips_leading_dot_slash() {
        let refs = extract_path_refs("run `./scripts/build.sh` first");
        assert_eq!(refs, vec!["scripts/build.sh"]);
    }

    /// Regression: a multi-byte UTF-8 char (e.g. Vietnamese "Nguyên") landing
    /// within 8 bytes before a match used to panic by slicing mid-character —
    /// `start.saturating_sub(8)` must walk back to a real char boundary.
    #[test]
    fn does_not_panic_on_multibyte_utf8_before_match() {
        let refs = extract_path_refs("Nguyên tắc: xem server.py để biết chi tiết");
        assert_eq!(refs, vec!["server.py"]);
    }

    #[test]
    fn captures_leading_dot_for_dotfiles_and_dotdirs() {
        let refs = extract_path_refs(
            "Claude Code (`.mcp.json`), Cursor (`.cursor/mcp.json`), see `.github/workflows/release.yml`",
        );
        assert_eq!(
            refs,
            vec![
                ".mcp.json",
                ".cursor/mcp.json",
                ".github/workflows/release.yml"
            ]
        );
    }

    #[test]
    fn skips_references_inside_fenced_code_blocks() {
        let refs = extract_path_refs(
            "prose mentions server.py\n\
             ```\n\
             file_overview(\"src/auth/login.ts\")\n\
             ```\n\
             more prose mentions client.py",
        );
        assert_eq!(refs, vec!["server.py", "client.py"]);
    }
}
