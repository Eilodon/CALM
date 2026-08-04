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

## Risk classification's change-kind signal covers signatures only

`compute_touch_risk` (`crates/calm-server/src/tools/edit.rs`) now detects
one specific change kind: an edit that actually changes a touched
function/method's own signature TEXT (not just overlaps its line range —
a hunk that fully covers the signature but leaves it byte-for-byte
identical, e.g. `edit_symbol`'s default whole-body replace, does NOT
escalate) raises risk to "high", reusing `diff_impact`'s own
`escalate_risk_if_signature_changed` after a real semantic comparison
(`is_signature_semantically_changed`). What's still not modeled: the
broader taxonomy this entry originally asked for (comment-only /
body-only / visibility / deletion / security-sensitive). Two edits to the
same low-fan-in symbol that both leave the signature alone — one renaming
a local variable, one deleting an authorization check in the body — still
get the same risk tier. Turning the actual diff content into a full
change-kind classification remains unstarted; the signature dimension was
the highest-value, most tractable slice to land first.

## `reason` grounding has a stronger opt-in path; the default is still lexical

`edit_lines`/`edit_symbol` gained a `cites` param: set it to the EXACT
`qualified_name` of a caller `edit_context` returned this session (already
freshness/digest-verified the same way the existing lexical citation is —
see `known_caller_qns`'s own freshness-window + caller-set-digest check)
and the gate checks it by equality, not a substring search — closing the
"paste a real caller name into an unrelated sentence" gaming path this
entry used to describe. But `cites` is optional and additive, not a
replacement: an agent that never sets it still goes through the original
`cites_token` word-boundary substring check against `reason`, which is
exactly as gameable as before. Cutting over to `cites`-only (deprecating
the lexical path entirely) would close this properly, but is a breaking
change to every existing caller's request shape — a deliberate follow-up,
not attempted here.

## Remote HTTP has a request-size/concurrency floor, not a real DoS policy

`serve_http` (`crates/calm-server/src/http.rs`) now caps request body size
(16 MiB) and concurrent in-flight requests (64) via `axum::extract::
DefaultBodyLimit` and `tower::limit::ConcurrencyLimitLayer` — closing the
most egregious unbounded-resource gaps a bare `axum::Router` had. Still
genuinely absent: per-IP rate limiting, backoff, or request queueing.
`docs/http-transport.md` is explicit this remains defense-in-depth only;
put a real reverse proxy in front if you need actual rate limiting.

## Shared daemon has one capability ceiling for every connection

A daemon's tool-preset ceiling is fixed at whichever process first
spawned it (`crates/calm-server/src/daemon.rs`); `calm connect --preset`
only takes effect if that connection is the one doing the spawning. Two
MCP clients attached to the same project daemon share the same ceiling —
there's no per-connection handshake negotiating a narrower profile per
client. Each connection *does* get its own session state (`oriented`,
`enabled_toolsets`, `session_log`), just not its own ceiling.

## Malicious/pathological-repo indexing DoS has partial mitigations

`SECURITY.md` still calls resource-exhaustion-via-huge-repo out of scope
unless it also causes memory corruption or RCE, but two of the concrete
gaps that stance used to rest on are now closed: `read_source_capped`
(`crates/calm-core/src/indexer/pipeline.rs`) skips any file over 8 MiB
before it's ever read into memory (checked via a cheap `metadata()` stat,
not a full read), and `parse_tree` (`crates/calm-core/src/indexer/
parser.rs`) now bounds a single tree-sitter parse to 5 seconds via
`Parser::set_timeout_micros`. Still genuinely missing: an AST-node budget
for a file that's under the size cap but pathologically nested, and a
`.calm/` disk quota. Those would need their own design (a node-count
callback into tree-sitter's walk, and a disk-usage check somewhere in the
maintenance/checkpoint path) — not attempted here.

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
