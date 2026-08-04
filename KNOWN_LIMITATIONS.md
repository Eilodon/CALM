# Known limitations

An honest list of gaps in CALM as it stands today, kept separate from
`CONTRIBUTING.md`'s "roadmap items open for contribution" so a user
evaluating CALM can see what NOT to assume without digging through plans
and source. Each entry says what's missing, why it's out of scope for now
rather than half-implemented, and where to look if you want the detail.

This file is about the *product*, not a single commit — it will drift out
of date the moment something on it gets fixed. If you land a fix for one
of these, delete the entry in the same PR rather than leaving it stale.

## Verification is single-language, single-check, and unsandboxed

`verify_change` runs exactly one thing today: `cargo check` on the nearest
Rust package (`crates/calm-core/src/verify.rs`). It is bound to the
transaction's `proposed_digest` (checked before *and* after the run — a
concurrent write can't get bound to someone else's receipt) and has a
wall-clock timeout, but the process itself is not sandboxed: no network
policy, no filesystem allowlist beyond the OS's own, no toolchain pinning.
`cargo check` can run `build.rs`, proc-macros, and (if not already
vendored) fetch dependencies — that's real code execution, not just
reading compiler diagnostics.

Extending verification to other languages (`go build`/`test`, `tsc
--noEmit`, `pytest`) needs a shared execution-policy abstraction
(network: deny/allow-registry, filesystem: repo-ro + target-rw, env
allowlist, resource limits) applied uniformly first — bolting each new
language's runner directly onto today's bare `Command::new(...)` would
just multiply the unsandboxed surface instead of closing it. Not started.

## Durable state and the rebuildable index share one SQLite file

`.calm/index.db` holds the symbol/call-graph index (rebuildable from
source, `PRAGMA synchronous=NORMAL` is a deliberate tradeoff for it) *and*
the edit-transaction journal, audit ledger, and project-memory notes
(none of which are rebuildable). All of it currently shares that same
`synchronous=NORMAL` posture and the same physical file. A hard
power-loss can only lose the last few committed rows under WAL+NORMAL,
never corrupt the file — an acceptable cost for a cache, less obviously
so for a journal used as evidence.

Splitting into two files (`index.db` at NORMAL, a `state.db` at FULL for
transactions/ledger/memory) is a real architecture change — new
migration path, two DB handles instead of one, cross-file consistency to
reason about — not attempted here.

## No multi-file change-set / transaction

Every `edit_lines`/`edit_symbol`/`format_files` call gets its own,
independent `EditTransaction` (`crates/calm-core/src/txn.rs`), scoped to
one file. A multi-file rename or refactor is N independent transactions
that can each land in a different state (one committed, one gated, one
failed) with no aggregate "did the whole refactor succeed" view. There is
no `prepare → validate-all → atomic-ish commit → verify` pipeline across
files, no `PARTIALLY_APPLIED` status, no rollback plan spanning a set.

## No unified reference-impact tool

`callers`/`edit_context` model the *call* graph. A safe rename needs the
full reference surface — imports, re-exports, type positions, string/
config references — which today means composing `edit_context` +
`dependencies` + `search(kind="grep")` by hand. This is not
hypothetical: `benchmarks/b7_task_correctness` has two real, currently
failing cases from this exact gap (`rename_express_set_charset`,
`rename_zod_prettify_error` — both miss a bare re-export statement). A
single `reference_impact(symbol, operation="rename")` tool that merges
call edges, SCIP references, imports/re-exports, and textual matches into
one classified list (`must_change`/`likely_change`/`review`/
`textual_only`) would close this, but doesn't exist yet.

## Risk classification has no change-kind signal

`compute_touch_risk` (`crates/calm-server/src/tools/edit.rs`) is driven by
caller-count/hub-status and, as of this pass, an optional path-based
`risk_rules` floor (`.calm/config.json`) — but it has no idea whether the
edit itself is a comment tweak, a body-only change, or a public-signature
break. Two edits to the same low-fan-in symbol get the same risk tier
whether one renames a local variable and the other removes an
authorization check. A real fix needs the actual diff content turned into
a classified "change kind" (comment-only / body-only / signature /
visibility / deletion / security-sensitive) and folded into the risk
model as its own axis — not started.

## `reason` grounding is a lexical check, not semantic

The write gate's "cites a real caller" check (`cites_token`,
`crates/calm-server/src/tools/edit.rs`) is a word-boundary substring
match against `edit_context`'s returned caller names. An agent can
satisfy it by pasting a caller name into an unrelated sentence. This is
narrower than it sounds: risk ≥ "high" already requires an independent
elicitation round-trip regardless of what `reason` says (`
high_risk_needs_independent_review`), so the exploitable window is
specifically the medium-risk, cited-caller path. Replacing free-text
`reason` with structured evidence IDs (`edit_context` returns stable
per-caller IDs, `prepare_edit`-style tools require citing one, the server
verifies it's real/fresh/not-superseded) would close this properly; not
started.

## Remote HTTP has no per-tool network/DoS policy

`calm serve --http --allow-remote` forces the `remote-safe` preset
(every tool with `read_only_hint = true` — see `CHANGELOG.md`'s
Unreleased section for what that replaced), but the transport itself
still has no built-in rate limiting or request-size/DoS protection
(`README.md` already says so). A read-only tool can still be hammered.
Out of scope for this feature's current threat model
(`docs/http-transport.md`); put it behind a reverse proxy if that
matters for your deployment.

## Shared daemon has one capability ceiling for every connection

A daemon's tool-preset ceiling is fixed at whichever process first
spawned it (`crates/calm-server/src/daemon.rs`); `calm connect --preset`
only takes effect if that connection is the one doing the spawning. Two
MCP clients attached to the same project daemon share the same ceiling —
there's no per-connection handshake negotiating a narrower profile per
client. Each connection *does* get its own session state (`oriented`,
`enabled_toolsets`, `session_log`), just not its own ceiling.

## Malicious/pathological-repo indexing DoS is explicitly out of scope

`SECURITY.md` states this directly: a maliciously huge/malformed repo
causing resource exhaustion is out of scope unless it also causes memory
corruption or RCE. For a tool whose entire job is indexing repos an agent
may point it at without much vetting, that's a real gap, not just a
formality — no file-size cap, AST-node budget, parse timeout, or
`.calm/` disk quota exists today.

## CLI binary name collides with an unrelated project

The native release binary and `scripts/install.sh` both install a command
named `calm` — the same name FINOS's `@finos/calm-cli` ("Common
Architecture Language Model") uses. The npm package (`@eilodon/calm-mcp`)
already avoids this (its `bin` entry is `calm-mcp`, not `calm`), but the
native/GitHub-release path doesn't. Renaming the native binary is a
breaking change for anyone who's already scripted against `calm` and
needs a deliberate decision (and probably a compatibility-alias
transition period), not a silent rename — not done here.

## No Git/CI-native integration path

Everything above assumes an MCP client calling CALM's tools directly. A
native editor `Edit`/`Bash` call, or a change made outside any MCP
session entirely (a teammate's local edit, a bot PR), is invisible to
CALM. There's no `calm guard --staged` / `calm review-diff --base
origin/main` CLI surface and no publishable GitHub Action — the
integration points where a team would see CALM's value without depending
on every contributor's agent calling the right tool. Not started.
