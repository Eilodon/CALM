# multi_lang_workspace

Static ground-truth fixture for the 8-language Formal-tier plan
(`docs/superskills/plans/2026-07-07-eight-lang-formal-tier.md`, P0.5). Not a
buildable project — no build actually runs against most of these directories
today (only `rust_workspace`, a separate fixture, is driven through a real
`rust-analyzer` run). Each subdirectory is a minimal, hand-written mini
project exercising the "standard gap" a given language's Phase 1 heuristic
or Phase 2 SCIP provider needs to close, per the plan's own language-by-
language notes:

- `go/` — `main.go` calls `Greet` (defined in `helper.go`) with no import:
  same-package resolution (P1.3 Tier-1.5 same-dir preference; P2.1 scip-go).
  Single-module only — the sibling `../go_workspace/` fixture (own README)
  covers the `go.work` multi-module case (P2.1 V2), kept separate rather
  than added as a subdirectory here since it's a different scenario
  (module enumeration + `sub_root` rebasing), not another "standard gap"
  in the same per-language sense as the rest of this fixture set.
- `java/` — `Main` calls `Helper.greet` statically, same package, no import
  (P1.3; P2.2 scip-java).
- `csharp/` — `Program` calls `Helper.Greet` via `using MultiLang;` (P1.5
  namespace→file resolution; P2.3 scip-dotnet).
- `c/` — `main.c` calls `greet` declared in `helper.h` / defined in
  `helper.c`, with a minimal `compile_commands.json` (P1.4; P3.1 scip-clang).
- `cpp/` — `main.cpp` calls `Circle::area()` through a `Shape&` reference —
  a virtual dispatch call site (P1.4; P3.1 scip-clang).
- `js/` — `main.js` requires `helper.js` and calls `greet` from a wrapping
  `run()` function (CommonJS; P1.1 stack-graphs JS; P3.2 scip-typescript).
  `run()` is required, not incidental: a bare top-level `greet("world")`
  call produces no `symbols`/`call_edges` row at all (top-level statements
  aren't attributed to an enclosing function) — confirmed empirically by
  running `calm index` against the original bare-call version before this
  wrapper was added, which yielded 1 symbol and 0 call edges.
- `python/` — `main.py` does `from pkg.helper import helper` and calls it
  from a wrapping `run()` function, mirroring `go/`'s and `js/`'s
  cross-file-call shape (P2.4 scip-python). Added alongside P3.2 to close a
  gap: P2.4 shipped without any fixture committed here, only an ad-hoc
  uncommitted one used for its manual verification.
- `php/` — `index.php` does `require_once` then `$helper->greet(...)` on a
  PSR-4-autoloadable class (P1.2 PHP heuristics; P2.5 scip-php).
- `sql/` — `schema.sql`: `CREATE TABLE users`, a `CREATE VIEW` referencing
  it, and one stored procedure `CALL`-ing another (P3.3 SQL module).

`go/`, `python/`, and `js/` are wired into `#[ignore]`d integration tests in
`crates/calm-core/src/scip/mod.rs` (`go_overlay_upgrades_a_real_edge_on_the_multi_lang_fixture`,
`python_overlay_upgrades_a_real_edge_on_the_multi_lang_fixture`,
`js_overlay_upgrades_a_real_edge_on_the_multi_lang_fixture`) — each asserts
the real external indexer (`scip-go`/`scip-python`/`scip-typescript`)
upgrades the fixture's one cross-file call edge to `formal`. The remaining
subdirectories (`java/`, `csharp/`, `c/`, `cpp/`, `php/`, `sql/`) are still
unwired — Phase 2/3 work for those languages should add its own `#[ignore]`d
integration test(s) pointing at the relevant subdirectory here as it lands,
so the nightly CI job (`.github/workflows/scip-nightly.yml`) picks them up
automatically via `cargo test --workspace -- --ignored` without any
workflow-file change.
