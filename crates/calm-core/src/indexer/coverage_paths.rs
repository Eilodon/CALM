//! Coverage-report search paths — lives in `indexer` (not `analysis`) even
//! though `analysis::coverage::load_coverage` is the only real consumer of
//! the parsing logic, because `indexer::refresh` also needs the plain path
//! list to classify coverage files during a watch-triggered refresh
//! (`RefreshClassifier::coverage_paths`). `indexer` must stay upstream of
//! `analysis` (fitness-check's `[[boundaries]]` rule in `thresholds.toml`
//! enforces this), so the list is defined here and `analysis::coverage`
//! re-exports it — see the 2026-07-28 `hotspot_risk`/`common.rs` split for
//! the same pattern applied to a different boundary violation.

pub const COVERAGE_SEARCH_PATHS: &[(&str, &str)] = &[
    ("lcov.info", "lcov"),
    ("coverage/lcov.info", "lcov"),
    (".nyc_output/lcov.info", "lcov"),
    (".coverage", "python"),
    ("coverage.out", "go"),
    ("coverage/coverage.out", "go"),
    ("coverage.xml", "cobertura"),
    ("coverage/coverage.xml", "cobertura"),
];
