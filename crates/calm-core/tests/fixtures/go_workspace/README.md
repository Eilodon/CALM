# go_workspace

Static fixture for the Go `go.work` multi-module case (P2.1 V2,
`docs/superskills/plans/2026-07-07-eight-lang-formal-tier.md`). Separate from
`../multi_lang_workspace/go/` (single-module) on purpose — this exercises
module *enumeration* and per-module `sub_root` path rebasing, not another
per-language "standard gap" in the same sense as that fixture set.

Two independent modules (`moda/`, `modb/`), deliberately **not**
cross-importing each other — each is a self-contained same-package call
(`main.go` → `helper.go`'s `Greet`, same shape as the single-module fixture),
so the live test can assert each module's edge independently. A broken
`sub_root` rebase (e.g. both modules accidentally sharing one prefix) would
land an edge under the wrong path and fail one of the two assertions even if
`scip-go` itself ran fine on both — cross-module import resolution is
`scip-go`'s own responsibility, not something this fixture needs to exercise
to prove CALM's enumeration/rebasing logic works.

Wired into `#[ignore]`d integration test
`go_workspace_overlay_upgrades_edges_in_both_member_modules` in
`crates/calm-core/src/scip/mod.rs` — same reason and same `scip-go` install
path as `multi_lang_workspace/go`'s sibling test. **Not yet run against a
real `scip-go` binary in any environment** (none was available while writing
it) — treat a first real run's result with appropriately less confidence
than the already-proven single-module path until it has been; if it exposes
a genuine Go-workspace-mode build quirk (e.g. how `go.work`'s local module
resolution interacts with `scip-go index --module-root`), that's the moment
to find out, not assumed away here.
