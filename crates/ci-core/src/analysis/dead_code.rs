use super::coverage::CoverageData;

#[allow(clippy::too_many_arguments)]
pub fn compute_dead_code_confidence(
    symbol_path: &str,
    line_start: i64,
    line_end: i64,
    caller_count: i64,
    is_entry_point: bool,
    is_private: bool,
    scope_clear: bool,
    coverage: &CoverageData,
) -> (&'static str, &'static str) {
    if is_entry_point || caller_count > 0 {
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
        let (conf, src) =
            compute_dead_code_confidence("/f.py", 1, 10, 0, true, false, false, &no_coverage());
        assert_eq!(conf, "none");
        assert_eq!(src, "static");
    }

    #[test]
    fn test_has_callers_always_none() {
        let (conf, src) =
            compute_dead_code_confidence("/f.py", 1, 10, 3, false, false, false, &no_coverage());
        assert_eq!(conf, "none");
        assert_eq!(src, "static");
    }

    #[test]
    fn test_runtime_covered_returns_low() {
        let cov = with_coverage("/f.py", &[5]);
        let (conf, src) =
            compute_dead_code_confidence("/f.py", 1, 10, 0, false, false, false, &cov);
        assert_eq!(conf, "low");
        assert_eq!(src, "static+coverage");
    }

    #[test]
    fn test_private_no_callers_returns_high() {
        let (conf, _) =
            compute_dead_code_confidence("/f.py", 1, 10, 0, false, true, false, &no_coverage());
        assert_eq!(conf, "high");
    }

    #[test]
    fn test_scope_clear_returns_medium() {
        let (conf, _) =
            compute_dead_code_confidence("/f.py", 1, 10, 0, false, false, true, &no_coverage());
        assert_eq!(conf, "medium");
    }

    #[test]
    fn test_unclear_scope_returns_low() {
        let (conf, _) =
            compute_dead_code_confidence("/f.py", 1, 10, 0, false, false, false, &no_coverage());
        assert_eq!(conf, "low");
    }

    #[test]
    fn test_source_reflects_coverage_availability() {
        let (_, src) =
            compute_dead_code_confidence("/f.py", 1, 10, 5, false, false, false, &no_coverage());
        assert_eq!(src, "static");

        let cov = with_coverage("/other.py", &[1]);
        let (_, src) = compute_dead_code_confidence("/f.py", 1, 10, 5, false, false, false, &cov);
        assert_eq!(src, "static+coverage");
    }
}
