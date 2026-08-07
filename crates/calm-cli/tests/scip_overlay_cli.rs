//! P0.1 integration test: the one-shot `calm index` CLI command wires in the
//! SCIP overlay the same way calm-server's background indexer
//! (`serve_stdio_with_preset`, crates/calm-server/src/lib.rs) already does.
//!
//! Ignored by default — requires rust-analyzer on PATH/rustup/VS Code and a
//! real `cargo metadata` resolve, neither of which CI is guaranteed to have
//! for this opt-in feature. Mirrors
//! `crates/calm-core/src/scip/mod.rs::overlay_upgrades_a_real_edge_on_the_fixture`,
//! but drives the real `calm` binary as a subprocess instead of calling
//! `run_overlay` directly, so it also exercises the CLI wiring itself.

use std::path::Path;
use std::process::Command;

/// Recursively copies `src` into `dst`, skipping `target/` (build artifacts —
/// irrelevant across machines and just slow down the copy; `cargo metadata`
/// regenerates whatever it needs).
fn copy_fixture(src: &Path, dst: &Path) {
    std::fs::create_dir_all(dst).unwrap();
    for entry in std::fs::read_dir(src).unwrap() {
        let entry = entry.unwrap();
        let file_type = entry.file_type().unwrap();
        let name = entry.file_name();
        if name == "target" {
            continue;
        }
        let src_path = entry.path();
        let dst_path = dst.join(&name);
        if file_type.is_dir() {
            copy_fixture(&src_path, &dst_path);
        } else {
            std::fs::copy(&src_path, &dst_path).unwrap();
        }
    }
}

#[test]
#[ignore]
fn calm_index_cli_upgrades_a_real_edge_on_the_fixture() {
    let fixture = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .join("calm-core/tests/fixtures/rust_workspace");
    let tmp = tempfile::tempdir().unwrap();
    copy_fixture(&fixture, tmp.path());

    let output = Command::new(env!("CARGO_BIN_EXE_calm"))
        .args(["index", "--project-root"])
        .arg(tmp.path())
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "calm index failed: stdout={}\nstderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let db_path = tmp.path().join(".calm").join("index.db");
    let conn = rusqlite::Connection::open(&db_path).unwrap();
    let conf: String = conn
        .query_row(
            "SELECT edge_confidence FROM call_edges \
             WHERE from_symbol = 'app/src/main.rs::main' \
               AND to_symbol = 'core/src/engine.rs::Engine::start'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(
        conf,
        "formal",
        "expected the CLI's one-shot `calm index` to run the SCIP overlay \
         and upgrade this edge to formal, same as the background indexer does \
         -- child stdout={}\nchild stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}
