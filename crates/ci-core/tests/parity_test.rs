use ci_core::analysis::codeowners::load_codeowners;
use ci_core::analysis::coverage::load_coverage;
use serde_json::Value;

use std::fs;
use std::path::{Path, PathBuf};

/// Build the synthetic fixture DB in-memory.
///
/// Ported from the retired Python `build_synthetic_db.py` oracle. Keeping this
/// as Rust means the parity suite reproduces from a clean checkout with no Python
/// interpreter and no committed binary `.db` blob.
///
/// Graph shape: 3 files, 3 symbols (A, B, C); edges A→B, B→C.
fn build_synthetic_db(conn: &rusqlite::Connection) {
    conn.execute_batch(
        "CREATE TABLE file_index (path TEXT PRIMARY KEY, hash TEXT NOT NULL, language TEXT, symbol_count INTEGER NOT NULL DEFAULT 0, last_indexed REAL NOT NULL, mtime REAL);
         CREATE TABLE symbols (id INTEGER PRIMARY KEY AUTOINCREMENT, qualified_name TEXT NOT NULL, name TEXT NOT NULL, kind TEXT NOT NULL, language TEXT NOT NULL, path TEXT NOT NULL, line_start INTEGER NOT NULL, line_end INTEGER NOT NULL, signature TEXT NOT NULL DEFAULT '', docstring TEXT NOT NULL DEFAULT '', name_tokens TEXT NOT NULL DEFAULT '', caller_count INTEGER NOT NULL DEFAULT 0, is_hub INTEGER NOT NULL DEFAULT 0, coreness INTEGER, is_entry_point INTEGER NOT NULL DEFAULT 0, is_test INTEGER NOT NULL DEFAULT 0, file_hash TEXT NOT NULL DEFAULT '', indexed_at REAL NOT NULL DEFAULT 0);
         CREATE TABLE call_edges (id INTEGER PRIMARY KEY AUTOINCREMENT, from_symbol TEXT NOT NULL, to_symbol TEXT NOT NULL, call_site_line INTEGER, edge_confidence TEXT NOT NULL DEFAULT 'textual', from_path TEXT, to_path TEXT);

         INSERT INTO file_index (path, hash, language, symbol_count, last_indexed, mtime) VALUES ('src/a.rs', 'hash_a', 'rust', 1, 0, 0);
         INSERT INTO file_index (path, hash, language, symbol_count, last_indexed, mtime) VALUES ('src/b.rs', 'hash_b', 'rust', 1, 0, 0);
         INSERT INTO file_index (path, hash, language, symbol_count, last_indexed, mtime) VALUES ('src/c.rs', 'hash_c', 'rust', 1, 0, 0);

         INSERT INTO symbols (qualified_name, name, kind, language, path, line_start, line_end, caller_count, is_hub, coreness) VALUES ('A', 'A', 'function', 'rust', 'src/a.rs', 1, 10, 0, 0, 0);
         INSERT INTO symbols (qualified_name, name, kind, language, path, line_start, line_end, caller_count, is_hub, coreness) VALUES ('B', 'B', 'function', 'rust', 'src/b.rs', 1, 10, 1, 0, 0);
         INSERT INTO symbols (qualified_name, name, kind, language, path, line_start, line_end, caller_count, is_hub, coreness) VALUES ('C', 'C', 'function', 'rust', 'src/c.rs', 1, 10, 1, 0, 0);

         INSERT INTO call_edges (from_symbol, to_symbol, call_site_line, from_path, to_path) VALUES ('A', 'B', 5, 'src/a.rs', 'src/b.rs');
         INSERT INTO call_edges (from_symbol, to_symbol, call_site_line, from_path, to_path) VALUES ('B', 'C', 5, 'src/b.rs', 'src/c.rs');",
    )
    .expect("failed to build synthetic fixture DB");
}

fn setup_test_db() -> (rusqlite::Connection, PathBuf, PathBuf) {
    let fixture_dir = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("synthetic_project");

    let conn = rusqlite::Connection::open_in_memory().unwrap();
    build_synthetic_db(&conn);
    let project_root = fixture_dir.clone();
    (conn, project_root, fixture_dir)
}

fn sort_arrays(val: &mut Value) {
    match val {
        Value::Array(arr) => {
            for v in arr.iter_mut() {
                sort_arrays(v);
            }
            // Sort primitive arrays
            if arr.iter().all(|x| x.is_number() || x.is_string()) {
                arr.sort_by(|a, b| {
                    if let (Some(a_num), Some(b_num)) = (a.as_i64(), b.as_i64()) {
                        a_num.cmp(&b_num)
                    } else if let (Some(a_str), Some(b_str)) = (a.as_str(), b.as_str()) {
                        a_str.cmp(b_str)
                    } else {
                        std::cmp::Ordering::Equal
                    }
                });
            }
        }
        Value::Object(map) => {
            // Sort object keys implicitly by converting to BTreeMap (but serde_json::Map preserves order).
            // When we compare Value::Object, the key order doesn't matter for equality.
            for v in map.values_mut() {
                sort_arrays(v);
            }
        }
        _ => {}
    }
}

fn sanitize_paths(val: &mut Value, root: &Path) {
    let root_str = root.to_string_lossy().to_string();
    match val {
        Value::Object(map) => {
            let mut new_map = serde_json::Map::new();
            for (k, mut v) in map.clone() {
                sanitize_paths(&mut v, root);
                let new_k = k.replace(&root_str, "{PROJECT_ROOT}");
                new_map.insert(new_k, v);
            }
            *map = new_map;
        }
        Value::Array(arr) => {
            for v in arr.iter_mut() {
                sanitize_paths(v, root);
            }
        }
        Value::String(s) => {
            *s = s.replace(&root_str, "{PROJECT_ROOT}");
        }
        _ => {}
    }
}

#[test]
fn test_codeowners_parity() {
    let fixture_dir = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("synthetic_project");

    let expected_json = fs::read_to_string(fixture_dir.join("expected_codeowners.json")).unwrap();
    let expected: Value = serde_json::from_str(&expected_json).unwrap();

    let rust_output = load_codeowners(&fixture_dir);
    let actual: Value = serde_json::to_value(rust_output).unwrap();

    assert_eq!(expected, actual, "Codeowners parity failed");
}

#[test]
fn test_coverage_parity() {
    let fixture_dir = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("synthetic_project");

    let expected_json = fs::read_to_string(fixture_dir.join("expected_coverage.json")).unwrap();
    let expected: Value = serde_json::from_str(&expected_json).unwrap();

    let rust_output = load_coverage(&fixture_dir);
    let mut actual: Value = serde_json::to_value(rust_output).unwrap();

    sanitize_paths(&mut actual, &fixture_dir);
    sort_arrays(&mut actual);

    assert_eq!(expected, actual, "Coverage parity failed");
}

#[test]
fn test_coreness_parity() {
    let (conn, _project_root, _fixtures_dir) = setup_test_db();

    let rust_coreness = ci_core::graph::coreness::compute_coreness(&conn).unwrap();

    let json_path = _fixtures_dir.join("expected_coreness.json");
    let expected_json: std::collections::HashMap<String, i64> =
        serde_json::from_reader(std::fs::File::open(&json_path).unwrap()).unwrap();

    assert_eq!(rust_coreness.len(), expected_json.len());
    for (k, v) in expected_json {
        assert_eq!(
            *rust_coreness.get(&k).unwrap_or(&0),
            v,
            "Mismatch for {}",
            k
        );
    }
}

#[test]
fn test_path_parity() {
    let (conn, _project_root, _fixtures_dir) = setup_test_db();

    let result =
        ci_core::graph::path::bidirectional_bfs_path(&conn, "A", "C", 10, 3, 5000).unwrap();
    let routes = result.routes;
    let exists = result.exists;
    let terminated_by = result.terminated_by;

    assert_eq!(exists, Some(true));
    assert_eq!(terminated_by, None);

    let json_path = _fixtures_dir.join("expected_path.json");
    let expected_json: serde_json::Value =
        serde_json::from_reader(std::fs::File::open(&json_path).unwrap()).unwrap();

    let expected_routes = expected_json.as_array().unwrap();
    assert_eq!(routes.len(), expected_routes.len());
    assert_eq!(
        routes[0].len() - 1,
        expected_routes[0]["length"].as_i64().unwrap() as usize
    );

    let expected_steps = expected_routes[0]["steps"].as_array().unwrap();
    assert_eq!(routes[0].len(), expected_steps.len());
    assert_eq!(
        routes[0][0].symbol,
        expected_steps[0]["symbol"].as_str().unwrap()
    );
    assert_eq!(
        routes[0][1].symbol,
        expected_steps[1]["symbol"].as_str().unwrap()
    );
    assert_eq!(
        routes[0][2].symbol,
        expected_steps[2]["symbol"].as_str().unwrap()
    );
}

#[test]
fn test_hotspot_parity() {
    let (conn, _project_root, fixtures_dir) = setup_test_db();

    let config = ci_core::config::HotspotsConfig::default();
    // same arguments as generate_oracle.py
    let rust_hotspots = ci_core::analysis::hotspot::compute_hotspots(
        &fixtures_dir,
        &conn,
        &config,
        10,
        "1 year",
        0,
        false,
    );

    let json_path = fixtures_dir.join("expected_hotspot.json");
    let expected_json: serde_json::Value =
        serde_json::from_reader(std::fs::File::open(&json_path).unwrap()).unwrap();

    let expected_hotspots = expected_json.as_array().unwrap();
    let actual_hotspots = rust_hotspots.hotspots;

    assert_eq!(actual_hotspots.len(), expected_hotspots.len());
    // Since it's empty in this synthetic project, it will just be 0 == 0.
}

#[test]
fn test_formal_edges_integration() {
    let mut resolver = ci_core::resolver::formal::FormalResolver::new();

    // 1. Phải load thành công rules của Python
    resolver
        .load_python()
        .expect("Failed to load Python stack-graphs configuration");

    let source = r#"
class DatabaseConnector:
    def connect(self):
        pass

def main():
    db = DatabaseConnector()
    db.connect()
"#;

    // 2. Chạy FormalResolver (Stack Graphs) để extract paths
    let edges = resolver
        .resolve_file("python", "synthetic_db.py", source)
        .expect("Failed to resolve formal edges");

    // 3. Nghiệm thu: Kế hoạch yêu cầu formal edges phải xuất hiện
    let has_class_edge = edges.iter().any(|e| {
        e.reference_symbol == "DatabaseConnector" && e.definition_symbol == "DatabaseConnector"
    });
    let has_method_edge = edges
        .iter()
        .any(|e| e.reference_symbol == "connect" && e.definition_symbol == "connect");

    assert!(
        has_class_edge,
        "Parity harness output must contain formal edges for Python class instantiation"
    );
    assert!(
        has_method_edge,
        "Parity harness output must contain formal edges for Python method calls"
    );
}
