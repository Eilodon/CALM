# Rust Support Implementation Plan

> **For agentic workers:** Thực thi từng task theo thứ tự. Mỗi task là một vòng TDD khép kín
> (viết test đỏ → chạy xác nhận đỏ → code tối thiểu → chạy xác nhận xanh → commit). Đọc
> `docs/rust-support-research.md` trước để nắm bằng chứng thực nghiệm đằng sau plan này.

**Goal:** Đưa chất lượng call-graph/symbol-graph của `ci` trên Rust từ "chạy được nhưng thủng
nhiều" lên top-tier, bằng 2 tầng: (A) nâng cấp resolver syntactic Rust-native luôn chạy, zero
dependency mới; (B) overlay SCIP từ `rust-analyzer scip` (batch) như tầng confidence `Formal`
opt-in, additive-only.

**Architecture:** Tầng A sửa/mở rộng `indexer` + `resolver` sẵn có (tree-sitter, thuần cú
pháp — Rust có module/import resolution tĩnh nên heuristic đạt gần chính xác). Tầng B là module
mới `scip/` feature-gated: spawn `rust-analyzer scip` sinh file `.scip`, parse, rồi **chỉ nâng**
confidence của edge đã tồn tại lên `Formal` (không bao giờ tạo/xoá/hạ edge). Cả hai độc lập:
tắt B thì A vẫn nguyên vẹn.

**Tech Stack:** Rust, tree-sitter-rust 0.23, rusqlite/SQLite, `cargo metadata` (đã có trên máy
Rust dev), crate `scip` 0.9 (Phase B, feature-gated), rayon.

**Audit Gate:** N/A — plan này bắt nguồn từ research doc đã kiểm chứng thực nghiệm
(`docs/rust-support-research.md`), không qua audit-design. Chủ dự án duyệt trực tiếp.

**Risk Flags:**
- Phase B B4/B5 (ingest + wiring vào background thread) chạm graph đang hoạt động → bất biến
  "additive-only, không regression đường base" là điều kiện dừng cứng.
- A3 (module resolution) có phần `super::` chấp nhận không chính xác 100% ở Tier-0 (Phase B phủ
  phần dư) — dán nhãn confidence trung thực, không giả vờ formal.

**Invariants (không được phá ở bất kỳ task nào):**
1. Đường base (tree-sitter + ConservativeResolver) luôn chạy, luôn nhanh, robust trên code
   không build được. Phase B chỉ được **cộng thêm**.
2. `EdgeConfidence` giữ nguyên 4 biến thể (`Formal`/`Resolved`/`Inferred`/`Textual`) — tái dùng
   `Formal`, không thêm biến thể mới (ADR-0004 §5).
3. Zero dependency mới bắt buộc cho Phase A. Phase B thêm đúng 1 crate (`scip`), feature-gated,
   **off by default**.
4. Trước mỗi commit: `cargo fmt --all` + `cargo clippy --all-targets -- -D warnings` sạch.

---

## File Structure

**Phase A (sửa file có sẵn):**
- `crates/ci-core/src/indexer/imports.rs` — sửa `parse_rust_import` (A1)
- `crates/ci-core/src/indexer/crate_map.rs` — **mới**: workspace crate→src-root map (A2)
- `crates/ci-core/src/indexer/mod.rs` — khai báo `pub mod crate_map;` (A2)
- `crates/ci-core/src/indexer/pipeline.rs` — dùng crate map trong resolve import (A3)
- `crates/ci-core/src/indexer/lang_constants.rs` — thêm `function_signature_item` (A4)
- `crates/ci-core/src/indexer/parser.rs` — `walk_symbols` per-node-kind class name field cho
  `trait_item` (A4) + constructor type inference (A5)
- `crates/ci-core/tests/fixtures/rust_workspace/` — **mới**: fixture (Task 0)

**Phase B (module mới):**
- `crates/ci-core/src/scip/mod.rs` — **mới**: module gốc, feature `scip-overlay`
- `crates/ci-core/src/scip/runner.rs` — **mới**: detect + spawn rust-analyzer
- `crates/ci-core/src/scip/parse.rs` — **mới**: đọc `.scip` → occurrences
- `crates/ci-core/src/scip/ingest.rs` — **mới**: upgrade edges → Formal
- `crates/ci-core/src/scip/cache.rs` — **mới**: cache key + staleness
- `crates/ci-core/src/lib.rs` — khai báo `pub mod scip;` (feature-gated)
- `crates/ci-core/src/config.rs` — thêm `RustConfig`/`ScipConfig`
- `crates/ci-core/Cargo.toml` — thêm `scip` dep + feature `scip-overlay`
- `crates/ci-server/src/lib.rs` — gọi overlay sau `phase=ready`
- `benchmarks/b2_call_graph_quality/` — **mới**: harness đo precision/recall vs SCIP oracle,
  lấp slot "B2 | Call Graph Resolution Quality | Planned" có sẵn trong `benchmarks/README.md` (B6)

---

## Phase 0 — Fixture & baseline

### Task 0: Rust fixture workspace + baseline assertions

Fixture đa-crate cố tình chứa các hình dạng khó (re-export, cross-crate, trait/dyn, constructor
không annotation). Trở thành ground truth cho toàn bộ Phase A. **Không** cố tình chứa lỗi
compile ở đây (khác workspace thử nghiệm trong research) — fixture này để assert kết quả đúng,
cần deterministic.

**Files:**
- Create: `crates/ci-core/tests/fixtures/rust_workspace/Cargo.toml`
- Create: `crates/ci-core/tests/fixtures/rust_workspace/core/Cargo.toml`
- Create: `crates/ci-core/tests/fixtures/rust_workspace/core/src/lib.rs`
- Create: `crates/ci-core/tests/fixtures/rust_workspace/core/src/engine.rs`
- Create: `crates/ci-core/tests/fixtures/rust_workspace/app/Cargo.toml`
- Create: `crates/ci-core/tests/fixtures/rust_workspace/app/src/main.rs`
- Test: `crates/ci-core/tests/rust_indexing.rs`

- [ ] **Step 1: Tạo fixture workspace**

`.../rust_workspace/Cargo.toml`:
```toml
[workspace]
members = ["core", "app"]
resolver = "2"
```

`.../rust_workspace/core/Cargo.toml`:
```toml
[package]
name = "demo-core"
version = "0.1.0"
edition = "2021"
```

`.../rust_workspace/core/src/lib.rs`:
```rust
pub mod engine;

// Re-export façade — the `pub use` bug (#1) makes this invisible today.
pub use engine::Engine;

pub trait Runner {
    fn run(&self) -> u32;
}

pub struct FastRunner;

impl Runner for FastRunner {
    fn run(&self) -> u32 {
        1
    }
}

pub fn call_dynamic(r: &dyn Runner) -> u32 {
    r.run()
}
```

`.../rust_workspace/core/src/engine.rs`:
```rust
pub struct Engine {
    pub id: u32,
}

impl Engine {
    pub fn new() -> Self {
        Engine { id: 0 }
    }

    pub fn start(&self) -> u32 {
        self.id + 1
    }
}
```

`.../rust_workspace/app/Cargo.toml`:
```toml
[package]
name = "demo-app"
version = "0.1.0"
edition = "2021"

[dependencies]
demo-core = { path = "../core" }
```

`.../rust_workspace/app/src/main.rs`:
```rust
use demo_core::{call_dynamic, Engine, FastRunner};

fn main() {
    let e = Engine::new();
    let n = e.start();
    let d = call_dynamic(&FastRunner);
    println!("{} {}", n, d);
}
```

- [ ] **Step 2: Viết test scaffold ghi lại baseline (đỏ có chủ đích)**

`crates/ci-core/tests/rust_indexing.rs`:
```rust
use rusqlite::Connection;
use std::path::PathBuf;

fn fixture_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/rust_workspace")
}

/// Index the fixture workspace into an in-memory DB and return the connection.
fn index_fixture() -> Connection {
    let mut conn = Connection::open_in_memory().unwrap();
    ci_core::db::schema::init_db(&conn).unwrap();
    let phase = std::sync::Arc::new(std::sync::RwLock::new(
        ci_core::types::IndexingPhase::Scanning,
    ));
    ci_core::indexer::pipeline::run_indexing_pipeline(&mut conn, &fixture_root(), phase).unwrap();
    conn
}

fn count(conn: &Connection, sql: &str) -> i64 {
    conn.query_row(sql, [], |r| r.get(0)).unwrap()
}

#[test]
fn pub_use_reexport_is_indexed() {
    let conn = index_fixture();
    // The `pub use engine::Engine;` line must produce an import edge.
    assert!(
        count(
            &conn,
            "SELECT COUNT(*) FROM import_edges \
             WHERE from_path = 'core/src/lib.rs' AND module_name = 'engine'",
        ) >= 1,
        "pub use re-export must be recorded as an import edge"
    );
}
```

- [ ] **Step 3: Chạy — xác nhận FAIL** `cargo test -p ci-core --test rust_indexing pub_use_reexport_is_indexed` → expected: FAIL (bug #1 — `pub use` bị bỏ qua)
- [ ] **Step 4: Commit fixture** `git commit -am "test(rust): fixture workspace + failing pub-use baseline"`

---

## Phase A — Tier-0 syntactic upgrade

### Task A1: Fix `pub use` invisibility (R0.1)

**Nguyên nhân gốc** (đã verify bằng grammar node-types): node `use_declaration` với `pub use`
có child `visibility_modifier`, nên `source[node.byte_range()]` bắt đầu bằng `"pub use ..."`.
`parse_rust_import` làm `text.strip_prefix("use ")?` → trả `None` → toàn bộ re-export façade
biến mất khỏi graph.

**Files:**
- Modify: `crates/ci-core/src/indexer/imports.rs:154-177`
- Test: `crates/ci-core/src/indexer/imports.rs` (module `#[cfg(test)]` có sẵn)

- [ ] **Step 1: Viết test đỏ** (thêm vào `mod tests` trong `imports.rs`, cạnh `rust_use_group`):
```rust
#[test]
fn rust_pub_use_reexport() {
    let i = one("pub use engine::Engine;\n", "rust");
    assert_eq!(i.module_name, "engine");
    assert_eq!(i.imported_names, vec!["Engine"]);
}

#[test]
fn rust_pub_crate_use() {
    let i = one("pub(crate) use crate::a::b;\n", "rust");
    assert_eq!(i.module_name, "crate::a");
    assert_eq!(i.imported_names, vec!["b"]);
}
```

- [ ] **Step 2: Chạy — xác nhận FAIL** `cargo test -p ci-core --lib imports::tests::rust_pub_use_reexport` → expected: FAIL (`one` unwrap None)
- [ ] **Step 3: Sửa `parse_rust_import`** — thêm bước strip visibility trước khi strip `use `. Thay thân hàm `parse_rust_import` (imports.rs:154-177) bằng:
```rust
/// Strip an optional leading Rust visibility modifier from `text`, returning the
/// remainder. Handles `pub`, `pub(crate)`, `pub(super)`, `pub(self)`, and
/// `pub(in a::b)` — the parenthesized form may contain spaces, so we skip to the
/// matching `)` rather than splitting on whitespace.
fn strip_rust_visibility(text: &str) -> &str {
    let t = text.trim_start();
    let Some(rest) = t.strip_prefix("pub") else {
        return t;
    };
    let rest = rest.trim_start();
    match rest.strip_prefix('(') {
        Some(after) => match after.split_once(')') {
            Some((_, tail)) => tail.trim_start(),
            None => rest, // malformed; leave as-is
        },
        None => rest,
    }
}

fn parse_rust_import(text: &str) -> Option<ParsedImport> {
    // use a::b::c;  use a::b::{c, d};  use a::b as x;  use a::b::*;
    // and the pub/pub(...) re-export forms of each.
    let rest = strip_rust_visibility(text)
        .strip_prefix("use ")?
        .trim()
        .trim_start_matches("::");
    if let Some((prefix, list)) = rest.split_once("::{") {
        let list = list.trim_end_matches('}');
        let names = list.split(',').filter_map(bound_name).collect();
        Some(ParsedImport {
            module_name: prefix.trim().to_string(),
            imported_names: names,
        })
    } else {
        let module = rest
            .split_once(" as ")
            .map(|(m, _)| m.trim())
            .unwrap_or(rest)
            .trim_end_matches("::*")
            .to_string();
        let names = bound_name(rest).into_iter().collect();
        Some(ParsedImport {
            module_name: module,
            imported_names: names,
        })
    }
}
```

> **Phạm vi Task A1**: vẫn thuần string-manipulation (chỉ thêm bước strip visibility) — **không**
> chuyển sang AST-based traversal của `use_declaration` như `docs/rust-support-research.md` §R0.1
> mô tả ("parse bằng cấu trúc node tree-sitter... sửa luôn nested groups"). Nested groups như
> `use a::{b::{c, d}, e}` vẫn chưa được xử lý đúng sau task này — không phải regression (đã sai
> y hệt trước đó), chỉ là scope Task A1 hẹp hơn cách research doc mô tả. Không chặn merge, chỉ
> ghi rõ để không ai tưởng nested groups đã được sửa xong.

- [ ] **Step 4: Chạy — xác nhận PASS** `cargo test -p ci-core --lib imports::tests` và `cargo test -p ci-core --test rust_indexing pub_use_reexport_is_indexed` → expected: cả hai PASS
- [ ] **Step 5: fmt + clippy + commit** `cargo fmt --all && cargo clippy --all-targets -- -D warnings && git commit -am "fix(imports): recognize pub use re-exports (Rust R0.1)"`

---

### Task A2: Workspace crate map (R0.2)

Cross-crate import chết vì không có mapping `crate-name → src-root`. Build map một lần từ
`cargo metadata --no-deps` (đo được 44ms), fallback quét `Cargo.toml` khi không có `cargo`
(giữ zero-dependency install).

**Files:**
- Create: `crates/ci-core/src/indexer/crate_map.rs`
- Modify: `crates/ci-core/src/indexer/mod.rs` (thêm `pub mod crate_map;`)
- Test: trong `crate_map.rs` (`#[cfg(test)]`)

- [ ] **Step 1: Khai báo module** — thêm dòng vào `crates/ci-core/src/indexer/mod.rs`:
```rust
pub mod crate_map;
```

- [ ] **Step 2: Viết test đỏ** trong `crate_map.rs`:
```rust
#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn fixture() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/rust_workspace")
    }

    #[test]
    fn maps_crate_names_to_src_roots() {
        let m = CrateMap::build(&fixture());
        // `-` in the package name is normalized to `_` (matches the identifier
        // used in `use demo_core::...`).
        assert_eq!(m.root_of("demo_core"), Some("core/src"));
        assert_eq!(m.root_of("demo_app"), Some("app/src"));
    }

    #[test]
    fn resolves_owning_crate_of_a_file() {
        let m = CrateMap::build(&fixture());
        let (name, root) = m.crate_of_file("core/src/engine.rs").unwrap();
        assert_eq!(name, "demo_core");
        assert_eq!(root, "core/src");
    }
}
```

- [ ] **Step 3: Chạy — xác nhận FAIL** `cargo test -p ci-core --lib crate_map` → expected: FAIL (chưa có `CrateMap`)
- [ ] **Step 4: Implement `CrateMap`** — nội dung `crate_map.rs` (phần trên test):
```rust
use std::collections::HashMap;
use std::path::Path;
use std::process::Command;

/// Maps a workspace's crate names to their source-root directories, so Rust
/// `use other_crate::Item` and `use crate::mod::Item` imports resolve to real
/// indexed files. Built once per index pass.
#[derive(Debug, Default)]
pub struct CrateMap {
    /// normalized crate name (`-` → `_`) → src-root dir, project-root-relative,
    /// forward-slashed, no trailing slash (e.g. `"crates/ci-core/src"`).
    roots: HashMap<String, String>,
}

impl CrateMap {
    /// Build from `cargo metadata --no-deps` when `cargo` is available; otherwise
    /// fall back to scanning `Cargo.toml` files. Never fails — an empty map just
    /// means cross-crate resolution degrades to today's behavior.
    pub fn build(project_root: &Path) -> Self {
        Self::from_cargo_metadata(project_root)
            .unwrap_or_else(|| Self::from_toml_scan(project_root))
    }

    pub fn root_of(&self, crate_name: &str) -> Option<&str> {
        self.roots.get(crate_name).map(String::as_str)
    }

    /// The (crate name, src-root) that owns `rel_path` — longest src-root prefix.
    pub fn crate_of_file(&self, rel_path: &str) -> Option<(&str, &str)> {
        self.roots
            .iter()
            .filter(|(_, root)| {
                rel_path == root.as_str() || rel_path.starts_with(&format!("{root}/"))
            })
            .max_by_key(|(_, root)| root.len())
            .map(|(name, root)| (name.as_str(), root.as_str()))
    }

    pub fn is_empty(&self) -> bool {
        self.roots.is_empty()
    }

    fn from_cargo_metadata(project_root: &Path) -> Option<Self> {
        let out = Command::new("cargo")
            .args(["metadata", "--no-deps", "--format-version", "1"])
            .current_dir(project_root)
            .output()
            .ok()?;
        if !out.status.success() {
            return None;
        }
        let json: serde_json::Value = serde_json::from_slice(&out.stdout).ok()?;
        let mut roots = HashMap::new();
        for pkg in json.get("packages")?.as_array()? {
            let name = pkg.get("name")?.as_str()?.replace('-', "_");
            // The lib target (fallback: any target) gives the crate's root file.
            let targets = pkg.get("targets")?.as_array()?;
            let root_file = targets
                .iter()
                .find(|t| {
                    t.get("kind")
                        .and_then(|k| k.as_array())
                        .is_some_and(|ks| ks.iter().any(|k| k == "lib"))
                })
                .or_else(|| targets.first())
                .and_then(|t| t.get("src_path"))
                .and_then(|p| p.as_str())?;
            if let Some(rel) = rel_src_dir(project_root, root_file) {
                roots.insert(name, rel);
            }
        }
        if roots.is_empty() {
            None
        } else {
            Some(Self { roots })
        }
    }

    fn from_toml_scan(project_root: &Path) -> Self {
        let mut roots = HashMap::new();
        for entry in crate::walk::build_walker(project_root, &[]) {
            let Ok(entry) = entry else { continue };
            if entry.file_name() != "Cargo.toml" {
                continue;
            }
            let path = entry.path();
            let Ok(text) = std::fs::read_to_string(path) else {
                continue;
            };
            let Ok(doc) = text.parse::<toml::Value>() else {
                continue;
            };
            let Some(name) = doc
                .get("package")
                .and_then(|p| p.get("name"))
                .and_then(|n| n.as_str())
            else {
                continue; // virtual/workspace-only manifest
            };
            // Convention: <manifest_dir>/src is the crate root unless [lib] path overrides.
            let manifest_dir = path.parent().unwrap_or(project_root);
            let lib_path = doc
                .get("lib")
                .and_then(|l| l.get("path"))
                .and_then(|p| p.as_str());
            let src_dir = match lib_path {
                Some(p) => manifest_dir.join(p).parent().map(Path::to_path_buf),
                None => Some(manifest_dir.join("src")),
            };
            if let Some(dir) = src_dir
                && let Some(rel) = rel_dir(project_root, &dir)
            {
                roots.insert(name.replace('-', "_"), rel);
            }
        }
        Self { roots }
    }
}

/// Project-root-relative, forward-slashed parent dir of an absolute src file path.
fn rel_src_dir(project_root: &Path, abs_file: &str) -> Option<String> {
    let parent = Path::new(abs_file).parent()?;
    rel_dir(project_root, parent)
}

fn rel_dir(project_root: &Path, abs_dir: &Path) -> Option<String> {
    let rel = abs_dir.strip_prefix(project_root).ok()?;
    Some(rel.to_string_lossy().replace('\\', "/"))
}
```

> **Lưu ý cho executor:** `crate::walk::build_walker(root, &[])` là walker gitignore-aware sẵn
> có (dùng ở `pipeline.rs:37`). `toml` và `serde_json` đã là dependency của `ci-core`
> (`Cargo.toml:9-10`) — không thêm dep mới.

- [ ] **Step 5: Chạy — xác nhận PASS** `cargo test -p ci-core --lib crate_map` → expected: PASS
- [ ] **Step 6: fmt + clippy + commit** `cargo fmt --all && cargo clippy --all-targets -- -D warnings && git commit -am "feat(indexer): workspace crate→src-root map (Rust R0.2)"`

---

### Task A3: Rust cross-crate + module resolution (R0.3)

Thay đường strip-prefix sai trong `resolve_module_to_path` bằng resolver Rust dùng `CrateMap`.

> **Lưu ý số dòng**: các trích dẫn `pipeline.rs:NNN` dưới đây đã cập nhật theo commit
> `9d45c63` ("prefer same-file candidate when resolving a call by bare name", đã merge trước
> khi plan này được viết ra nhưng phần phân tích ban đầu không tính tới), commit này chèn thêm
> ~21 dòng vào giữa file (khối "same-file preference" trong `rebuild_graph`, dòng ~440-472).
> Khối đó nằm **trước** và **tách biệt** với `resolve_import_targets`/`resolve_module_to_path`
> mà Task A3 sửa — không có tương tác chức năng, chỉ cần biết để số dòng khớp bản hiện tại.
Đây là task đóng gap #2 (cross-crate `to_path = NULL`). Scope Tier-0: `crate::`, tên crate
ngoài, `self::` resolve chính xác; `super::` chấp nhận xấp xỉ (climb theo thư mục) và ghi chú
rõ — phần dư để Phase B phủ.

**Files:**
- Modify: `crates/ci-core/src/indexer/pipeline.rs` — `resolve_import_targets` (489-522) build & truyền `CrateMap`; thêm `resolve_rust_module`; rẽ nhánh rust trong `resolve_module_to_path`
- Test: `crates/ci-core/tests/rust_indexing.rs`

- [ ] **Step 1: Viết test đỏ** (thêm vào `rust_indexing.rs`):
```rust
#[test]
fn cross_crate_import_resolves_to_path() {
    let conn = index_fixture();
    // `use demo_core::{...}` in app/src/main.rs must resolve to the demo-core crate.
    // Item imports resolve to the crate root file (lib.rs) that re-exports them.
    let to_path: Option<String> = conn
        .query_row(
            "SELECT to_path FROM import_edges \
             WHERE from_path = 'app/src/main.rs' AND module_name = 'demo_core'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(to_path.as_deref(), Some("core/src/lib.rs"));
}

#[test]
fn crate_relative_import_resolves() {
    let conn = index_fixture();
    // `pub use engine::Engine;` in core/src/lib.rs → engine module → core/src/engine.rs
    let to_path: Option<String> = conn
        .query_row(
            "SELECT to_path FROM import_edges \
             WHERE from_path = 'core/src/lib.rs' AND module_name = 'engine'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(to_path.as_deref(), Some("core/src/engine.rs"));
}
```

- [ ] **Step 2: Chạy — xác nhận FAIL** `cargo test -p ci-core --test rust_indexing cross_crate_import_resolves_to_path` → expected: FAIL (`to_path` = NULL)

- [ ] **Step 3: Thêm `resolve_rust_module`** vào `pipeline.rs` (đặt ngay trên `resolve_module_to_path`, ~line 523):
```rust
/// Resolve a Rust `use` module path to an indexed file, using the workspace
/// crate map. Handles `crate::`, `self::`, an external crate-name prefix, and a
/// best-effort `super::`. Returns `None` for paths that leave the workspace
/// (std, third-party crates) — those correctly keep `to_path = NULL`.
///
/// `super::` climbs one directory per `super` from the importing file's dir.
/// This is exact for `foo.rs`-style modules and off-by-one for `foo/mod.rs`
/// modules; the residual is covered by the optional SCIP overlay, so a miss
/// here simply falls back to today's NULL, never a wrong edge.
fn resolve_rust_module(
    from_path: &str,
    module: &str,
    crate_map: &crate::indexer::crate_map::CrateMap,
    known: &HashSet<String>,
) -> Option<String> {
    let segs: Vec<&str> = module.split("::").filter(|s| !s.is_empty()).collect();
    if segs.is_empty() {
        return None;
    }
    let from_dir = std::path::Path::new(from_path)
        .parent()
        .map(|p| p.to_string_lossy().replace('\\', "/"))
        .unwrap_or_default();

    // (base directory to resolve the remaining segments under, remaining segments)
    let (base_dir, rest): (String, &[&str]) = match segs[0] {
        "crate" => {
            let (_, root) = crate_map.crate_of_file(from_path)?;
            (root.to_string(), &segs[1..])
        }
        "self" => (from_dir.clone(), &segs[1..]),
        "super" => {
            let mut dir = from_dir.clone();
            let mut i = 0;
            while i < segs.len() && segs[i] == "super" {
                dir = parent_of(&dir);
                i += 1;
            }
            (dir, &segs[i..])
        }
        other => {
            // External crate name → its src root; unknown → leaves the workspace.
            let root = crate_map.root_of(&other.replace('-', "_"))?;
            (root.to_string(), &segs[1..])
        }
    };

    // Try the full remaining path and, for item imports (`use a::b::Item`), its
    // parent — plus `mod.rs` / crate-root conventions.
    let joined = rest.join("/");
    let mut bases: Vec<String> = Vec::new();
    if joined.is_empty() {
        bases.push(base_dir.clone());
    } else {
        bases.push(join_rel(&base_dir, &joined));
        if let Some((parent, _)) = joined.rsplit_once('/') {
            bases.push(join_rel(&base_dir, parent));
        } else {
            // Single trailing item (`crate::Item`) → the crate root file itself.
            bases.push(base_dir.clone());
        }
    }

    for base in &bases {
        let base = base.trim_start_matches('/');
        for cand in [
            format!("{base}.rs"),
            format!("{base}/mod.rs"),
            format!("{base}/lib.rs"),
        ] {
            if known.contains(&cand) {
                return Some(cand);
            }
        }
        if known.contains(base) {
            return Some(base.to_string());
        }
    }
    None
}
```

- [ ] **Step 4: Rẽ nhánh rust trong `resolve_module_to_path`** — sửa đầu hàm (pipeline.rs:547, ngay sau khối `let m = ...; if m.is_empty()`), thêm tham số `crate_map` và ưu tiên resolver rust cho file `.rs`. Đổi signature:
```rust
fn resolve_module_to_path(
    from_path: &str,
    module: &str,
    known: &HashSet<String>,
    crate_map: &crate::indexer::crate_map::CrateMap,
) -> Option<String> {
    let m = module.trim().trim_matches(|c| c == '"' || c == '\'');
    if m.is_empty() {
        return None;
    }
    // Rust: use the crate-map-aware resolver first; fall through to the generic
    // convention scan only if it finds nothing (keeps single-crate repos working
    // even when the crate map is empty).
    if from_path.ends_with(".rs")
        && let Some(hit) = resolve_rust_module(from_path, m, crate_map, known)
    {
        return Some(hit);
    }
    // ... existing generic body unchanged from here ...
```

- [ ] **Step 5: Build & truyền `CrateMap` trong `resolve_import_targets`** — sửa hàm (pipeline.rs:510). Nó cần `project_root` để build map; hiện chỉ nhận `tx`. Thêm tham số. Sửa signature + thân:
```rust
fn resolve_import_targets(
    tx: &rusqlite::Transaction,
    crate_map: &crate::indexer::crate_map::CrateMap,
) -> rusqlite::Result<()> {
    // ... `known` and `rows` blocks unchanged ...
    let targets: Vec<Option<String>> = rows
        .par_iter()
        .map(|(_, from_path, module)| resolve_module_to_path(from_path, module, &known, crate_map))
        .collect();
    // ... UPDATE loop unchanged ...
}
```

Và tại call site trong `rebuild_graph` (pipeline.rs:501), đổi `resolve_import_targets(tx)?;` thành nhận map. `rebuild_graph` cũng chưa có `project_root` — thêm tham số vào nó và truyền từ 2 call site (`run_indexing_pipeline` ~733, incremental ~844). Chuỗi thay đổi:
```rust
// rebuild_graph signature:
fn rebuild_graph(
    tx: &rusqlite::Transaction,
    hub_config: &crate::config::HubThresholdConfig,
    crate_map: &crate::indexer::crate_map::CrateMap,
) -> rusqlite::Result<()> {
    // ... existing body ...
    resolve_import_targets(tx, crate_map)?;
    // ...
}
```
Ở cả 2 call site của `rebuild_graph`, build map một lần trước khi gọi:
```rust
let crate_map = crate::indexer::crate_map::CrateMap::build(root);
rebuild_graph(&tx, &config.hub_threshold, &crate_map)?;
```
(`root`/`project_root` có sẵn trong cả `run_indexing_pipeline` và hàm incremental — kiểm tra tên biến địa phương và dùng đúng.)

- [ ] **Step 6: Chạy — xác nhận PASS** `cargo test -p ci-core --test rust_indexing` → expected: cross_crate + crate_relative PASS. Chạy lại full `cargo test -p ci-core` để chắc không regression resolver các ngôn ngữ khác (generic branch giữ nguyên).
- [ ] **Step 7: fmt + clippy + commit** `cargo fmt --all && cargo clippy --all-targets -- -D warnings && git commit -am "feat(resolver): Rust cross-crate & module resolution via crate map (R0.3)"`

---

### Task A4: Trait/impl method declarations as symbols (R0.5)

Trait method decl (`fn run(&self) -> u32;`) là node `function_signature_item`, **không** nằm
trong `function_node_types` của Rust → `Runner::run` vô hình. Thêm node kind này. Default arm
của `node_kind_to_symbol_kind` (đã đọc: parser.rs:54-60) map function-like không rõ →
`Method` khi `in_class`, đúng vì trait đặt `class_context`.

**Files:**
- Modify: `crates/ci-core/src/indexer/lang_constants.rs:26-34` (rust `function_node_types`)
- Modify: `crates/ci-core/src/indexer/parser.rs:294-301` (`walk_symbols` — per-node-kind class
  name field cho Rust `trait_item`; xem Step 4)
- Test: `crates/ci-core/tests/rust_indexing.rs`

- [ ] **Step 1: Viết test đỏ**:
```rust
#[test]
fn trait_method_declaration_is_a_symbol() {
    let conn = index_fixture();
    // The trait method `Runner::run` (declaration only, no body) must be indexed,
    // qualified by its trait, so "who declares run" / trait API is queryable.
    assert_eq!(
        count(
            &conn,
            "SELECT COUNT(*) FROM symbols \
             WHERE qualified_name = 'core/src/lib.rs::Runner::run' AND kind = 'method'",
        ),
        1
    );
}
```

- [ ] **Step 2: Chạy — xác nhận FAIL** `cargo test -p ci-core --test rust_indexing trait_method_declaration_is_a_symbol` → expected: FAIL
- [ ] **Step 3: Thêm node kind** — sửa rust `function_node_types` trong `lang_constants.rs:27`:
```rust
        "rust" => Some(LangConstants {
            function_node_types: &[
                "function_item",
                "function_signature_item",
                "struct_item",
                "trait_item",
                "impl_item",
            ],
            name_field: "name",
            docstring_type: Some("line_comment"),
            call_node_types: &["call_expression"],
            call_function_field: "function",
            class_node_types: &["impl_item", "trait_item"],
            class_name_field: "type",
        }),
```

- [ ] **Step 4: Sửa `walk_symbols` cho field tên của `trait_item`** — **đã xác nhận bằng
  `node-types.json` thật của `tree-sitter-rust 0.23.3`, không phải giả định**: `impl_item` có
  field `type` (kiểu Self) và field optional `trait`; `trait_item` có field `name` (kiểu
  `type_identifier`) — **không có field `type`**. `class_name_field` là một hằng số dùng chung
  cho cả ngôn ngữ (`"type"` cho rust), nên chỉ thêm `trait_item` vào `class_node_types` như
  Step 3 là **chưa đủ**: `node.child_by_field_name(lc.class_name_field)` trong `walk_symbols`
  (`parser.rs:295-297`) sẽ tra field `"type"` trên `trait_item`, field này không tồn tại →
  `None` → rơi về `enclosing_class` (thường là `None` ở top-level) → method trong trait
  **không** nhận được `class_context = "Runner"`, và oracle test ở Step 1 (`qualified_name =
  'core/src/lib.rs::Runner::run'`) sẽ FAIL dù Step 3 đã áp dụng đúng. Sửa `walk_symbols`
  (`parser.rs:294-301`) để chọn field theo node kind thay vì dùng thẳng `lc.class_name_field`:

```rust
    // Entering a class/impl sets the context for its descendants. Rust's
    // `trait_item` names itself via field `name` (a `type_identifier`) — it
    // does not share `impl_item`'s `class_name_field` ("type", the Self
    // type) — so the field to read can't come from the single
    // per-language `class_name_field` constant alone for this node kind.
    let child_class = if lc.class_node_types.contains(&node.kind()) {
        let name_field = if node.kind() == "trait_item" {
            "name"
        } else {
            lc.class_name_field
        };
        node.child_by_field_name(name_field)
            .map(|n| source[n.byte_range()].to_string())
            .or_else(|| enclosing_class.clone())
    } else {
        enclosing_class.clone()
    };
```

- [ ] **Step 5: Chạy — xác nhận PASS** `cargo test -p ci-core --test rust_indexing` và full `cargo test -p ci-core` → expected: PASS, không regression (đặc biệt các test symbol Rust có sẵn trong `parser.rs`/`pipeline.rs`).
- [ ] **Step 6: fmt + clippy + commit** `cargo fmt --all && cargo clippy --all-targets -- -D warnings && git commit -am "feat(indexer): index Rust trait method declarations (R0.5)"`

---

### Task A5: Constructor type inference (R0.6)

`let e = Engine::new(); e.start()` → hiện `e.start()` là `textual` (không có annotation cho
tier-2). Mở rộng `extract_type_map` để suy `let x = Foo::new()` / `Foo::default()` /
`Foo { .. }` → `type_map[x] = Foo`. Tier-2 sẵn có sẽ nâng `e.start()` lên `inferred` với
`target_class = Engine`.

**Files:**
- Modify: `crates/ci-core/src/indexer/parser.rs` — `extract_type_map` walk (798-822) thêm pass rust cho `let_declaration` không type
- Test: `crates/ci-core/tests/rust_indexing.rs`

- [ ] **Step 1: Viết test đỏ**:
```rust
#[test]
fn constructor_binding_infers_receiver_type() {
    let conn = index_fixture();
    // `let e = Engine::new(); e.start()` — e's type is inferred from the
    // constructor, so the call resolves to Engine::start with >= inferred confidence.
    let conf: String = conn
        .query_row(
            "SELECT edge_confidence FROM call_edges \
             WHERE from_symbol = 'app/src/main.rs::main' \
               AND to_symbol = 'core/src/engine.rs::Engine::start'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert!(
        matches!(conf.as_str(), "inferred" | "resolved" | "formal"),
        "expected >= inferred, got {conf}"
    );
}
```

- [ ] **Step 2: Chạy — xác nhận FAIL** `cargo test -p ci-core --test rust_indexing constructor_binding_infers_receiver_type` → expected: FAIL (textual, edge có thể không tồn tại theo target_class)
- [ ] **Step 3: Thêm inference** — trong `extract_type_map_from_tree` (parser.rs, hàm chứa vòng `while let Some(node) = stack.pop()` ~810), thêm một nhánh rust: khi gặp `let_declaration` không có field `type` nhưng có `value` là constructor. Chèn ngay sau khối `if binding_kinds.contains(...)`:
```rust
        // Rust constructor inference: `let x = Foo::new(...)`, `Foo::default()`,
        // or `Foo { .. }` binds x to type Foo even without a type annotation.
        if language == "rust"
            && node.kind() == "let_declaration"
            && node.child_by_field_name("type").is_none()
            && let Some(pat) = node.child_by_field_name("pattern")
            && pat.kind() == "identifier"
            && let Some(value) = node.child_by_field_name("value")
            && let Some(ty) = rust_constructor_type(value, source)
        {
            map.insert(source[pat.byte_range()].to_string(), ty);
        }
```
Và thêm helper (cạnh `binding_names_and_type`):
```rust
/// The type constructed by a Rust expression used as a `let` initializer:
/// `Foo::new(..)` / `Foo::default()` / `Foo::with_x(..)` → `Foo`;
/// `Foo { .. }` (struct literal) → `Foo`. Returns `None` for anything else.
fn rust_constructor_type(value: tree_sitter::Node, source: &str) -> Option<String> {
    match value.kind() {
        // Foo::new(...) — a call whose function is a scoped identifier.
        "call_expression" => {
            let func = value.child_by_field_name("function")?;
            if func.kind() != "scoped_identifier" {
                return None;
            }
            let path = func.child_by_field_name("path")?;
            // The type is the last path segment before the associated fn name.
            let seg = source[path.byte_range()].rsplit("::").next()?;
            first_type_ident(seg)
        }
        // Foo { .. } — struct literal names its type directly.
        "struct_expression" => {
            let name = value.child_by_field_name("name")?;
            first_type_ident(&source[name.byte_range()])
        }
        _ => None,
    }
}

/// Keep a leading UpperCamelCase type identifier from `seg` (drop generics etc.);
/// returns None if it doesn't look type-like (avoids treating `foo::bar()` module
/// calls as constructors).
fn first_type_ident(seg: &str) -> Option<String> {
    let ident: String = seg
        .trim()
        .chars()
        .take_while(|c| c.is_alphanumeric() || *c == '_')
        .collect();
    if ident.chars().next()?.is_uppercase() {
        Some(ident)
    } else {
        None
    }
}
```
> **Executor verify:** node kind cho struct literal trong tree-sitter-rust 0.23 là
> `struct_expression`; field function/path của `call_expression`/`scoped_identifier` — nếu
> Step 4 fail, dump AST của `let e = Engine::new();` bằng một test nhỏ để chỉnh tên
> field/kind. Test Step 1 là oracle.

- [ ] **Step 4: Chạy — xác nhận PASS** `cargo test -p ci-core --test rust_indexing` + full `cargo test -p ci-core` → expected: PASS, không regression tier-2 các ngôn ngữ khác (nhánh mới gated `language == "rust"`).
- [ ] **Step 5: fmt + clippy + commit** `cargo fmt --all && cargo clippy --all-targets -- -D warnings && git commit -am "feat(resolver): Rust constructor type inference for tier-2 (R0.6)"`

**Kết thúc Phase A:** chạy `ci index` trên fixture, xác nhận DB có ≥1 import edge cho `pub use`,
`to_path` cross-crate không NULL, `Runner::run` là symbol, `e.start()` là inferred. Đây là mốc
"Rust syntactic top-tier" — độc lập, ship được, không phụ thuộc Phase B.

---

## Phase B — Tier-1 SCIP overlay (opt-in, additive-only)

> **Bất biến Phase B:** chỉ **nâng** confidence của edge đã tồn tại lên `Formal`. Không tạo edge
> mới, không xoá, không hạ. Tắt feature/config → hành vi giống hệt cuối Phase A. Đây là điều
> kiện dừng cứng — bất kỳ task nào vi phạm phải dừng và báo lại.

### Task B1: `scip` dependency + SCIP parse module

**Files:**
- Modify: `crates/ci-core/Cargo.toml` (dep `scip` + feature `scip-overlay`)
- Modify: `crates/ci-core/src/lib.rs` (khai báo module feature-gated)
- Create: `crates/ci-core/src/scip/mod.rs`
- Create: `crates/ci-core/src/scip/parse.rs`

- [ ] **Step 1: Vet + thêm dep** — trước khi thêm, kiểm license/size: `cargo tree -p scip` sau khi thêm; `scip` 0.9 là Apache-2.0, kéo `protobuf`. Nếu size/license không chấp nhận được → dừng, báo lại (fallback: parse subset protobuf bằng `prost` tự sinh — quyết định riêng). Thêm vào `crates/ci-core/Cargo.toml`:
```toml
# (trong [dependencies], optional)
scip = { version = "0.9", optional = true }
```
```toml
# (trong [features])
scip-overlay = ["dep:scip"]
```
> **KHÔNG** thêm `scip-overlay` vào `default`. Off by default (Invariant 3).

- [ ] **Step 2: Khai báo module** — trong `crates/ci-core/src/lib.rs`:
```rust
#[cfg(feature = "scip-overlay")]
pub mod scip;
```

- [ ] **Step 3: Viết test đỏ** — `crates/ci-core/src/scip/parse.rs` (`#[cfg(test)]`). Test dùng một `.scip` nhỏ đã sinh sẵn từ fixture (sinh một lần bằng `rust-analyzer scip` và commit vào `tests/fixtures/`), hoặc build Index bằng chính crate `scip` trong test. Dùng cách thứ hai (không phụ thuộc rust-analyzer lúc test):
```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_definition_and_reference_occurrences() {
        // Minimal hand-built SCIP index: one doc, one def + one ref.
        let mut index = scip::types::Index::new();
        let mut doc = scip::types::Document::new();
        doc.relative_path = "core/src/engine.rs".into();
        let mut def = scip::types::Occurrence::new();
        def.range = vec![2, 4, 2, 7]; // line 2 (0-based), cols
        def.symbol = "rust-analyzer cargo demo-core 0.1.0 engine/impl#[Engine]start().".into();
        def.symbol_roles = scip::types::SymbolRole::Definition as i32;
        let mut rf = scip::types::Occurrence::new();
        rf.range = vec![5, 8, 5, 13];
        rf.symbol = def.symbol.clone();
        doc.occurrences = vec![def, rf];
        index.documents = vec![doc];

        let occ = parse_index(&index);
        assert_eq!(occ.len(), 2);
        let def = occ.iter().find(|o| o.is_def).unwrap();
        assert_eq!(def.file, "core/src/engine.rs");
        assert_eq!(def.line, 3); // 1-based
        assert_eq!(def.symbol, "rust-analyzer cargo demo-core 0.1.0 engine/impl#[Engine]start().");
    }
}
```

- [ ] **Step 4: Chạy — xác nhận FAIL** `cargo test -p ci-core --features scip-overlay --lib scip::parse` → expected: FAIL (chưa có `parse_index`)
- [ ] **Step 5: Implement parse** — `crates/ci-core/src/scip/parse.rs`:
```rust
//! Decode a SCIP `Index` into flat occurrences the ingester can match against
//! `ci`'s call sites and symbols. We keep only file/line/symbol/role — SCIP's
//! rich moniker string is preserved verbatim as the identity key.

/// One SCIP occurrence, normalized to 1-based line and `ci`'s conventions.
#[derive(Debug, Clone)]
pub struct ScipOccurrence {
    pub file: String,
    /// 1-based line of the occurrence start.
    pub line: usize,
    /// SCIP symbol moniker (opaque identity string).
    pub symbol: String,
    pub is_def: bool,
    /// True for `local N` monikers (function-scoped, not cross-file useful).
    pub is_local: bool,
}

pub fn parse_index(index: &scip::types::Index) -> Vec<ScipOccurrence> {
    let mut out = Vec::new();
    for doc in &index.documents {
        for occ in &doc.occurrences {
            // SCIP range is [startLine, startChar, endLine, endChar] (0-based) or
            // [startLine, startChar, endChar] when single-line.
            let Some(&start_line) = occ.range.first() else {
                continue;
            };
            let is_def = occ.symbol_roles & (scip::types::SymbolRole::Definition as i32) != 0;
            out.push(ScipOccurrence {
                file: doc.relative_path.clone(),
                line: (start_line as usize) + 1,
                symbol: occ.symbol.clone(),
                is_def,
                is_local: occ.symbol.starts_with("local "),
            });
        }
    }
    out
}

#[cfg(feature = "scip-overlay")]
pub fn parse_scip_file(path: &std::path::Path) -> anyhow::Result<Vec<ScipOccurrence>> {
    let bytes = std::fs::read(path)?;
    let index = scip::types::Index::parse_from_bytes(&bytes)?;
    Ok(parse_index(&index))
}
```
> **Executor verify:** API chính xác của crate `scip` 0.9 (`scip::types::Index`,
> `parse_from_bytes`, `SymbolRole::Definition`) — chạy `cargo doc -p scip --open` hoặc đọc
> crate source nếu tên khác. Test Step 3 build Index in-memory nên khớp API thật; điều chỉnh
> theo compiler.

- [ ] **Step 6: Chạy — xác nhận PASS** `cargo test -p ci-core --features scip-overlay --lib scip::parse` → PASS
- [ ] **Step 7: Xác nhận build mặc định KHÔNG kéo scip** `cargo build -p ci-core` (không `--features`) → build sạch, `cargo tree -p ci-core | grep -c scip` = 0. Commit: `cargo fmt --all && git commit -am "feat(scip): SCIP index parse module, feature-gated (Phase B1)"`

---

### Task B2: rust-analyzer detection + batch runner

**Files:**
- Create: `crates/ci-core/src/scip/runner.rs`
- Modify: `crates/ci-core/src/scip/mod.rs` (khai báo submodule)

- [ ] **Step 1: Viết test đỏ** — `runner.rs` (`#[cfg(test)]`). Test detection thuần (không spawn):
```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detect_returns_none_when_binary_absent() {
        // A guaranteed-absent binary name yields None (fail-silent, no panic).
        assert!(resolve_binary(Some("definitely-not-a-real-ra-binary-xyz")).is_none());
    }
}
```

- [ ] **Step 2: Chạy — xác nhận FAIL** `cargo test -p ci-core --features scip-overlay --lib scip::runner` → FAIL
- [ ] **Step 3: Implement runner** — `crates/ci-core/src/scip/runner.rs`:
```rust
//! Detect a `rust-analyzer` binary and drive its batch `scip` subcommand.
//! Detect-once, fail-silent (ADR-0004 §2): any failure returns None/Err and the
//! caller keeps the syntactic graph untouched.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

/// Total wall-clock budget for one `rust-analyzer scip` pass. Measured cost on
/// the `ci` workspace itself (~44k occurrences) was 21.5s; ripgrep 20s. 120s
/// leaves generous headroom; overrun → kill and keep whatever the syntactic
/// tier already produced.
pub const SCIP_TIMEOUT: Duration = Duration::from_secs(120);

/// Resolve a usable rust-analyzer binary path. Tries, in order: an explicit
/// override, `PATH`, the rustup component, and the VS Code extension bundle.
pub fn resolve_binary(override_bin: Option<&str>) -> Option<PathBuf> {
    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Some(b) = override_bin {
        candidates.push(PathBuf::from(b));
    }
    candidates.push(PathBuf::from("rust-analyzer")); // PATH lookup via which-style probe
    if let Some(home) = dirs_home() {
        candidates.push(home.join(".rustup/toolchains")); // marker; expanded below
        // VS Code extension server dir (glob newest).
        if let Some(p) = newest_vscode_ra(&home) {
            candidates.push(p);
        }
    }
    candidates.into_iter().find(|c| binary_runs(c))
}

/// Run `<bin> scip <root> --output <out>` under the time budget. Returns the
/// output path on success. Never propagates a panic; a non-zero exit or timeout
/// is an `Err` the caller swallows.
pub fn run_scip(bin: &Path, root: &Path, out: &Path) -> anyhow::Result<()> {
    let mut child = Command::new(bin)
        .arg("scip")
        .arg(root)
        .arg("--output")
        .arg(out)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()?;
    // Poll with a deadline; kill on overrun (Command has no built-in timeout —
    // same pattern as analysis/diff_impact.rs's bounded wait).
    let deadline = std::time::Instant::now() + SCIP_TIMEOUT;
    loop {
        if let Some(status) = child.try_wait()? {
            if status.success() {
                return Ok(());
            }
            anyhow::bail!("rust-analyzer scip exited with {status}");
        }
        if std::time::Instant::now() >= deadline {
            let _ = child.kill();
            anyhow::bail!("rust-analyzer scip exceeded {}s budget", SCIP_TIMEOUT.as_secs());
        }
        std::thread::sleep(Duration::from_millis(200));
    }
}

fn binary_runs(path: &Path) -> bool {
    Command::new(path)
        .arg("--version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

fn dirs_home() -> Option<PathBuf> {
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
```
> **Executor:** `dirs_home` + `.rustup/toolchains` marker ở trên là placeholder cho probe
> rustup — nếu `rust-analyzer` không trên PATH nhưng rustup có, dùng `rustup which
> rust-analyzer`. Đơn giản hoá: bỏ dòng `.rustup/toolchains` marker, thêm nhánh chạy
> `rustup which --toolchain stable rust-analyzer` và push kết quả. Giữ `binary_runs` làm
> cổng cuối. Test Step 1 chỉ chốt hành vi fail-silent.

- [ ] **Step 4: Chạy — xác nhận PASS** `cargo test -p ci-core --features scip-overlay --lib scip::runner` → PASS
- [ ] **Step 5: fmt + clippy + commit** `cargo fmt --all && cargo clippy -p ci-core --features scip-overlay --all-targets -- -D warnings && git commit -am "feat(scip): rust-analyzer detection + batch runner with timeout (Phase B2)"`

---

### Task B3: Config surface (RustConfig / ScipConfig)

**Files:**
- Modify: `crates/ci-core/src/config.rs` — thêm `rust: RustConfig`
- Test: `crates/ci-core/src/config.rs` (`#[cfg(test)]`)

- [ ] **Step 1: Viết test đỏ**:
```rust
#[test]
fn rust_scip_defaults_off() {
    let c = Config::default();
    assert!(!c.rust.scip.enabled, "SCIP overlay must be off by default");
}

#[test]
fn rust_scip_opt_in_parses() {
    let json = r#"{"rust":{"scip":{"enabled":true}}}"#;
    let c: Config = serde_json::from_str(json).unwrap();
    assert!(c.rust.scip.enabled);
}
```

- [ ] **Step 2: Chạy — xác nhận FAIL** `cargo test -p ci-core --lib config::tests::rust_scip_defaults_off` → FAIL
- [ ] **Step 3: Thêm struct + field** — trong `config.rs`, thêm field vào `Config` (sau `cochange`), vào `Config::default()`, và định nghĩa struct:
```rust
// field trong struct Config:
    pub rust: RustConfig,
```
```rust
// trong Config::default(), thêm:
            rust: RustConfig::default(),
```
```rust
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(default)]
pub struct RustConfig {
    pub scip: ScipConfig,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct ScipConfig {
    /// Off by default. When true and `rust-analyzer` is detectable, the batch
    /// SCIP overlay upgrades Rust call edges to `formal` confidence after the
    /// syntactic index reaches `ready`.
    pub enabled: bool,
    /// Optional explicit rust-analyzer binary path (else auto-detect).
    pub binary: Option<String>,
}

impl Default for ScipConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            binary: None,
        }
    }
}
```

- [ ] **Step 4: Chạy — xác nhận PASS** `cargo test -p ci-core --lib config` → PASS
- [ ] **Step 5: fmt + clippy + commit** `cargo fmt --all && cargo clippy --all-targets -- -D warnings && git commit -am "feat(config): rust.scip overlay config, off by default (Phase B3)"`

---

### Task B4: Ingest — upgrade edges to `Formal` (additive-only)

Trái tim của Phase B và **task rủi ro cao nhất**. Đối chiếu SCIP occurrence với `call_edges`
đã tồn tại; chỉ nâng edge lên `formal` khi SCIP xác nhận cùng call site resolve tới cùng
definition. Không INSERT, không DELETE, chỉ UPDATE `edge_confidence` từ giá trị thấp hơn lên
`formal`.

**Chiến lược đối chiếu (v1, thận trọng):**
- Từ SCIP defs: map `symbol_moniker → (file, line)` của definition.
- Từ SCIP refs (không local): tại `(file, ref_line)`, biết ref này trỏ tới `symbol_moniker`
  → tra def của moniker → `(def_file, def_line)`.
- `ci` có `call_sites(from_path, call_line, callee_name, ...)` và sau rebuild có
  `call_edges(from_symbol, to_symbol, from_path, to_path, call_site_line, edge_confidence)`.
- Với mỗi call_edge rank < Formal: nếu tồn tại SCIP ref tại `(from_path, call_site_line)` mà
  def của nó rơi vào cùng file+dòng của `to_symbol` (tra `symbols` theo qualified_name →
  path+line_start), thì UPDATE edge → `formal`.
- **Không match được → giữ nguyên.** Timeout/RA absent → không chạy, graph nguyên vẹn.

**Files:**
- Create: `crates/ci-core/src/scip/ingest.rs`
- Test: `crates/ci-core/src/scip/ingest.rs` (`#[cfg(test)]`)

- [ ] **Step 1: Viết test đỏ** — ingest thuần trên DB dựng tay + occurrences dựng tay (không cần rust-analyzer):
```rust
#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;

    fn db_with_one_textual_edge() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        crate::db::schema::init_db(&conn).unwrap();
        conn.execute_batch(
            "INSERT INTO symbols (qualified_name, name, kind, language, path, line_start, line_end)
             VALUES ('core/src/engine.rs::Engine::start','start','method','rust','core/src/engine.rs',6,8);
             INSERT INTO call_edges (from_symbol, to_symbol, call_site_line, edge_confidence, from_path, to_path)
             VALUES ('app/src/main.rs::main','core/src/engine.rs::Engine::start',5,'textual','app/src/main.rs','core/src/engine.rs');",
        )
        .unwrap();
        conn
    }

    #[test]
    fn upgrades_matching_edge_to_formal() {
        let conn = db_with_one_textual_edge();
        let occ = vec![
            // def of start() at engine.rs line 6
            ScipOccurrence { file: "core/src/engine.rs".into(), line: 6,
                symbol: "M".into(), is_def: true, is_local: false },
            // ref at the call site (main.rs line 5) pointing to the same moniker
            ScipOccurrence { file: "app/src/main.rs".into(), line: 5,
                symbol: "M".into(), is_def: false, is_local: false },
        ];
        let n = ingest_occurrences(&conn, &occ).unwrap();
        assert_eq!(n, 1);
        let conf: String = conn
            .query_row("SELECT edge_confidence FROM call_edges", [], |r| r.get(0))
            .unwrap();
        assert_eq!(conf, "formal");
    }

    #[test]
    fn never_downgrades_or_inserts() {
        let conn = db_with_one_textual_edge();
        conn.execute("UPDATE call_edges SET edge_confidence = 'resolved'", []).unwrap();
        // Occurrences that match nothing must leave the edge and count untouched.
        let occ = vec![ScipOccurrence { file: "zzz.rs".into(), line: 99,
            symbol: "X".into(), is_def: false, is_local: false }];
        let n = ingest_occurrences(&conn, &occ).unwrap();
        assert_eq!(n, 0);
        let (conf, cnt): (String, i64) = conn
            .query_row("SELECT edge_confidence, (SELECT COUNT(*) FROM call_edges) FROM call_edges",
                [], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap();
        assert_eq!(conf, "resolved");
        assert_eq!(cnt, 1);
    }
}
```

- [ ] **Step 2: Chạy — xác nhận FAIL** `cargo test -p ci-core --features scip-overlay --lib scip::ingest` → FAIL
- [ ] **Step 3: Implement ingest** — `crates/ci-core/src/scip/ingest.rs`:
```rust
//! Upgrade existing Rust call edges to `formal` confidence using SCIP evidence.
//! ADDITIVE ONLY: never inserts, deletes, or downgrades an edge (ADR-0004 §3).

use std::collections::HashMap;

use rusqlite::Connection;

use super::parse::ScipOccurrence;

/// Match SCIP occurrences against existing call edges and upgrade the confidence
/// of each corroborated edge to `formal`. Returns the number of edges upgraded.
///
/// Matching (conservative): a call edge `(from_path, call_site_line) -> to_symbol`
/// is corroborated when there is a non-local SCIP reference at
/// `(from_path, call_site_line)` whose definition occurrence lands on the same
/// file+line as `to_symbol`'s declaration.
pub fn ingest_occurrences(conn: &Connection, occ: &[ScipOccurrence]) -> rusqlite::Result<usize> {
    // moniker -> (def_file, def_line)
    let mut def_of: HashMap<&str, (&str, usize)> = HashMap::new();
    for o in occ {
        if o.is_def && !o.is_local {
            def_of.insert(o.symbol.as_str(), (o.file.as_str(), o.line));
        }
    }
    // (ref_file, ref_line) -> set of def sites it points to
    let mut ref_targets: HashMap<(&str, usize), Vec<(&str, usize)>> = HashMap::new();
    for o in occ {
        if !o.is_def && !o.is_local
            && let Some(&def) = def_of.get(o.symbol.as_str())
        {
            ref_targets
                .entry((o.file.as_str(), o.line))
                .or_default()
                .push(def);
        }
    }

    // Load candidate edges (rank below formal) joined to their target's decl site.
    let rows: Vec<(i64, String, i64, String, i64)> = {
        let mut stmt = conn.prepare(
            "SELECT ce.id, ce.from_path, ce.call_site_line, s.path, s.line_start \
             FROM call_edges ce \
             JOIN symbols s ON s.qualified_name = ce.to_symbol \
             WHERE ce.edge_confidence != 'formal' \
               AND ce.call_site_line IS NOT NULL \
               AND ce.from_path IS NOT NULL",
        )?;
        stmt.query_map([], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, i64>(2)?,
                r.get::<_, String>(3)?,
                r.get::<_, i64>(4)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?
    };

    let mut to_upgrade: Vec<i64> = Vec::new();
    for (id, from_path, call_line, def_path, def_line) in &rows {
        let key = (from_path.as_str(), *call_line as usize);
        if let Some(targets) = ref_targets.get(&key)
            && targets
                .iter()
                .any(|(f, l)| *f == def_path.as_str() && *l == *def_line as usize)
        {
            to_upgrade.push(*id);
        }
    }

    let mut stmt =
        conn.prepare("UPDATE call_edges SET edge_confidence = 'formal' WHERE id = ?1")?;
    for id in &to_upgrade {
        stmt.execute([id])?;
    }
    Ok(to_upgrade.len())
}
```
> Cần khai báo `pub mod ingest;` và `pub mod parse;` + `pub mod runner;` trong `scip/mod.rs`.

- [ ] **Step 4: Chạy — xác nhận PASS** `cargo test -p ci-core --features scip-overlay --lib scip::ingest` → PASS (cả `upgrades_matching_edge_to_formal` và `never_downgrades_or_inserts`)
- [ ] **Step 5: fmt + clippy + commit** `cargo fmt --all && cargo clippy -p ci-core --features scip-overlay --all-targets -- -D warnings && git commit -am "feat(scip): additive-only edge upgrade to formal (Phase B4)"`

---

### Task B5: Orchestration + cache + wire vào server

Gắn 4 mảnh (detect → run → parse → ingest) thành một entrypoint, cache theo
`(RA version, Cargo.lock hash, dirty file set)`, chạy nền sau `phase=ready`.

**Files:**
- Create: `crates/ci-core/src/scip/cache.rs`
- Modify: `crates/ci-core/src/scip/mod.rs` — hàm `run_overlay(conn, root, config) -> Result<usize>`
- Modify: `crates/ci-server/src/lib.rs` — gọi overlay sau khi index xong (feature-gated)
- Test: `crates/ci-core/src/scip/mod.rs` (integration nhẹ, gated + `#[ignore]` nếu cần RA)

- [ ] **Step 1: Viết test đỏ** — cache key ổn định:
```rust
// trong scip/cache.rs #[cfg(test)]
#[test]
fn cache_key_changes_with_lockfile() {
    let a = overlay_cache_key("1.96.0", "hashAAA", &["src/x.rs".into()]);
    let b = overlay_cache_key("1.96.0", "hashBBB", &["src/x.rs".into()]);
    assert_ne!(a, b);
}
```

- [ ] **Step 2: Chạy — xác nhận FAIL** `cargo test -p ci-core --features scip-overlay --lib scip::cache` → FAIL
- [ ] **Step 3: Implement cache + orchestrator**

`crates/ci-core/src/scip/cache.rs`:
```rust
//! Stable cache key for a SCIP overlay pass, so we skip re-running rust-analyzer
//! when nothing that affects the index changed.

/// FNV-1a over (RA version, Cargo.lock hash, sorted dirty files). Reuses the
/// same stable hash as the indexer's file hashing (pipeline::hash_content).
pub fn overlay_cache_key(ra_version: &str, lockfile_hash: &str, dirty: &[String]) -> String {
    let mut sorted: Vec<&str> = dirty.iter().map(String::as_str).collect();
    sorted.sort_unstable();
    let material = format!("{ra_version}|{lockfile_hash}|{}", sorted.join(","));
    crate::indexer::pipeline::hash_content(&material)
}
```

`crates/ci-core/src/scip/mod.rs` (orchestrator):
```rust
pub mod cache;
pub mod ingest;
pub mod parse;
pub mod runner;

use std::path::Path;

use rusqlite::Connection;

use crate::config::RustConfig;

/// Run the full SCIP overlay: detect rust-analyzer, run batch scip into a temp
/// file, parse, and upgrade edges. Fail-silent — every failure mode (disabled,
/// no binary, timeout, parse error) returns `Ok(0)` after logging once, leaving
/// the syntactic graph untouched. Returns the number of edges upgraded.
pub fn run_overlay(conn: &Connection, root: &Path, rust: &RustConfig) -> anyhow::Result<usize> {
    if !rust.scip.enabled {
        return Ok(0);
    }
    let Some(bin) = runner::resolve_binary(rust.scip.binary.as_deref()) else {
        tracing::info!("SCIP overlay enabled but no rust-analyzer found — skipping");
        return Ok(0);
    };
    let tmp = tempfile::Builder::new().suffix(".scip").tempfile()?;
    if let Err(e) = runner::run_scip(&bin, root, tmp.path()) {
        tracing::warn!("SCIP overlay run failed, keeping syntactic graph: {e}");
        return Ok(0);
    }
    let occ = match parse::parse_scip_file(tmp.path()) {
        Ok(o) => o,
        Err(e) => {
            tracing::warn!("SCIP parse failed: {e}");
            return Ok(0);
        }
    };
    let upgraded = ingest::ingest_occurrences(conn, &occ)?;
    tracing::info!("SCIP overlay upgraded {upgraded} Rust edges to formal");
    Ok(upgraded)
}
```
> `tempfile` đã là dev-dependency (`Cargo.toml:38`) — chuyển sang `[dependencies]` (nó nhỏ,
> nhưng để giữ Invariant 3 "zero new dep cho phần luôn-build", đặt nó optional dưới feature
> `scip-overlay`: `tempfile = { version = "3", optional = true }` và thêm vào
> `scip-overlay = ["dep:scip", "dep:tempfile"]`; giữ dòng dev-dependency cho test khác).

- [ ] **Step 4: Wire vào server** — trong `crates/ci-server/src/lib.rs`, sau khi indexer đặt `phase=Ready` trong `spawn_blocking` (khối hiện tại là dòng 53-113; điểm chèn đúng là ngay sau khối `if index_ok { ... }` ở dòng 99-105, cạnh lệnh `bootstrap_embeddings(&conn, ...)` đã có sẵn ở dòng 107-109, trước khi gọi `watcher::run_watch_loop` ở dòng 112), thêm (feature-gated):
```rust
            #[cfg(feature = "scip-overlay")]
            if index_ok {
                let rust_cfg = ci_core::config::load_config(&indexer_root)
                    .map(|c| c.rust)
                    .unwrap_or_default();
                match ci_core::scip::run_overlay(&conn, &indexer_root, &rust_cfg) {
                    Ok(n) if n > 0 => tracing::info!("SCIP overlay: {n} edges upgraded"),
                    Ok(_) => {}
                    Err(e) => tracing::warn!("SCIP overlay error (base graph intact): {e}"),
                }
            }
```
> `ci-server/Cargo.toml` cần feature passthrough: `scip-overlay = ["ci-core/scip-overlay"]`.
> Overlay chạy **sau** khi phase đã Ready → agent dùng được graph ngay, edge được nâng dần.
> Cache (Step 3 `overlay_cache_key`) dùng ở đây để bỏ qua nếu key trùng lần chạy trước (lưu
> key vào `project_memory` hoặc file `.codeindex/scip.cache` — executor chọn, ưu tiên file
> đơn giản).

- [ ] **Step 5: Chạy — xác nhận PASS + không regression**
  - `cargo test -p ci-core --features scip-overlay --lib scip` → PASS
  - `cargo build` (default, không feature) → sạch; `cargo build --features ci-core/scip-overlay` từ workspace → sạch
  - **Regression gate:** `cargo test` toàn workspace ở cấu hình mặc định → xanh (Phase B off không đổi gì)
- [ ] **Step 6: fmt + clippy + commit** `cargo fmt --all && cargo clippy --workspace --all-targets -- -D warnings && cargo clippy -p ci-core --features scip-overlay --all-targets -- -D warnings && git commit -am "feat(scip): overlay orchestration + cache + server wiring (Phase B5)"`

---

### Task B6: Benchmark — SCIP as precision/recall oracle (lấp slot B2 có sẵn)

Biến "Rust tốt chưa" thành số: so call edges của Tầng A với SCIP ground truth trên corpus thật.

`benchmarks/README.md` đã có sẵn dòng **"B2 | Call Graph Resolution Quality | Tier-1/2/3 vs
textual | Planned"** — đây chính là lần triển khai đầu tiên của B2 (scope: Rust, dùng
rust-analyzer làm oracle). Đặt tên/vị trí theo đúng convention `bN_ten/run_benchmark.py` +
tái dùng `benchmarks/lib/` (`mcp_client.py`) mà `b3_search_quality`/`b4_token_efficiency`/
`b6_tool_call_efficiency` đã dùng — không tạo thư mục top-level riêng lệch chuẩn như
`rust_precision/run.py`.

**Files:**
- Create: `benchmarks/b2_call_graph_quality/run_benchmark.py`
- Create: `benchmarks/b2_call_graph_quality/README.md`
- Modify: `benchmarks/README.md` — đổi dòng B2 từ `Planned` → `Implemented` (ghi rõ scope
  hiện tại: Rust only) kèm link tới `b2_call_graph_quality/`, theo đúng cách B3/B4/B6 đã làm.

- [ ] **Step 1: Viết harness** `benchmarks/b2_call_graph_quality/run_benchmark.py` — pseudo-flow (executor hoàn thiện theo `benchmarks/lib/mcp_client.py` sẵn có, cùng cách b3/b4/b6 đã dùng):
  1. Nhận `--repo <path>` (mặc định: chính `ci`).
  2. Chạy `rust-analyzer scip <repo> --output oracle.scip`; decode bằng `scip print --json` (hoặc crate) → tập cạnh `(caller_file:line → callee_def_file:line)` cho các ref không-local.
  3. Chạy `ci index <repo>`; đọc `.codeindex/index.db` `call_edges` (Rust) → tập cạnh tương ứng qua `symbols.line_start`.
  4. Tính precision = |ci ∩ oracle| / |ci|, recall = |ci ∩ oracle| / |oracle|, phân tách theo `edge_confidence`.
  5. In bảng; ghi JSON để track qua thời gian.
- [ ] **Step 2: Chạy baseline** `python benchmarks/b2_call_graph_quality/run_benchmark.py --repo .` → ghi lại số precision/recall hiện tại (sau Phase A). Đây là mốc để mọi thay đổi Rust về sau đối chiếu.
- [ ] **Step 3: Cập nhật `benchmarks/README.md`** — đổi dòng B2 sang `Implemented` kèm link tới `b2_call_graph_quality/`.
- [ ] **Step 4: Commit** `git commit -am "bench(rust): B2 call-graph precision/recall harness with SCIP oracle"`

---

## Mốc hoàn thành & thứ tự ưu tiên

- **Sau Phase A** (Task 0 → A5): Rust syntactic top-tier, ship độc lập, zero dep mới, robust
  tuyệt đối trên code hỏng. Đây là phần bắt buộc, giá trị cao nhất/rủi ro thấp nhất — làm trước.
- **Sau Phase B** (B1 → B6): SCIP overlay opt-in cho ai bật, nâng edge lên `formal`, và cho
  oracle benchmark. Off by default nên không ảnh hưởng ai chưa bật.

**Việc kèm sau khi merge** (không thuộc plan code này, nhưng cần làm):
- Cập nhật ADR-0004 với finding batch-SCIP: với Rust (và ngôn ngữ có SCIP indexer trưởng
  thành), **batch SCIP đi trước live-LSP** trong thứ tự cân nhắc — đơn giản hơn pilot gopls ở
  mọi trục vận hành. Ghi rõ đây là biến thể transport của cùng 6 nguyên tắc ADR-0004, không
  phải quyết định mới.
- Cập nhật `docs/comparison.md` nếu có mục so accuracy theo ngôn ngữ.

## Known limitations (dán nhãn trung thực, không giấu)

- A3 `super::` xấp xỉ theo thư mục — sai với module kiểu `foo/mod.rs`; miss → NULL (không bao
  giờ sai edge), Phase B phủ phần dư.
- A3 không parse `#[path = "..."]` và `mod` ở vị trí phi quy ước — hiếm, Phase B phủ.
- A5 chỉ suy constructor cho `let x = Foo::new()`/`Foo::default()`/`Foo{..}` — không cho method
  chaining hay biểu thức phức tạp; những cái đó ở `textual` cho tới khi Phase B nâng.
- Phase B cần `cargo metadata` load được (deps resolve) — repo Bazel/Buck cần
  `rust-project.json`; không có thì Tầng A vẫn nguyên vẹn.
