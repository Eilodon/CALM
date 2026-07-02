use super::coverage::CoverageData;

/// Best-effort "is this symbol private/internal" signal from name + signature
/// conventions, per tier-0 language. Used only as a `dead_code_confidence`
/// input — not stored, computed live from columns already in the index.
pub fn is_private_symbol(language: &str, name: &str, signature: &str) -> bool {
    match language {
        "python" => name.starts_with('_'),
        "rust" => !signature.contains("pub "),
        "go" => name
            .chars()
            .next()
            .map(|c| c.is_lowercase())
            .unwrap_or(false),
        "java" => signature.contains("private "),
        "javascript" | "typescript" => !signature.contains("export"),
        _ => false,
    }
}

/// Whether `language` is a tier-0 language with full symbol extraction
/// (vs. the generic textual-only fallback), per `get_lang_constants`.
pub fn scope_clear_for_language(language: &str) -> bool {
    crate::indexer::lang_constants::get_lang_constants(language).is_some()
}

#[allow(clippy::too_many_arguments)]
pub fn compute_dead_code_confidence(
    symbol_path: &str,
    line_start: i64,
    line_end: i64,
    caller_count: i64,
    is_entry_point: bool,
    is_test: bool,
    is_private: bool,
    scope_clear: bool,
    coverage: &CoverageData,
    kind: &str,
) -> (&'static str, &'static str) {
    // Type-level definitions (struct/class/...) aren't "called" the way
    // functions and methods are — they're referenced via construction
    // syntax (`Foo { .. }` in Rust) that the call-graph extractor doesn't
    // track as a call at all, so `caller_count` is 0 for essentially every
    // one of them regardless of real usage (confirmed: 100% of this repo's
    // own `struct` symbols have caller_count=0). "Dead code" isn't a
    // well-formed question for a kind that structurally can't accrue
    // callers — answering "high confidence dead" here was the single
    // largest source of `dead_code_pct` false positives.
    if !matches!(kind, "function" | "method") {
        let source = if coverage.source != "none" {
            "static+coverage"
        } else {
            "static"
        };
        return ("none", source);
    }

    if is_entry_point || is_test || caller_count > 0 {
        let source = if coverage.source != "none" {
            "static+coverage"
        } else {
            "static"
        };
        return ("none", source);
    }

    let runtime_covered =
        coverage.source != "none" && coverage.is_covered(symbol_path, line_start, line_end);

    if runtime_covered {
        return ("low", "static+coverage");
    }

    let source = if coverage.source != "none" {
        "static+coverage"
    } else {
        "static"
    };

    if is_private {
        return ("high", source);
    }
    if scope_clear {
        return ("medium", source);
    }
    ("low", source)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::{HashMap, HashSet};

    fn no_coverage() -> CoverageData {
        CoverageData::none()
    }

    fn with_coverage(path: &str, lines: &[i64]) -> CoverageData {
        let mut covered_lines = HashMap::new();
        covered_lines.insert(
            path.to_string(),
            lines.iter().copied().collect::<HashSet<_>>(),
        );
        CoverageData {
            source: "lcov".to_string(),
            covered_lines,
        }
    }

    #[test]
    fn test_entry_point_always_none() {
        let (conf, src) = compute_dead_code_confidence(
            "/f.py",
            1,
            10,
            0,
            true,
            false,
            false,
            false,
            &no_coverage(),
            "function",
        );
        assert_eq!(conf, "none");
        assert_eq!(src, "static");
    }

    #[test]
    fn test_has_callers_always_none() {
        let (conf, src) = compute_dead_code_confidence(
            "/f.py",
            1,
            10,
            3,
            false,
            false,
            false,
            false,
            &no_coverage(),
            "function",
        );
        assert_eq!(conf, "none");
        assert_eq!(src, "static");
    }

    /// DEBT-008 regression: a `#[test]`/`test_*` symbol has no in-repo callers
    /// by design (invoked by the test harness) — it must never be flagged as
    /// dead code just because `is_private` + zero callers happen to hold too.
    #[test]
    fn test_is_test_always_none_even_when_private_and_zero_callers() {
        let (conf, src) = compute_dead_code_confidence(
            "/f.py",
            1,
            10,
            0,
            false,
            true,
            true,
            true,
            &no_coverage(),
            "function",
        );
        assert_eq!(conf, "none");
        assert_eq!(src, "static");
    }

    #[test]
    fn test_runtime_covered_returns_low() {
        let cov = with_coverage("/f.py", &[5]);
        let (conf, src) = compute_dead_code_confidence(
            "/f.py", 1, 10, 0, false, false, false, false, &cov, "function",
        );
        assert_eq!(conf, "low");
        assert_eq!(src, "static+coverage");
    }

    #[test]
    fn test_private_no_callers_returns_high() {
        let (conf, _) = compute_dead_code_confidence(
            "/f.py",
            1,
            10,
            0,
            false,
            false,
            true,
            false,
            &no_coverage(),
            "function",
        );
        assert_eq!(conf, "high");
    }

    #[test]
    fn test_scope_clear_returns_medium() {
        let (conf, _) = compute_dead_code_confidence(
            "/f.py",
            1,
            10,
            0,
            false,
            false,
            false,
            true,
            &no_coverage(),
            "function",
        );
        assert_eq!(conf, "medium");
    }

    #[test]
    fn test_unclear_scope_returns_low() {
        let (conf, _) = compute_dead_code_confidence(
            "/f.py",
            1,
            10,
            0,
            false,
            false,
            false,
            false,
            &no_coverage(),
            "function",
        );
        assert_eq!(conf, "low");
    }

    #[test]
    fn test_source_reflects_coverage_availability() {
        let (_, src) = compute_dead_code_confidence(
            "/f.py",
            1,
            10,
            5,
            false,
            false,
            false,
            false,
            &no_coverage(),
            "function",
        );
        assert_eq!(src, "static");

        let cov = with_coverage("/other.py", &[1]);
        let (_, src) = compute_dead_code_confidence(
            "/f.py", 1, 10, 5, false, false, false, false, &cov, "function",
        );
        assert_eq!(src, "static+coverage");
    }

    /// Regression: a `struct` (or any non-callable kind) must never be
    /// scored "high confidence dead" just because it has zero callers — it
    /// *always* has zero callers in this codebase's call-graph model (type
    /// construction isn't tracked as a call), so the old behavior flagged
    /// essentially every private struct in the project.
    #[test]
    fn test_non_callable_kind_is_never_flagged_dead_even_when_private() {
        let (conf, src) = compute_dead_code_confidence(
            "/f.rs",
            1,
            10,
            0,
            false,
            false,
            true, // is_private — would return "high" for a function/method
            true,
            &no_coverage(),
            "struct",
        );
        assert_eq!(conf, "none");
        assert_eq!(src, "static");
    }

    #[test]
    fn test_method_kind_is_still_evaluated() {
        let (conf, _) = compute_dead_code_confidence(
            "/f.rs",
            1,
            10,
            0,
            false,
            false,
            true,
            true,
            &no_coverage(),
            "method",
        );
        assert_eq!(
            conf, "high",
            "method is a callable kind — still subject to normal dead-code rules"
        );
    }
}
