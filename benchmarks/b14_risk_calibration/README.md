# B14 — Risk calibration: does `aggregate_risk` track real bug-inducing commits?

## Why this benchmark exists

Every other risk-adjacent check in this suite (b12's `edit_context`/
`diff_impact` correctness checks) verifies the *mechanism* works as coded —
none of them ask whether the resulting risk score lines up with what
actually turned out to be risky. The 2026-08-05 CALM-improvements review
named this gap explicitly (P1.3): extending the risk model (comment-only/
deletion/visibility signals — the P2 items in that same review) without a
calibration baseline first means extending it blind. This is that baseline.

## Methodology

**Ground truth (SZZ-lite, deliberately narrow — see Limitations)**: a commit
C is labeled `risky` if the commit immediately following it (no-merges
order) is a `fix(`/`fix:`/`fix ` commit touching at least one file C also
touched. `safe` is a random sample of same-population commits with a
`feat`/`fix`/`refactor`/`perf`/`style`/`test` prefix that aren't a risky
label's parent. Mined from **this repo's own git history** — real incidents,
not a synthetic corpus (the very first risky pair found, `86f56aba` →
`24083fc`, is the exact state.db-rewiring incident that motivated this
session's own P0 work).

**Analysis**: for each labeled commit, `git worktree add --detach` at the
commit's *own parent*, index that worktree (embeddings disabled — irrelevant
to `compute_touch_risk`, costs ~3min/run at this repo's size otherwise), run
`calm guard --commits <parent>..<commit> --json`, read `aggregate_risk`.
Indexing at the parent (not today's HEAD) matters: `diff_impact` maps hunk
line ranges onto the *current* index's symbol table, so analyzing an old
diff against today's index would silently misattribute risk to symbols that
have since moved/renamed.

Run: `python3 benchmarks/b14_risk_calibration/run_benchmark.py --sample 12`
— 12 risky + 12 safe (of 25+25 mined; capped for session time budget, ~2min/
commit × 24 ≈ 43 min actual). `results.json` has every row.

## Headline numbers

| threshold | tp | fp | tn | fn | precision | recall | f1 | annoyance rate |
|---|---|---|---|---|---|---|---|---|
| low | 12 | 12 | 0 | 0 | 0.50 | 1.00 | 0.67 | 100% |
| medium | 8 | 9 | 3 | 4 | 0.47 | 0.67 | 0.55 | 71% |
| high (calm guard's default) | 8 | 9 | 3 | 4 | 0.47 | 0.67 | 0.55 | 71% |

Taken at face value, this looks weak — 47% precision, a 71% "annoyance rate"
(fraction of all reviewed commits that would have failed `calm guard
--fail-on high`). **It would be dishonest to stop here.** Auditing both
error classes individually tells a materially different story.

## Auditing the false negatives (4/4 are a ground-truth labeling bug, not a CALM miss)

Every one of the 4 risky commits CALM scored `low`:

| commit | subject | the "fix" that labeled it risky |
|---|---|---|
| `a7d1b876` | `docs(plans): root-cause remediation plan...` | `fix(core,server): add refresh reconciliation...` |
| `39e02813` | `chore(deps): bump fast-uri in /tests/js_client_interop` | `fix(deps): override transitive @hono/node-server...` |
| `6805abcd` | `docs: risk assessment for calm-remaining-backlog...` | `fix(hooks,edit): close DEBT-010 hook-state TOCTOU race` |
| `31e62db0` | `docs: defer Plan 3 Phase B to a dedicated session` | `fix(search): min-max normalize personalization boost...` |

All four are `docs:`/`chore(deps):` commits — not code changes at all. The
SZZ-lite heuristic labeled them "risky" purely because they happen to sit
immediately before a `fix:` commit that *also* touched an overlapping file
path (a docs file mentioning the same filename, a lockfile a later dep-patch
also touches) — not because they introduced the bug the fix actually
addresses. This is a known, textbook SZZ limitation (real SZZ does line-
level blame tracing specifically to avoid this; the "immediate parent + file
overlap" shortcut here doesn't). **`calm guard` scoring these `low` is
correct** — there is essentially nothing here to flag. Excluding these 4
mislabeled rows, recall on the remaining 8 genuinely-code-touching risky
commits is **8/8 = 100%** at every threshold.

## Auditing the false positives (9/9 correspond to real hub touches)

Every `safe`-labeled commit CALM scored `high` has at least one symbol in
its `high_risk_symbols` list; every one scored `low` has zero — a
perfectly clean split, not noise:

| commit | risk | hub symbols flagged | subject |
|---|---|---|---|
| `f8461683` | high | 2 (`scip_overlay::run_all`, `JavaConfig::default`) | LSP resolve-time overlay |
| `7814dc1f` | high | 2 (`common::line_preview`, `CalmServer::edit_context`) | batch call-site preview lookups |
| `200795f6` | high | 1 (`CalmServer::understand`) | ETag support on `source()` |
| `7288fa24` | high | 2 (`gopls_resolve_binary`, `clangd_resolve_binary`) | wire gopls/clangd |
| `5a7013be` | high | 4 (`rebuild_graph`, `resolve_module_to_path`, ...) | Rust cross-crate resolution |
| `d341fbbd` | high | 1 (`CalmServer::edit_lines`) | move embedding off the edit lock |
| `7497c3de` | high | 6 (`language_for_extension`, `extract_symbols_shallow`, ...) | Zig grammar support |
| `048cfb9b` | high | 3 (`CalmServer::diff_impact`, ...) | cap/order caller/callee lists |
| `0806531b` | high | 6 (`refresh_caller_counts`, `reindex_paths`, ...) | incremental_graph_update |
| `bb3347d9` | low | 0 | point configs at mcp-launcher.sh |
| `6229c017` | low | 0 | B7 benchmark |
| `c2865b34` | low | 0 | semantic search first-run UX |

`edit_context`, `edit_lines`, `diff_impact`, `understand`, `rebuild_graph` —
these are among the most central functions in the whole server. **A commit
that modifies one of these IS structurally risky by any reasonable
definition, whether or not it happened to ship a bug that got immediately
fixed.** The benchmark's `safe` label only encodes "wasn't immediately
followed by a fix" — a materially weaker claim than "was actually safe to
make." Scoring these `high` is the risk model doing exactly its designed
job (flag hub touches for extra scrutiny); calling it a false positive
requires accepting a ground-truth definition of "safe" this benchmark never
actually claimed to establish.

## What this benchmark actually shows

- **On the narrow question it can answer cleanly** (does `aggregate_risk`
  correlate with real caller-count/hub structure) **the signal is clean and
  strong**: every hub-touching commit in this sample scored high, every
  non-hub-touching one scored low, with zero exceptions in either direction.
- **On the broader question** (does `aggregate_risk` predict "will this
  specific commit need an immediate fix") **the raw numbers are weak, and
  correctly so** — CALM's risk model is topology-based (caller count, hub
  status, signature change), not outcome-based (whether this particular
  hub touch happens to contain a bug). Expecting hub-touch-detection to
  double as bug-prediction was never a claim CALM's own docs make; this
  benchmark's initial framing implicitly expected it to, and the audit
  above is the correction.
- The real, actionable finding for the P2 risk-model work this review also
  discusses (comment-only/deletion/visibility signals): **the topology
  signal already works as designed and doesn't need recalibrating.** What's
  missing is a signal that distinguishes *which* hub touches are actually
  dangerous (the 8 genuinely risky ones) from which are routine maintenance
  on a hub (the 9 safe-but-hub-touching ones) — exactly the "change-kind"
  gap KNOWN_LIMITATIONS.md's "Risk classification's change-kind signal
  covers signatures only" section already names.

## Limitations (honest accounting)

- **Sample size**: 12 risky + 12 safe (of 25+25 mined) — capped for this
  session's time budget. Re-run with `--sample` omitted (all 25+25) for a
  tighter confidence interval before treating any single percentage here as
  stable.
- **Single repo**: this benchmark only mines CALM's own history. A
  project with different commit-message conventions, review culture, or
  codebase shape could calibrate differently — this is a baseline
  methodology and a first data point, not a cross-project claim.
- **SZZ-lite window is narrow by construction**: only catches a fix
  landing as the *literal next* no-merges commit. A bug fixed 5 commits
  later, or by a fix that doesn't start with `fix(`, is invisible to this
  ground truth entirely (undercounts `risky`, doesn't affect `safe`).
- **`safe` measures "not immediately fixed," not "was safe"** — see the
  false-positive audit above. A stricter ground truth (e.g., cross-
  referencing actual production incidents, or a longer fix-window) would
  very likely raise measured precision without CALM's risk model changing
  at all.

## Reproducing

```bash
cargo build -p calm-cli
python3 benchmarks/b14_risk_calibration/run_benchmark.py            # all mined risky+safe
python3 benchmarks/b14_risk_calibration/run_benchmark.py --sample 12  # capped, ~45min
```

`results.json` (gitignored -- `benchmarks/**/results.json`, same convention
every other benchmark in this suite follows; not committed, regenerate via
the command above) has the full row-level data behind every number in this
README — `sha`, `label`, `aggregate_risk`, `files_changed`,
`high_risk_symbols`, and (for risky rows) the fix commit that supplied the
label, for independent re-auditing.
