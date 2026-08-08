//! `ChangeKind` classification -- CCK-08
//! (docs/plans/2026-08-08-master-change-control-execution-blueprint.md).
//!
//! `classify_observed_change` is a **heuristic, line-based** classifier,
//! not a real per-language AST diff -- appropriate for this PR's declared
//! **shadow-only** status (it computes a decision and nothing yet reads it
//! to block a write; CCK-10 is the PR that would need to justify a bigger
//! investment here). It is deliberately conservative in one direction
//! only: every category narrower than [`ChangeKind::Body`] requires the
//! narrower condition to hold exactly, so anything the heuristic can't
//! positively identify falls through to `Body` (the same "unknown ⇒
//! highest realistic caution" posture `IndexInputDrift::Unknown` and
//! `EvidenceSnapshot`'s `Degraded` class already take elsewhere in this
//! train) rather than ever under-classifying a real code change as
//! something narrower and lower-stakes than it is.

use std::path::Path;

use crate::analysis::diff_impact::is_signature_semantically_changed;

/// The shared category set both "what was declared" and "what the diff
/// actually shows" are drawn from. See the module doc comment for why
/// [`ChangeIntentKind`] and [`ObservedChangeKind`] wrap this instead of
/// each declaring their own copy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChangeKind {
    /// A symbol that didn't exist in the old text.
    Add,
    /// A symbol that doesn't exist in the new text.
    Delete,
    /// Differs only in blank lines / indentation / inter-token spacing --
    /// no non-blank line's trimmed, whitespace-collapsed content changed.
    Whitespace,
    /// Only regular (non-doc) comment lines differ; every code line and
    /// every doc-comment line is unchanged.
    Comment,
    /// Only doc-comment lines (`///`, `//!`, `/** */`, `##`, Python
    /// docstrings) differ; every code line and every regular-comment line
    /// is unchanged. Kept distinct from `Comment` specifically so a
    /// declared "just updating docs" intent can be checked against real
    /// code drift (the blueprint's "declared-doc-vs-observed-code"
    /// mismatch fixture).
    DocOnly,
    /// A leading visibility keyword (`pub`, `pub(crate)`, `public`,
    /// `private`, `protected`, `internal`, ...) changed and nothing else
    /// on the touched code lines did.
    Visibility,
    /// A function/method's own signature changed meaning, per
    /// [`is_signature_semantically_changed`] -- only checked when the
    /// caller supplies both signature texts; see [`ObservedChangeInput`].
    Signature,
    /// Path matches a known dependency-manifest filename.
    Manifest,
    /// `ObservedChangeInput::is_test` was `true` -- caller-supplied, not
    /// re-derived from text (CALM already tracks `is_test` per symbol at
    /// index time; re-guessing it from content here would just be a
    /// second, potentially-disagreeing signal).
    TestOnly,
    /// Real code changed and none of the narrower categories applied --
    /// the conservative fallback (see module doc comment).
    Body,
}

impl ChangeKind {
    /// Stable lowercase name -- the exact string persisted in
    /// `change_intents.kind` (CCK-07) and safe to round-trip through
    /// [`ChangeKind::parse`].
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Add => "add",
            Self::Delete => "delete",
            Self::Whitespace => "whitespace",
            Self::Comment => "comment",
            Self::DocOnly => "doc_only",
            Self::Visibility => "visibility",
            Self::Signature => "signature",
            Self::Manifest => "manifest",
            Self::TestOnly => "test_only",
            Self::Body => "body",
        }
    }

    /// Inverse of [`as_str`](Self::as_str); `None` for anything else --
    /// same "loud unknown, never a silent guess" posture as
    /// `policy::model::RiskLevel::parse`.
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "add" => Some(Self::Add),
            "delete" => Some(Self::Delete),
            "whitespace" => Some(Self::Whitespace),
            "comment" => Some(Self::Comment),
            "doc_only" => Some(Self::DocOnly),
            "visibility" => Some(Self::Visibility),
            "signature" => Some(Self::Signature),
            "manifest" => Some(Self::Manifest),
            "test_only" => Some(Self::TestOnly),
            "body" => Some(Self::Body),
            _ => None,
        }
    }
}

/// What a caller *declared* they were about to do. Distinct type from
/// [`ObservedChangeKind`] so the two can never be compared to themselves by
/// accident -- only [`kinds_mismatch`] compares them, deliberately.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct ChangeIntentKind(pub ChangeKind);

/// What [`classify_observed_change`] actually found in the diff.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct ObservedChangeKind(pub ChangeKind);

/// Invariant #2 (docs/plans/2026-08-08-master-change-control-execution-blueprint.md
/// §2): a declared/observed disagreement must never be silently accepted.
/// This is a pure equality check on purpose -- any smoothing ("doc and
/// comment are close enough") belongs in the *policy* that decides how to
/// react to a mismatch (`policy::evaluate`), not in whether one is
/// detected at all.
pub fn kinds_mismatch(declared: ChangeIntentKind, observed: ObservedChangeKind) -> bool {
    declared.0 != observed.0
}

/// Everything [`classify_observed_change`] needs, bundled so the function
/// itself stays a single `&ObservedChangeInput` parameter as this grows.
pub struct ObservedChangeInput<'a> {
    /// Repo-relative path, forward slashes -- used only for the manifest
    /// check.
    pub path: &'a str,
    /// `symbols.language` column value, e.g. `"rust"`, `"python"`.
    pub language: &'a str,
    /// Caller-supplied, not re-derived -- see [`ChangeKind::TestOnly`].
    pub is_test: bool,
    pub old_text: Option<&'a str>,
    pub new_text: Option<&'a str>,
    /// Present only when the caller has already resolved the touched
    /// symbol's signature range on both sides (mirrors
    /// `compute_touch_risk`'s own signature-escalation precondition in
    /// `calm-server`) -- `None` skips the `Signature` check entirely
    /// rather than guessing at signature boundaries from raw text.
    pub old_signature: Option<&'a str>,
    pub new_signature: Option<&'a str>,
}

const MANIFEST_BASENAMES: &[&str] = &[
    "Cargo.toml",
    "Cargo.lock",
    "package.json",
    "package-lock.json",
    "yarn.lock",
    "pnpm-lock.yaml",
    "go.mod",
    "go.sum",
    "pom.xml",
    "build.gradle",
    "build.gradle.kts",
    "requirements.txt",
    "pyproject.toml",
    "Pipfile",
    "Pipfile.lock",
    "Gemfile",
    "Gemfile.lock",
    "composer.json",
    "composer.lock",
];

/// Broader and non-root-relative on purpose, unlike
/// `indexer::pipeline`'s own private `is_manifest_path` (which is
/// deliberately narrow -- root-only, 3 names -- to match one specific
/// resolution-cache invalidation check). This one answers a different
/// question ("is this file a dependency manifest at all, anywhere in the
/// tree") for change classification, not cache invalidation, so a
/// workspace member's `Cargo.toml` counts here even though it wouldn't for
/// that cache.
fn is_manifest_path(path: &str) -> bool {
    let basename = Path::new(path).file_name().and_then(|n| n.to_str()).unwrap_or(path);
    MANIFEST_BASENAMES.contains(&basename) || basename.ends_with(".csproj") || basename.ends_with(".fsproj")
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LineKind {
    Blank,
    DocComment,
    RegularComment,
    Code,
}

fn classify_line(line: &str, language: &str) -> LineKind {
    let t = line.trim();
    if t.is_empty() {
        return LineKind::Blank;
    }
    if t.starts_with("///") || t.starts_with("//!") || t.starts_with("/**") || t.starts_with("##") {
        return LineKind::DocComment;
    }
    if matches!(language, "python") && (t.starts_with("\"\"\"") || t.starts_with("'''")) {
        return LineKind::DocComment;
    }
    if t.starts_with("//") || t.starts_with('#') || t.starts_with("/*") || t.starts_with('*') {
        return LineKind::RegularComment;
    }
    LineKind::Code
}

/// Lines of `text` whose [`classify_line`] result is in `keep`, trimmed and
/// whitespace-collapsed, joined with `\n` -- the shared normalization used
/// by every equality check below so "differs only in formatting" never
/// masquerades as "differs in content" or vice versa.
fn normalized_lines(text: &str, language: &str, keep: &[LineKind]) -> String {
    text.lines()
        .filter(|line| keep.contains(&classify_line(line, language)))
        .map(|line| line.split_whitespace().collect::<Vec<_>>().join(" "))
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .join("\n")
}

const VISIBILITY_KEYWORDS: &[&str] = &[
    "pub(crate)",
    "pub(super)",
    "pub",
    "public",
    "private",
    "protected",
    "internal",
];

fn strip_leading_visibility_keyword(line: &str) -> &str {
    let t = line.trim_start();
    for kw in VISIBILITY_KEYWORDS {
        if let Some(rest) = t.strip_prefix(kw) {
            if rest.starts_with(char::is_whitespace) || rest.is_empty() {
                return rest.trim_start();
            }
        }
    }
    t
}

fn code_ignoring_leading_visibility(text: &str, language: &str) -> String {
    text.lines()
        .filter(|line| classify_line(line, language) == LineKind::Code)
        .map(|line| {
            strip_leading_visibility_keyword(line).split_whitespace().collect::<Vec<_>>().join(" ")
        })
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .join("\n")
}

use LineKind::{Blank, Code, DocComment, RegularComment};

/// Classifies `input`'s diff into one [`ObservedChangeKind`]. See the
/// module doc comment for the fallback posture and each [`ChangeKind`]
/// variant's doc comment for exactly what it requires.
pub fn classify_observed_change(input: &ObservedChangeInput) -> ObservedChangeKind {
    let kind = classify_inner(input);
    ObservedChangeKind(kind)
}

fn classify_inner(input: &ObservedChangeInput) -> ChangeKind {
    match (input.old_text, input.new_text) {
        (None, Some(_)) => return ChangeKind::Add,
        (Some(_), None) => return ChangeKind::Delete,
        (None, None) => return ChangeKind::Body, // nothing to classify; conservative fallback
        (Some(_), Some(_)) => {}
    }
    if is_manifest_path(input.path) {
        return ChangeKind::Manifest;
    }
    if input.is_test {
        return ChangeKind::TestOnly;
    }

    let old = input.old_text.unwrap();
    let new = input.new_text.unwrap();
    let lang = input.language;

    if old == new {
        return ChangeKind::Whitespace;
    }

    let all_kinds = &[Blank, DocComment, RegularComment, Code][..];
    if normalized_lines(old, lang, all_kinds) == normalized_lines(new, lang, all_kinds) {
        return ChangeKind::Whitespace;
    }

    let code_and_regular = &[RegularComment, Code][..];
    if normalized_lines(old, lang, code_and_regular) == normalized_lines(new, lang, code_and_regular) {
        return ChangeKind::DocOnly;
    }

    let code_only = &[Code][..];
    if normalized_lines(old, lang, code_only) == normalized_lines(new, lang, code_only) {
        return ChangeKind::Comment;
    }

    if let (Some(old_sig), Some(new_sig)) = (input.old_signature, input.new_signature) {
        if is_signature_semantically_changed(old_sig, new_sig, lang) {
            return ChangeKind::Signature;
        }
    }

    if code_ignoring_leading_visibility(old, lang) == code_ignoring_leading_visibility(new, lang) {
        return ChangeKind::Visibility;
    }

    ChangeKind::Body
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input<'a>(
        path: &'a str,
        language: &'a str,
        old_text: Option<&'a str>,
        new_text: Option<&'a str>,
    ) -> ObservedChangeInput<'a> {
        ObservedChangeInput {
            path,
            language,
            is_test: false,
            old_text,
            new_text,
            old_signature: None,
            new_signature: None,
        }
    }

    fn classify(path: &str, language: &str, old: Option<&str>, new: Option<&str>) -> ChangeKind {
        classify_observed_change(&input(path, language, old, new)).0
    }

    #[test]
    fn add_when_old_text_absent() {
        assert_eq!(classify("a.rs", "rust", None, Some("fn f() {}")), ChangeKind::Add);
    }

    #[test]
    fn delete_when_new_text_absent() {
        assert_eq!(classify("a.rs", "rust", Some("fn f() {}"), None), ChangeKind::Delete);
    }

    #[test]
    fn whitespace_only_reformatting() {
        let old = "fn f() {\n    let x = 1;\n}";
        let new = "fn f() {\n  let x =    1;\n}";
        assert_eq!(classify("a.rs", "rust", Some(old), Some(new)), ChangeKind::Whitespace);
    }

    #[test]
    fn regular_comment_only_change() {
        let old = "fn f() {\n    // old note\n    let x = 1;\n}";
        let new = "fn f() {\n    // new note, expanded\n    let x = 1;\n}";
        assert_eq!(classify("a.rs", "rust", Some(old), Some(new)), ChangeKind::Comment);
    }

    #[test]
    fn doc_comment_only_change() {
        let old = "/// old docs\nfn f() {\n    // keep\n    let x = 1;\n}";
        let new = "/// new, better docs\nfn f() {\n    // keep\n    let x = 1;\n}";
        assert_eq!(classify("a.rs", "rust", Some(old), Some(new)), ChangeKind::DocOnly);
    }

    #[test]
    fn body_change_when_code_line_differs() {
        let old = "fn f() {\n    let x = 1;\n}";
        let new = "fn f() {\n    let x = 2;\n}";
        assert_eq!(classify("a.rs", "rust", Some(old), Some(new)), ChangeKind::Body);
    }

    #[test]
    fn visibility_change_alone() {
        let old = "fn helper() {}";
        let new = "pub fn helper() {}";
        assert_eq!(classify("a.rs", "rust", Some(old), Some(new)), ChangeKind::Visibility);
    }

    #[test]
    fn signature_change_detected_when_signatures_supplied() {
        let old_body = "fn f(x: i32) -> i32 {\n    x\n}";
        let new_body = "fn f(x: i32, y: i32) -> i32 {\n    x\n}";
        let observed = classify_observed_change(&ObservedChangeInput {
            path: "a.rs",
            language: "rust",
            is_test: false,
            old_text: Some(old_body),
            new_text: Some(new_body),
            old_signature: Some("fn f(x: i32) -> i32"),
            new_signature: Some("fn f(x: i32, y: i32) -> i32"),
        });
        assert_eq!(observed.0, ChangeKind::Signature);
    }

    #[test]
    fn manifest_path_wins_over_content_analysis() {
        assert_eq!(
            classify("Cargo.toml", "toml", Some("a = 1"), Some("a = 2")),
            ChangeKind::Manifest
        );
        assert_eq!(
            classify("crates/foo/Cargo.toml", "toml", Some("a = 1"), Some("a = 2")),
            ChangeKind::Manifest
        );
    }

    #[test]
    fn test_only_when_flagged_regardless_of_content() {
        let mut i = input("tests/foo_test.rs", "rust", Some("assert!(true);"), Some("assert!(false);"));
        i.is_test = true;
        assert_eq!(classify_observed_change(&i).0, ChangeKind::TestOnly);
    }

    #[test]
    fn declared_doc_but_observed_body_is_a_mismatch() {
        let declared = ChangeIntentKind(ChangeKind::DocOnly);
        let old = "fn f() {\n    let x = 1;\n}";
        let new = "fn f() {\n    let x = 2;\n}";
        let observed = classify_observed_change(&input("a.rs", "rust", Some(old), Some(new)));
        assert_eq!(observed.0, ChangeKind::Body);
        assert!(kinds_mismatch(declared, observed), "DocOnly declared but real code changed must mismatch");
    }

    #[test]
    fn matching_declared_and_observed_kind_is_not_a_mismatch() {
        let declared = ChangeIntentKind(ChangeKind::Body);
        let observed = ObservedChangeKind(ChangeKind::Body);
        assert!(!kinds_mismatch(declared, observed));
    }

    #[test]
    fn classification_is_deterministic() {
        let old = "fn f() {\n    let x = 1;\n}";
        let new = "pub fn f() {\n    let x = 2;\n}";
        let a = classify("a.rs", "rust", Some(old), Some(new));
        let b = classify("a.rs", "rust", Some(old), Some(new));
        assert_eq!(a, b);
    }

    #[test]
    fn every_change_kind_round_trips_through_as_str_and_parse() {
        let all = [
            ChangeKind::Add,
            ChangeKind::Delete,
            ChangeKind::Whitespace,
            ChangeKind::Comment,
            ChangeKind::DocOnly,
            ChangeKind::Visibility,
            ChangeKind::Signature,
            ChangeKind::Manifest,
            ChangeKind::TestOnly,
            ChangeKind::Body,
        ];
        for kind in all {
            assert_eq!(ChangeKind::parse(kind.as_str()), Some(kind));
        }
    }

    #[test]
    fn change_kind_parse_rejects_unknown_strings() {
        assert_eq!(ChangeKind::parse("not_a_real_kind"), None);
        assert_eq!(ChangeKind::parse(""), None);
    }
}
