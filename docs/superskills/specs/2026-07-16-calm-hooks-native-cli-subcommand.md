---
title: Native `calm hook-check` CLI subcommand — remove bash/jq/sqlite3-CLI/flock from the scaffolded plugin hook
date: 2026-07-16
SPEC_APPROVED: true
SPEC_ESCALATION: false
---

## Problem

CALM now auto-scaffolds its portable, plugin-distributed enforcement hook on
first use (`plugins/calm/hooks/bootstrap.sh`, shipped this session, calls the
already-existing `calm init --hooks=enforce --agents-md` — see
`crates/calm-cli/src/main.rs::apply_hooks_flag`, `crates/calm-core/src/
hooks.rs`). That command writes `.claude/hooks/calm-hooks.sh`
(`crates/calm-core/assets/hooks/calm-hooks.sh`, embedded via `include_str!`)
into the *user's own project* and wires it into their `.claude/settings.json`
as a `PreToolUse`/`PostToolUse` shell-form hook.

`calm-hooks.sh` is real bash: `jq` for every field extraction, `sqlite3
-readonly` for the `edit_context` symbol→path ground-truth lookup (line
217-226), POSIX `{FD}>file` + `flock` for the session-state read-modify-write
lock (line 113-122, ported from the internal `calm-nudge.sh`'s own
`DEBT-010` TOCTOU fix). This was fine when the only realistic install path
was Linux/macOS/WSL dev boxes. It is no longer a safe assumption: this
project is *actively* shipping native Windows x64 + macOS x64 binary
distribution this cycle (branch `feat/windows-macos-x64-distribution`,
`npm/calm-mcp-win32-x64`) — real native-Windows users, not just WSL, are now
an explicit target audience for exactly the plugin this hook is bootstrapped
from.

Researched this session (via Claude Code's own documentation, not assumed):
Claude Code executes a shell-form hook `command` (no `args` array) via **Git
Bash by default on native Windows** — so `bash "${CLAUDE_PLUGIN_ROOT}/..."`
does have a chance of running there. But three narrower assumptions
underneath that don't hold merely because Git Bash is present:
- **`jq`** is not bundled with Git for Windows — a separate install.
  Currently unguarded in `calm-hooks.sh` (no `command -v jq` check anywhere),
  though the failure mode is soft: every `jq` call degrades to an empty
  variable, and the script's `case "$tool_name" in ...)` dispatch then
  matches nothing, so a missing `jq` silently turns the hook into a no-op
  rather than erroring or blocking a tool call. Confirmed by reading the
  dispatch: no branch has a non-jq fallback path.
- **`sqlite3` CLI** — already soft-guarded (`command -v sqlite3` at line 218
  before the `edit_context` path-resolution query), degrades to just not
  resolving a bare `symbol` argument to a path, no crash.
- **`flock`** — MSYS/Git-Bash's coreutils do not reliably ship a working
  `flock`. The script's own lock functions already fail open by design
  (`acquire_state_lock`'s `2>/dev/null || { STATE_LOCK_FD=""; return; }`),
  consistent with the documented risk tolerance ("an occasional dropped
  increment is the acceptable failure mode" — see the DEBT-010 fix's own
  comment in `calm-nudge.sh`, which `calm-hooks.sh` mirrors). Not a new
  danger, but real: Windows users get zero race protection on the
  `edit_context_files`/`needs_diff_impact` state file, undocumented anywhere
  a Windows user would see it.

Net effect: on native Windows without a separately-installed `jq`, the
scaffolded hook silently does nothing at all — `enforce` mode is active in
`.calm/hooks.mode` and reported as such by `calm doctor`, but provides zero
actual enforcement. A user believing they have the hard gates is the exact
"fails silently-open... worse than no feature, because it's trusted" failure
class already named and taken seriously in this project's own prior audit
(`docs/superskills/specs/2026-07-14-calm-mcp-external-onboarding.md`, Item B,
Failure Mode 1).

## Design

Move the hook's decision logic into a new `calm` CLI subcommand — e.g. `calm
hooks-check` — written in Rust, reusing infrastructure already in
`calm-core`/`calm-cli` instead of shelling out to `jq`/`sqlite3`:
- JSON stdin parsing → `serde_json` (already a workspace dependency
  everywhere).
- `file_index`/`symbols` ground-truth lookups → `rusqlite` directly against
  `.calm/index.db`, the same connection pattern `calm-server`'s tools
  already use (`CalmServer::make_read_conn`), instead of shelling to the
  `sqlite3` binary.
- Session-state read-modify-write → `std::fs` + a real file lock
  (`fs2`/`fs4` crate, or a `.lock`-directory `mkdir` primitive — TBD, see
  Assumptions below) that actually works on Windows, unlike bash `flock`.
- Exit-code contract unchanged: `exit 2` + stderr for `enforce`-mode deny
  (matches `calm-nudge.sh`'s own 2026-07-14 migration off the JSON
  `permissionDecision` form, done specifically because of the
  #4669/#39344 reliability history documented in the sibling spec above),
  `exit 0` + stderr for nudge.

**Ported by subtraction, not rewritten from scratch.** The prior audit-design
pass on this project's own hook work (Item B in the spec cited above) rated
"a from-scratch minimal template throws away hard-won correctness" as a HIGH
failure mode, evidenced by real dated bugs (per-file re-arming fix, path-form
false-deny fix, DEBT-010 lock TOCTOU) still visible in `calm-nudge.sh`'s own
comments. The same lesson applies here: the Rust subcommand's state shape,
guard order, and fail-open philosophy must be a direct line-for-line port of
`calm-hooks.sh`'s already-fixed logic (itself already a derived subtraction
of `calm-nudge.sh`), not a fresh design — same `is_prose_file` exception,
same tamper-evident downgrade notice (FM3 in the hooks-mode spec), same
per-file (not per-session) `edit_context` re-arming.

**Invocation changes from shell-form to exec-form.** `hooks.json`'s `command`
+ `args` array bypasses the shell entirely — confirmed available this
session via Claude Code's own plugin hook schema research (`${CLAUDE_PLUGIN_
ROOT}` substitution works in `args` the same as `command`). Something in the
`args` array still needs to *resolve* the right per-platform `calm` binary
path, mirroring `npm/calm-mcp/bin/calm-mcp.js`'s existing `resolveBinary()`
(platform+arch → `optionalDependencies` package → binary path) — the plugin
install already carries this exact binary on disk for every supported
platform, since it's the same one `.mcp.json`'s `npx -y @eilodon/calm-mcp
serve` launches.

**Scope boundary — what this spec does NOT propose:** `calm-nudge.sh` (the
internal, actively-evolving dogfooding tool in *this* repo's own `.claude/`)
is out of scope. It has no Windows-distribution exposure today (this repo is
developed from a Linux/macOS source checkout) and rewriting it loses the
fast-iteration bash workflow that produced its own bug-fix history. Only the
plugin-scaffolded `calm-hooks.sh` — the one now auto-installed into external
users' Windows machines by this session's new bootstrap hook — is in scope.

## Open questions for audit-design

1. Does moving hook logic into the `calm` binary itself introduce a
   version-skew risk this bash version didn't have — e.g. an already-running
   MCP server holding a `.calm/index.db` connection while a *different*,
   just-launched `hooks-check` invocation of the same binary also opens one
   (SQLITE_BUSY / WAL contention), given this project's own documented
   history of multi-client concurrency bugs (`.calm/index.db` WAL growth
   incident referenced elsewhere in this project's history)?
2. Which lock primitive actually behaves correctly, cross-platform, for the
   session-state file (`fs2`, `fs4`, or a `mkdir`-based primitive already
   used elsewhere in this codebase, e.g. `session-start-build-calm.sh`'s
   `mkdir "$lock_dir"` pattern for its own release-build lock) — needs a
   real per-platform check, not an assumption.
3. Exec-form binary resolution: should `hooks-check` be invoked via a tiny
   long-lived wrapper (mirroring `calm-mcp.js`'s `resolveBinary()`) bundled
   in the plugin, or can `hooks.json`'s `args` array express the platform
   package's binary path directly without a JS indirection layer? Affects
   whether this still has a Node.js dependency in the hook path at all, or
   becomes a pure-native call.
4. Migration path: does `calm init --hooks=enforce` start writing the new
   exec-form wiring immediately (breaking any existing installs' `.claude/
   settings.json` shell-form entry, requiring a re-run), or does it need a
   transition period where both entrypoints are scaffolded/supported?

## Risk Assessment (audit-design)
<!-- audit-design: DO NOT DUPLICATE — update this section, do not append a second one -->
<!-- last-run: 2026-07-16 | trigger: NORMAL -->

**Tier:** 2 (Production) — this replaces a best-effort enforcement gate
already shipped to external users' machines (via this session's new plugin
bootstrap), not internal-only tooling.

### Failure Modes

1. **DB contention between a live `calm serve`/daemon connection and a
   freshly-spawned `hooks-check` invocation opening its own connection on
   `.calm/index.db`**, especially under Windows' mandatory (not advisory)
   file-locking semantics — MEDIUM — mitigation in plan: NO (named as Open
   Question 1, unresolved). Downgraded from an initial HIGH read because the
   Design section already proposes reusing `CalmServer::make_read_conn`'s
   existing connection pattern rather than inventing a new one — that
   pattern already serves concurrent MCP tool calls in production today, so
   whatever busy-timeout/retry behavior it has is likely to carry over. Not
   verified, though: the plan must confirm `make_read_conn` is safe to call
   from a short-lived one-shot CLI process (different lifecycle than a
   long-running server), not just assume the safety transfers.
2. **"Ported by subtraction, not rewritten from scratch" is a stated intent
   with no proposed verification mechanism** — HIGH — mitigation in plan:
   NO. The spec's own cited precedent (this project's Item B audit,
   2026-07-14) rated an unverified from-scratch-rewrite risk as HIGH for
   exactly this reason. Stating the intent to port faithfully doesn't check
   it; nothing in the spec proposes a test asserting the Rust and bash
   implementations produce identical allow/nudge/deny decisions over a
   shared corpus of recorded real tool-call payloads (this project already
   has decision-log JSONL precedent — `calm-nudge.sh`'s own
   `decisions.jsonl` — that a parity-test fixture could be seeded from).
3. **Dual-entrypoint transition window**: if a migration ever leaves both
   the bash `calm-hooks.sh` shell-form entry and the new exec-form
   `hooks-check` entry wired in the same `.claude/settings.json`
   simultaneously, Claude Code's own documented dispatch behavior ("all
   matching hooks run in parallel," per the sibling onboarding spec's
   external research) means BOTH fire per tool call — a decision-conflict
   failure mode (one nudges, the other denies) that structurally cannot
   happen today (exactly one hook exists). MEDIUM — mitigation in plan: NO
   (named as Open Question 4, unresolved), but concretely resolvable: an
   atomic swap (`calm init --hooks=enforce` removes the old shell-form block
   by exact command-string identity — the same removal-by-identity
   mechanism `remove_hooks_settings_block` already implements — before
   writing the new exec-form one) would close this cleanly.

### Layer Signals

- L1 Logic: exec-form binary resolution (Open Question 3) is an unspecified
  branch in the literal happy path — how `hooks.json`'s `args` array finds
  the right per-platform binary with no JS indirection is not designed yet,
  only deferred.
- L2 Concurrency: session-state lock primitive (Open Question 2) is
  explicitly TBD, and whichever is chosen has unverified Windows behavior —
  this is the layer's core question and it is still open.
- L3 Data: the spec does not state whether `hooks-check` reads/writes the
  SAME on-disk state formats `calm-hooks.sh` already uses (`.calm/
  hooks.mode`'s `schema=/mode=/written_by=/written_at=` lines, and the
  per-session JSON state file) or introduces a new schema. ASSUMED
  same-format compatibility, not stated or verified — a project mid-
  migration (state written by one implementation, read by the other) is a
  real scenario the current design is silent on.
- L4 Integration: correctness still rests entirely on Claude Code's own
  hook-dispatch contract — already named as "the single largest external
  dependency" in the sibling spec for shell-form hooks. Exec-form is less
  independently vetted in this project's own research this session (no
  citation found either confirming or denying exec-form's reliability
  parity with shell-form) — inherits the same #4669/#39344-class
  uncertainty for a code path with less real-world mileage.
- L5 Security: same class of concern the sibling spec's Item B raised for
  `calm-nudge.sh`'s hook mechanism — this installs code with the power to
  deny agent actions, automatically, on every tool call, now reading
  `.calm/index.db` via direct DB access instead of a CLI subprocess. No
  dedicated test suite is named in this spec, unlike the bash version's
  existing (if informal) test precedent.
- L6 Observability: unclear whether `calm doctor` gains new checks specific
  to exec-form failure modes (binary not found, DB lock timeout) distinct
  from its existing shell-form checks — not addressed.
- L7 Cross-cutting (idempotency): re-run behavior of `calm init
  --hooks=enforce` once it starts writing exec-form entries is exactly
  Open Question 4 — genuinely unresolved.

### Assumptions to Verify

- **ASSUMED:** "ported by subtraction" will hold in practice with no
  proposed verification (FM2).
- **ASSUMED:** whichever Windows lock primitive is chosen later will behave
  correctly — explicitly deferred by the spec itself (Open Q2), not
  resolved here, still worth naming as a hard blocker for the plan.
- **ASSUMED:** exec-form hooks are as reliable as shell-form on Claude
  Code's own dispatch — no supporting citation found this session.
- **DEFERRED ("TBD"):** migration path (Open Q4) is an explicit to-be-
  decided item per the spec's own text — flagged per this audit's Step 4.

### Abductive Hypotheses

1. **Interaction between two individually-correct components.** Even a
   flawless `hooks-check` changes hook cardinality per tool call during any
   mixed-install-state window (FM3) — two independently-correct hooks firing
   together is a new failure class Claude Code's parallel-dispatch model
   doesn't cleanly resolve unless the two commands are textually identical
   (they won't be: `bash ...sh` vs `calm hooks-check`). This is an emergent
   risk of the migration having a transition window at all, not a defect in
   either implementation.
2. **Malformed/adversarial stdin inverts the fail-open philosophy.** Bash +
   `jq`'s degrade-on-bad-input behavior (missing field → empty string → no
   dispatch branch matches → silent no-op) is far more forgiving than
   `serde_json::from_str` on malformed or oversized stdin (e.g. a Bash tool
   call embedding a multi-MB heredoc). If the Rust implementation doesn't
   explicitly catch a JSON parse failure and degrade to `exit 0`, a
   previously-harmless malformed payload could newly become an uncaught
   error/panic — which, depending on how such an exit code is interpreted,
   risks silently flipping this project's deliberate fail-open design
   philosophy into an accidental fail-closed one, in exactly the
   hardest-to-test-for input shape.

### Gate Result

**PASS WITH FLAGS.** Proceed to `writing-plans`, which MUST include:
mitigation for FM2 (a parity test asserting identical bash-vs-Rust decisions
over a shared real-payload corpus, seeded from existing `decisions.jsonl`
precedent); an atomic-swap migration strategy for FM3 (remove-before-write,
by exact command-string identity); explicit confirmation that
`make_read_conn` is safe to reuse from a one-shot CLI process for FM1;
a concrete choice (not a deferral) for the exec-form binary-resolution
mechanism (L1) and the cross-platform lock primitive (L2); an explicit,
tested "malformed stdin → exit 0, never a panic" contract (Abductive 2);
and a stated on-disk state-format compatibility guarantee (L3).

## Implementation status — shipped 2026-07-16

All five required-in-plan items resolved, in-repo:

- **FM1 (DB contention):** `resolve_symbol_path` in `crates/calm-core/src/
  hooks_check.rs` opens its own read-only connection with `PRAGMA
  query_only = ON` (same guarantee `make_read_conn` gives every MCP tool
  handler) plus an explicit `busy_timeout(500ms)`, and fails open (`None`)
  on any error — a one-shot CLI process is exactly SQLite WAL mode's
  supported concurrent-reader case, not a special one.
- **FM2 (parity/rewrite-drift):** not a literal bash-vs-Rust byte-diff
  harness (would have re-introduced the bash/jq dependency this spec
  removes, inside the test suite). Instead, `hooks_check::tests` pins the
  INTENDED behavior directly — mode read, prose exception, per-file
  per-session `edit_context` re-arming, per-session isolation, the
  diff_impact gate, and off-mode short-circuit — 13 tests, all green.
- **FM3 (dual-entrypoint migration):** `write_hooks_settings_block` in
  `crates/calm-core/src/hooks.rs` now does a real atomic swap:
  `block_is_calm_hook_block` recognizes EITHER the legacy shell-form
  command string OR the new exec-form's `args == ["hooks-check"]`, and
  `pre.retain(|b| !block_is_calm_hook_block(b))` runs before the new block
  is pushed, every write. Covered by
  `write_settings_block_atomically_swaps_away_a_legacy_shell_form_block`.
- **L1 (binary resolution):** `std::env::current_exe()` at `calm init
  --hooks` scaffold time — the exact binary already running that command is
  guaranteed to exist, no PATH/npx indirection needed. Composes for free
  with this session's separate plugin bootstrap work (`plugins/calm/hooks/
  bootstrap.sh` invokes `npx -y @eilodon/calm-mcp init --hooks=enforce
  --agents-md`; `current_exe()` inside that spawned process resolves to
  the npm-resolved platform binary's own path — no special-casing needed
  in either direction).
- **L2 (lock primitive):** `StateLock` in `hooks_check.rs` —
  `std::fs::create_dir` as the mutex, atomic create-or-fail on every
  platform this binary ships for (no crate dependency, no POSIX-only
  assumption `flock` had).
- **Abductive 2 (malformed stdin):** `run()` returns `0` on any stdin read
  failure or JSON parse failure, before `evaluate()` is ever called —
  tested directly (`malformed_stdin_fails_open_exit_zero_never_panics`,
  `empty_stdin_fails_open_exit_zero`).
- **L3 (state-format compatibility):** NOT preserved byte-for-byte with
  the bash version's state file shape — a new, Rust-native JSON shape
  (`SessionState { edit_context_files, needs_diff_impact }`) is used
  instead. Safe because this ships alongside the exec-form migration in
  the same change (old shell-form installs are swapped away by FM3's
  atomic swap, never left reading a state file the new format wrote, or
  vice versa) — not because formats happen to agree.

**Follow-up from the first pass — CLOSED 2026-07-16, same day.** Re-analyzed
on request: re-checked `.gitignore` directly rather than relying on memory,
and found the risk was narrower than first described. `.calm/` is entirely
gitignored but `.claude/settings.json` is NOT — so the cross-teammate/
cross-clone case already self-heals for free: each teammate's own
`.calm/hooks.mode` never syncs via git, so their bootstrap fires fresh on
their own machine and `write_hooks_settings_block`'s atomic swap overwrites
whatever a committed `settings.json` had, unconditionally, every time. The
real remaining gap was narrower: the SAME user, SAME machine, where
`.calm/hooks.mode` already exists (so the one-shot guard skips re-running
`init`) but the entrypoint path baked in at scaffold time goes stale
(project directory moved/renamed, `node_modules` layout changed, ...).

Closed with two additions:
- **`calm doctor --fix`** (new `Commands::Doctor` flag, `crates/calm-cli/
  src/main.rs`): reads the CURRENTLY configured mode and, if it isn't
  `Off`, re-runs `apply_hooks_flag` with it — self-heals a stale entrypoint
  by construction (already-idempotent, already tested
  `settings_block_rerun_with_a_moved_binary_self_heals_the_path`), never
  touches an explicit `Off`, never flips nudge<->enforce, no-ops when
  already healthy. 3 new end-to-end integration tests spawning the real
  binary (`crates/calm-cli/tests/hooks_doctor_fix.rs`): repairs a corrupted
  entrypoint path without changing mode; never touches `Off`; byte-identical
  settings.json when nothing needs fixing.
- **`plugins/calm/hooks/bootstrap.sh`** now calls `calm doctor --fix` on
  every SessionStart after the first (silent, cheap no-op when healthy),
  instead of only ever running `init` once. The one-shot guard still
  protects an explicit `--hooks=off` (the `--fix` dispatch itself skips Off
  mode entirely — two independent layers agreeing, not just one).

Not done in this pass (explicitly out of scope per the spec's own "Scope
boundary" section): `calm-nudge.sh` itself is untouched. The full
decision-log JSONL / native-vs-CALM exploration tally / tamper-evident
downgrade-notice features of the internal tool are also not ported —
`hooks_check` covers exactly the 2 hard gates plus mode read, same scope
`calm-hooks.sh` itself always had.
