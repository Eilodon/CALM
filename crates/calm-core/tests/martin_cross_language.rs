//! Cross-language regression fixture for Martin/OOD metrics (2026-07-27
//! plan, T1.5). Indexes the REAL `multi_lang_workspace` fixture through the
//! actual parser/resolver pipeline -- not a hand-inserted synthetic DB --
//! because the failure mode this guards against is a silent cross-language
//! import-resolution regression, and only real parsing exercises the code
//! path that could regress. The four import-resolution defects fixed in
//! commit d1fd271 (86.8% -> 99.6% first-party resolution) had accumulated
//! unseen precisely because nothing measured `import_edges.to_path` before
//! this plan; this fixture is that measurement made permanent.
//!
//! Values below were captured directly from a real indexing run of this
//! fixture (not hand-computed/guessed) -- if a future resolver change moves
//! any of them, that's either an intentional resolution improvement (update
//! this test deliberately) or an accidental regression (this test is what
//! catches it before it ships unnoticed).

use rusqlite::Connection;
use std::path::PathBuf;

fn fixture_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/multi_lang_workspace")
}

fn index_fixture() -> Connection {
    let mut conn = Connection::open_in_memory().unwrap();
    calm_core::db::schema::init_db(&conn).unwrap();
    let phase = std::sync::Arc::new(std::sync::RwLock::new(
        calm_core::types::IndexingPhase::Scanning,
    ));
    calm_core::indexer::pipeline::run_indexing_pipeline(&mut conn, &fixture_root(), phase).unwrap();
    conn
}

fn approx(a: f64, b: f64) {
    assert!((a - b).abs() < 1e-9, "expected ~{b}, got {a}");
}

#[test]
fn martin_metrics_cross_language_shape_is_stable() {
    let conn = index_fixture();
    let summary = calm_core::analysis::martin::compute_martin_metrics(&conn).unwrap();

    // Population scoping (T1.2): 23 files indexed across 11 languages, but
    // only 16 clear the Ca+Ce>0 gate. The 7 excluded are a real, meaningful
    // signal, not noise -- java/kotlin/ruby files with no cross-file import
    // in this fixture at all, plus go/main.go whose only import (`fmt`) is
    // external (to_path IS NULL, excluded by the same query that excludes
    // unresolved imports repo-wide). If any of these 7 start resolving a
    // real edge, files_measured moves and this assertion forces a conscious
    // review of why.
    assert_eq!(summary.files_total, 23);
    assert_eq!(summary.files_measured, 16);

    approx(summary.avg_instability, 0.5625);
    assert!(summary.avg_distance.is_some());
    approx(summary.avg_distance.unwrap(), 2.0 / 3.0);

    let find = |path: &str| {
        summary
            .files
            .iter()
            .find(|f| f.path == path)
            .unwrap_or_else(|| panic!("{path} missing from measured population"))
    };

    // Pure efferent / pure afferent shape (Python, unsupported-for-A
    // language): main.py imports pkg.helper and nothing imports main.py.
    let main_py = find("python/main.py");
    assert_eq!((main_py.ca, main_py.ce), (0, 1));
    approx(main_py.instability, 1.0);
    assert_eq!(
        main_py.abstractness, None,
        "python is not in ABSTRACTNESS_SUPPORTED_LANGUAGES"
    );

    let helper_py = find("python/pkg/helper.py");
    assert_eq!((helper_py.ca, helper_py.ce), (1, 0));
    approx(helper_py.instability, 0.0);

    // C header/impl split: both main.c and helper.c include helper.h ->
    // helper.h has Ca=2 (two distinct importers, via DISTINCT).
    let helper_h = find("c/helper.h");
    assert_eq!((helper_h.ca, helper_h.ce), (2, 0));

    // C# (an ABSTRACTNESS_SUPPORTED_LANGUAGES member): Helper.cs is a single
    // concrete `class Helper` with no importers of its own kind mixed in --
    // worst-case distance (D=1.0, all-concrete + all-afferent).
    let cs_helper = find("csharp/Helper.cs");
    assert_eq!((cs_helper.ca, cs_helper.ce), (1, 0));
    assert_eq!(
        cs_helper.abstractness,
        Some(0.0),
        "single concrete class -> A=0.0"
    );
    approx(cs_helper.distance.unwrap(), 1.0);

    // Program.cs imports Helper.cs and is itself a single concrete class:
    // I=1.0, A=0.0 -> sits exactly on the main sequence (D=0.0).
    let cs_program = find("csharp/Program.cs");
    assert_eq!((cs_program.ca, cs_program.ce), (0, 1));
    assert_eq!(cs_program.abstractness, Some(0.0));
    approx(cs_program.distance.unwrap(), 0.0);

    // PHP: index.php has two imports in the fixture (a resolved
    // `./src/Helper.php` and an unresolved `App\Helper`), but only the
    // resolved one has a non-NULL to_path -- Ce must count 1, not 2.
    let php_index = find("php/index.php");
    assert_eq!((php_index.ca, php_index.ce), (0, 1));

    // Files with zero import edges at all (not merely Ca=Ce=0 from a
    // filtered query, but genuinely absent from import_edges) must be
    // absent from the measured population entirely, not present with
    // instability defaulted to some placeholder.
    for isolated in [
        "java/src/main/java/com/example/Main.java",
        "kotlin/src/main/kotlin/com/example/Greeter.kt",
        "ruby/handlers.rb",
        "sql/schema.sql",
        "go/main.go", // only import is external (`fmt`) -> to_path NULL
    ] {
        assert!(
            !summary.files.iter().any(|f| f.path == isolated),
            "{isolated} has no resolved import edge and must not enter the measured population"
        );
    }
}
