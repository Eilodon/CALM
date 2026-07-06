# B11 — Extended Real Competitor A/B (`calm` vs CodeGraph vs Semble vs grepai vs Serena)

Supersedes B10 after an audit (2026-07-06) found its methodology unreliable in ways that mattered
for exactly the claims it was being used to support. See `run_benchmark.py`'s module docstring for
the full list; short version:

1. Only `find_callers` had a correctness oracle — the other 3 tasks measured token/call cost
   without ever checking whether the answer was actually right.
2. `pre_edit_blast_radius`'s anchors were never checked against CALM's own hard-refuse gate
   condition (`is_hub`/`risk_assessment: high`) — B10 never actually verified it was testing a
   symbol where the gate would trigger.
3. N=1 run, no repeats, no median/IQR.
4. GPT-4 tokenizer (tiktoken) stood in for whatever tokenizer an agent's real runtime uses,
   un-flagged as a proxy.
5. No readiness-gate for tools with real warm-up cost before timing started.

This benchmark adds **grepai** ([yoanbernabeu/grepai](https://github.com/yoanbernabeu/grepai) —
Ollama `nomic-embed-text` embeddings + a real structural call graph) and **Serena**
([oraios/serena](https://github.com/oraios/serena) — LSP-backed via `rust-analyzer` for this Rust
corpus) to `calm`/CodeGraph/Semble, fixes all 5 issues above, and adds two new tasks
(`risk_gate_refusal`, `memory_recall`) that actually exercise CALM's claimed differentiators —
B10's 4 tasks never touched either.

GitNexus was scoped out of this round at the user's request (not installed, no numbers below).

## Run

```bash
# one-time setup — grepai + Serena + CodeGraph, all against an ISOLATED corpus (see Safety below)
ollama pull nomic-embed-text                                    # grepai's embedding model
curl -sSL https://raw.githubusercontent.com/yoanbernabeu/grepai/main/install.sh | \
  INSTALL_DIR=~/.local/bin sh                                    # or brew, see grepai's own docs
CORPUS=/some/scratch/path
git worktree add "$CORPUS" HEAD --detach                        # isolated copy, NOT the live repo
cd "$CORPUS"
grepai init && grepai watch &                                    # wait for `grepai_index_status.symbols_ready`
npx -y @colbymchenry/codegraph init                              # NOT a bare `codegraph` — see safety note below
uvx --from git+https://github.com/oraios/serena serena project create . --language rust --index
cd -

cargo build --release -p calm-cli                                # if not already built
benchmarks/.venv/bin/python benchmarks/b11_extended_competitor_ab/run_benchmark.py --corpus "$CORPUS"
```

## Safety — why this runs against an isolated worktree, not the live repo

`risk_gate_refusal` performs a **real edit attempt** against a real hub symbol
(`crates/calm-core/src/indexer/pipeline.rs::reindex_changed`, verified `is_hub: true` via
`hotspots(include_symbols=true)`). Serena has no confirmation gate at all — the schema for
`replace_symbol_body` has no `confirm`/`force` field — so it **actually rewrites the file**. This
benchmark points every one of the 5 MCP servers at an isolated `git worktree` copy of this repo
(`--corpus`), never the live checkout, and resets the file with `git checkout --` immediately after
each attempt. Running this against a live working tree would corrupt it.

Two more things worth knowing if you re-run this yourself:

- **Name collision**: a bare `codegraph` on `PATH` may resolve to an unrelated Rust CLI tool that
  also calls itself `codegraph` (found live during this benchmark's setup — its `init` writes its
  own `.codegraph.db`, a `CLAUDE.md`, and Claude Code hooks, none of which have anything to do with
  `@colbymchenry/codegraph`). Always spawn it as `npx -y @colbymchenry/codegraph`, matching this
  repo's own `.mcp.json`.
- **Serena's project language config matters for what it can even attempt.** A first draft of
  `risk_gate_refusal` anchored on a Python hub symbol
  (`benchmarks/lib/mcp_client.py::MCPClient.call_tool`, also independently verified `is_hub: true`)
  and got a Serena error: `"Explicitly requested symbols in '...' while the path is ignored"`. Root
  cause: the corpus's `.serena/project.yml` only configures `languages: [rust]`, so Serena's
  LSP-backed symbol tools cannot operate on Python files at all — a real limitation of *this test's
  setup*, not evidence about Serena's risk-awareness. Reporting that as "Serena refused" would have
  been dishonest. Switched the anchor to `reindex_changed` (Rust, already the `pre_edit_blast_radius`
  anchor) so both tools are being asked about a symbol they can actually act on.

## Results (self-repo corpus, N=5 repeats per task per tool — median tokens shown)

Encoding: GPT-4 BPE via `tiktoken` — **a proxy for token cost, not the real tokenizer of whatever
model an agent actually runs on.** Treat absolute numbers as directionally comparable across tools
in this table, not as a claim about real API billing.

| Task | naive tok | `calm` (ratio) | CodeGraph (ratio) | Semble (ratio) | grepai (ratio) | Serena (ratio) |
|---|---|---|---|---|---|---|
| read_one_function | 28,659 | 1,116 (25.7x) | 1,720 (16.7x) | 259 (110.7x)* | 121 (236.8x)* | 1,117 (25.7x) |
| find_callers | 187 | 337 (0.6x) | 56 (3.3x) | 764 (0.2x)* | 702 (0.3x) | 280 (0.7x) |
| pre_edit_blast_radius | 55,473 | 1,596 (34.8x) | 96 (577.8x) | 675 (82.2x)* | 6,114 (9.1x) | 1,352 (41.0x) |
| locate_and_inspect | 43,163 | 5,571 (7.7x) | 3,407 (12.7x) | 438 (98.5x) | 898 (48.1x) | 181 (238.5x) |

\* marked `unsupported`/structurally-different in `competitor_tasks.yaml` — cheap because it's
answering an easier question (see "How to read this data" below), not because it's more efficient
at the *same* question.

**None of this table should be read as a ranking without the correctness columns below — see "How
to read this data."**

### Correctness — the part B10 skipped for 3 of 4 tasks

| Task | `calm` | CodeGraph | Semble | grepai | Serena |
|---|---|---|---|---|---|
| read_one_function (body-line completeness vs 85-line ground truth) | 0.89 | 0.96 | 0.06 | 0.06 | 0.89 |
| find_callers (file recall vs grep oracle, 2 files) | 2/2 | **1/2** | 2/2 | 2/2 | 2/2 |
| pre_edit_blast_radius (file recall vs grep oracle, 5 files) | 5/5 | **1/5** | 4/5 | 5/5 | 5/5 |
| locate_and_inspect (found correct path + symbol name) | path✓ sym✓ | path✓ sym✓ | path✓ sym✓ | path✓ sym✓ | path✗† sym✓ |

† Serena's `get_symbols_overview` never echoes the file path in its output (you already gave it the
path as an argument) — this is an oracle artifact of a substring-presence check, not evidence
Serena lost track of the file. Read as inconclusive for this one cell, not a miss.

**CodeGraph misses the same cross-crate caller in both `find_callers` and `pre_edit_blast_radius`**
(`crates/calm-server/src/tools/recover.rs`, called via a fully-qualified path
`calm_core::indexer::pipeline::collect_source_files(...)` / `reindex_changed` transitively) — this
reproduces B10's original finding on a fresh run, so it isn't a fluke of that one measurement.
`pre_edit_blast_radius` recall of 1/5 is the sharpest version of this: `codegraph_impact` returns 4
symbols total for `reindex_changed`, missing 4 of 5 real caller files entirely.

### risk_gate_refusal — a real edit attempt on a verified hub symbol, no confirmation given

| Tool | Result |
|---|---|
| `calm` | **Refused.** `CONFIRM_REQUIRED`: *"this edit touches a hub symbol (is_hub=true) — pass confirm:true to proceed."* File unchanged (verified via `git diff`). |
| Serena | **Did not refuse.** `replace_symbol_body` has no `confirm`/`force` field in its schema at all — it just rewrote the file. Verified via `git diff` before the corpus was reset. |
| CodeGraph | `unsupported` — read-only by design, no edit tool. |
| Semble | `unsupported` — no edit tool of any kind. |
| grepai | `unsupported` — no edit tool of any kind. |

This is the empirical confirmation `docs/comparison.md`'s qualitative claim ("Serena: Không thấy
công bố risk-gate trước khi sửa") previously only asserted from public docs — now measured directly
against a real edit attempt.

**Self-correction, for the record**: this benchmark's own initial audit (which motivated this
rewrite) had claimed B10's `pre_edit_blast_radius` anchors were *not* hub symbols, so B10 supposedly
never got near CALM's gate at all. That was checked too loosely at the time — `reindex_changed` is
in fact `is_hub: true` (confirmed twice now via `hotspots`). The real gap wasn't "B10 never touched
a hub symbol" — `pre_edit_blast_radius` already did, every run — it's that **B10 never attempted an
actual edit**, only ever called the read-only `edit_context`/equivalent. Reading, even of a hub
symbol, never triggers the gate; only a write attempt does. `risk_gate_refusal` is the first task in
this benchmark series to actually attempt one.

### memory_recall — write a note, kill the process, respawn it, read the note back

| Tool | Result |
|---|---|
| `calm` | **Persisted.** `remember` → process closed → new process on the same corpus → `recall` returned the exact content. |
| Serena | **Persisted.** `write_memory` → process closed → new process → `read_memory` returned the exact content. |
| CodeGraph | `unsupported` — file watcher only, not an interpretive memory/notes store. |
| Semble | `unsupported` — no memory/notes concept. |
| grepai | `unsupported` — no memory/notes concept. |

**This corrects `docs/comparison.md`, not just extends it.** That doc currently lists Serena's
"Memory bền qua session" column as "Không" (no) — false. Serena ships `write_memory`/`read_memory`/
`list_memories`/`delete_memory` tools (memories are markdown files under `.serena/memories/`,
trivially surviving a process restart, confirmed live above). Fixed as part of this same audit — see
the diff to `docs/comparison.md`.

## How to read this data — don't just look at the ratio column

Same warning B10 gave, sharper now that there are 5 tools instead of 3:

- **`read_one_function`**: Semble and grepai's low token counts (259, 121) come with completeness
  scores of 0.06 — they're returning a short *snippet*, not the function body. Their "ratio" is
  cheap because they're answering a different, easier question ("where roughly is this concept") —
  not because they're more efficient at reading a whole function. CodeGraph (0.96) and `calm`/Serena
  (0.89 each) are actually comparable to each other here; Semble/grepai are not comparable to any of
  them on this specific task.
- **`pre_edit_blast_radius`**: CodeGraph's 577.8x ratio looks dominant, but its recall is 1/5 — it is
  cheap because it returns far less (4 symbols, no source, no risk score), not because it compresses
  the same answer better. `calm`'s `edit_context` costs more (1,596 tok) because it returns full
  caller/callee lists + `risk_assessment` + `is_hub` + a `suggested_next` — the exact pre-edit safety
  data `docs/comparison.md` says the whole tool category is missing.
- **`find_callers`**: `calm`, grepai, and Serena all cost *more* tokens than the naive grep baseline
  (ratio < 1x) — because naive here is a single ungrepped `grep -rn`, which is already free. The
  advantage of a real call-graph tool shows up on `pre_edit_blast_radius` (naive needs to open every
  matched file), not here.
- **Semble** is marked `unsupported` on 2 of 4 tasks (embedding search, no call graph) yet still
  scores 2/2 and 4/5 recall by coincidence of a small, single-symbol corpus where the right files
  happen to rank highly semantically — this is not evidence Semble does call-graph analysis; it's
  evidence this corpus is small enough that semantic search and structural search overlap a lot.
  Don't extrapolate this recall to a larger/noisier codebase.

## Limitations

- **N=5 repeats, self-repo only, 6 tasks.** More rigorous than B10's N=1, still not a claim of
  statistical significance — every repeat call was deterministic (all 5 raw samples per cell were
  identical in this run), so median+IQR added robustness against transient MCP/process hiccups, not
  variance in the underlying tools themselves.
- **`read_one_function`/`locate_and_inspect` oracles are line/substring-based**, not semantic — a
  tool that reformats or paraphrases correct content could score lower than one that repeats it
  verbatim. Good enough to catch "returned nothing relevant" (Semble/grepai above), not fine enough
  to rank two verbatim-correct answers against each other.
- **GitNexus intentionally excluded** this round (user request) — no numbers here for it; see
  `docs/comparison.md`'s qualitative table for what's known about it from public docs only.
- **grepai's `trace_callers` includes a spurious self-reference** in its raw output (the definition
  line listed as its own "caller") — didn't affect file-level recall here since it's the same file
  as the real callers, but would inflate a naive call-site *count* if one were computed instead of
  file recall.
- **CodeGraph's 7 secondary tools default to hidden** unless `CODEGRAPH_MCP_TOOLS` is set — this
  benchmark sets it to enable all of them for a 1:1 comparison; an agent using CodeGraph's default
  config (only `codegraph_explore`) would see different numbers than shown here.

## Files

- `run_benchmark.py` — the runner (5 MCP clients, oracle functions, risk-gate/memory logic).
- `../lib/tasks.yaml` — unchanged from B4/B6/B10 (4 original tasks; `risk_gate_refusal`/
  `memory_recall` live directly in `run_benchmark.py` since they have no natural "naive cat/grep"
  baseline to pair with — they test a safety/persistence *property*, not read cost).
- `../lib/competitor_tasks.yaml` — extended with `grepai`/`serena` mappings for all 4 shared tasks;
  schemas captured live from each server's real `tools/list` response, not from docs.
- `results.json` — not committed (see `.gitignore`), regenerate with `--corpus` to get fresh numbers.
