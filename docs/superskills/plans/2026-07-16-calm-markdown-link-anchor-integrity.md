# Markdown Link/Anchor Integrity (v1: on-demand) Implementation Plan

> **For agentic workers:** Use `subagent-driven-development` (recommended)
> or `executing-plans` to implement this plan task-by-task.

**Goal:** Detect markdown links (`[text](target)`) whose target file or
`#anchor` fragment doesn't resolve to a real file/heading, surfaced as a new
`fitness_report` check — the actual "does this link work" signal the spec's
Problem section asks for, without touching tree-sitter or `parse_status`.

**Architecture:** A new `analysis::doc_links` module (sibling to
`analysis::config_drift`), reusing `config_drift::build_real_path_index` for
the repo's real-path universe and a new relative-to-referring-file resolver
(clamped to `project_root`) for `./`/`../` markdown-link targets. Heading
anchors are computed with a GitHub-slug port over `extract_markdown_symbols`'s
existing heading output — no schema change, no new table, no new dependency.
Wired into `run_fitness_check`/`fitness_report` exactly like
`check_config_drift` is today (same `doc_paths` config key, reused as-is).

**Tech Stack:** Rust, `regex` (already a workspace dep), SQLite via existing
`analysis`/`fitness` modules. No new crates.

**Audit Gate:** PASS WITH FLAGS (`docs/superskills/specs/2026-07-16-calm-markdown-content-semantics.md`)

**Risk Flags:** none HIGH after decomposition (see Self-Review/Risk Summary
at the end) — the one genuinely HIGH-blast-radius piece of the original spec
(item 3, index-time edges queryable like `callers`) is deliberately **out of
scope for this plan** — see Scope Decision below.

---

## Scope Decision (read before implementing)

The approved spec (`2026-07-16-calm-markdown-content-semantics.md`) has 4
design items. This plan implements **items 1 and 2 only**:

1. Heading anchor-slug computation (GitHub-slug algorithm).
2. Markdown link extraction + path/anchor resolution, wired into
   `fitness_report` on-demand (same shape `check_config_drift` already uses).

**Explicitly deferred, each to its own follow-on plan:**

- **Item 3** (index-time `call_edges` storage so a doc anchor is queryable
  like `callers`, plus a `diff_impact.doc_link_impact` field). Investigating
  the actual integration surface for this plan surfaced real complexity the
  spec's audit-design pass hadn't seen: `rebuild_graph`
  (`crates/calm-core/src/indexer/pipeline.rs:1000-1057`) does
  `DELETE FROM call_edges` + re-insert from `resolve_sites_to_edges`, so a
  separate doc-link edge writer must run at a specific point in that
  sequence (after the delete/re-insert, before `refresh_caller_counts`) to
  survive a full reindex — and `incremental_graph_update`
  (line 1082-1208) needs the equivalent hook to keep
  `golden_graph_equivalence.rs`'s golden==full invariant true. That's real,
  separable feature work against some of the highest-risk, most heavily
  tested code in this repo (`resolve_sites_to_edges` alone carries ~40
  dedicated resolution-edge-case tests) — it deserves its own spec-level
  audit-design pass focused specifically on that pipeline, not a bolt-on to
  this plan. SQL's existing `edge_kind = "reference"` precedent
  (`crates/calm-core/src/indexer/edges.rs::CallEdge` doc comment) proves the
  general shape is reusable; it does not prove the specific ordering/
  incremental-parity work is free.
- **Item 4** (front-matter YAML validation) — needs a new dependency
  (`yaml-rust2`/`saphyr` recommended over `serde_yaml`, per the spec's
  Resolution (d)) with its own supply-chain review, unrelated to the
  link/anchor mechanism this plan builds. Independent subsystem, own plan.

This plan's own deliverable is fully self-contained and useful on its own:
after it ships, `fitness_report` (and `calm fitness-check` in CI) flags a
markdown link whose target file or anchor doesn't exist — closing the
"stale doc claims found only via manual re-verification" gap the spec's
Problem section names, without any of item 3's pipeline risk.

---

## File Structure

- **Create:** `crates/calm-core/src/analysis/doc_links.rs` — slug algorithm,
  link extraction, path/anchor resolution, `check_doc_links` entry point.
  Owns all new logic; nothing else needs to know how a markdown link is
  parsed.
- **Modify:** `crates/calm-core/src/analysis/mod.rs` — register the new
  module.
- **Modify:** `crates/calm-core/src/analysis/doc_refs.rs:23` —
  `strip_fenced_code_blocks` visibility `fn` → `pub(crate) fn` (reused by
  `doc_links.rs`; no behavior change).
- **Modify:** `crates/calm-core/src/analysis/config_drift.rs:125,133` —
  `write`/`temp_project` test helpers visibility `fn` → `pub(crate) fn`
  (reused by `doc_links.rs`'s own tests; no behavior change, test-only).
- **Modify:** `crates/calm-core/src/fitness.rs` — new
  `max_doc_link_count` threshold field + `Default` value, new `doc_links`
  field on `FitnessCheckResult`, new check pushed in `run_fitness_check`.
- **Modify:** `crates/calm-server/src/tools/orient.rs` — new
  `DocLinkFindingOutput` struct + `From` impl, new `doc_links` field on
  `FitnessReportOutput`, wired into the `fitness_report()` method body.
- **Regenerate:** `crates/calm-server/src/__toolsnaps__/fitness_report.snap`
  (auto-derived JSON schema snapshot — regenerated via
  `UPDATE_TOOLSNAPS=1 cargo test`, not hand-edited).

---

### Task 1: GitHub-slug algorithm

**Files:**
- Create: `crates/calm-core/src/analysis/doc_links.rs`
- Modify: `crates/calm-core/src/analysis/mod.rs`

- [ ] **Step 1: Register the module**

  In `crates/calm-core/src/analysis/mod.rs`, insert alphabetically between
  `diff_impact` and `doc_refs`:
  ```rust
  pub mod diff_impact;
  pub mod doc_links;
  pub mod doc_refs;
  ```

- [ ] **Step 2: Write the failing test**

  Create `crates/calm-core/src/analysis/doc_links.rs`:
  ```rust
  use std::collections::HashMap;

  /// GitHub's heading-anchor slug algorithm: lowercase, drop everything
  /// except Unicode-aware alphanumerics/underscore/hyphen/space, then turn
  /// spaces into hyphens. `char::is_alphanumeric()` is Unicode-aware (unlike
  /// ASCII `\w`), so non-Latin heading text (Vietnamese, CJK, ...) keeps its
  /// letters instead of being stripped to nothing — closer to real GitHub
  /// rendering than an ASCII-only filter, though not verified byte-for-byte
  /// against GitHub's own slugger for every script (see the golden-fixture
  /// tests below for the specific cases this project has verified by hand).
  pub fn github_slug(text: &str) -> String {
      let lower = text.to_lowercase();
      let mut out = String::with_capacity(lower.len());
      for c in lower.chars() {
          if c.is_alphanumeric() || c == '_' || c == '-' || c == ' ' {
              out.push(c);
          }
      }
      out.trim().replace(' ', "-")
  }

  /// Applies `github_slug` across all headings in one file, in heading
  /// order, adding GitHub's `-1`/`-2`... suffix on a repeated base slug —
  /// first occurrence keeps the bare slug, second gets `-1`, third `-2`.
  pub fn slug_sequence<'a>(headings: impl Iterator<Item = &'a str>) -> Vec<String> {
      let mut seen: HashMap<String, u32> = HashMap::new();
      headings
          .map(|h| {
              let base = github_slug(h);
              let count = seen.entry(base.clone()).or_insert(0);
              let slug = if *count == 0 {
                  base.clone()
              } else {
                  format!("{base}-{count}")
              };
              *count += 1;
              slug
          })
          .collect()
  }

  #[cfg(test)]
  mod tests {
      use super::*;

      #[test]
      fn slug_basic_words_become_hyphenated_lowercase() {
          assert_eq!(github_slug("Hello World"), "hello-world");
      }

      #[test]
      fn slug_strips_punctuation_but_keeps_words() {
          assert_eq!(github_slug("Getting Started (v2)!"), "getting-started-v2");
      }

      #[test]
      fn slug_preserves_existing_hyphens_and_underscores() {
          assert_eq!(github_slug("Already-Hyphenated"), "already-hyphenated");
          assert_eq!(github_slug("snake_case_heading"), "snake_case_heading");
      }

      #[test]
      fn slug_preserves_non_ascii_letters() {
          // Vietnamese: verified by hand against GitHub's real rendering —
          // diacritics and non-Latin letters are kept, not stripped.
          assert_eq!(github_slug("Cài đặt"), "cài-đặt");
      }

      #[test]
      fn slug_sequence_dedups_repeated_headings_with_numeric_suffix() {
          let headings = vec!["Setup", "Config", "Setup", "Setup"];
          assert_eq!(
              slug_sequence(headings.into_iter()),
              vec!["setup", "config", "setup-1", "setup-2"]
          );
      }

      #[test]
      fn slug_sequence_case_variants_collide_into_same_base_before_dedup() {
          // "Setup" and "setup" both slug to "setup" — GitHub treats them as
          // the same anchor for dedup purposes (case-insensitive collision),
          // which this project's original audit-design pass flagged as an
          // untested branch (L1 Logic). Verified here: it dedups correctly.
          let headings = vec!["Setup", "setup"];
          assert_eq!(slug_sequence(headings.into_iter()), vec!["setup", "setup-1"]);
      }
  }
  ```

- [ ] **Step 2: Run — verify FAIL**
  `cargo test -p calm-core doc_links::` → expected: FAIL (module doesn't
  compile into anything visible yet — trivial, this is the first write, so
  treat "compiles and all 6 pass" as the actual target of Step 3 below since
  there's no pre-existing red state to observe here beyond "module didn't
  exist".)

- [ ] **Step 3: Run — verify PASS**
  `cargo test -p calm-core doc_links::` → expected: `6 passed; 0 failed`

- [ ] **Step 4: Commit**
  `git commit -m "feat(doc-links): add GitHub-slug anchor algorithm"`

---

### Task 2: Markdown link extraction (fence-aware, URL-skipping)

**Files:**
- Modify: `crates/calm-core/src/analysis/doc_refs.rs:23`
- Modify: `crates/calm-core/src/analysis/doc_links.rs`

- [ ] **Step 1: Loosen visibility on the reused fence-stripper**

  In `crates/calm-core/src/analysis/doc_refs.rs`, change:
  ```rust
  fn strip_fenced_code_blocks(text: &str) -> String {
  ```
  to:
  ```rust
  pub(crate) fn strip_fenced_code_blocks(text: &str) -> String {
  ```
  (Same function, only the visibility keyword changes — reused by
  `doc_links.rs` instead of duplicating the fence-tracking loop.)

- [ ] **Step 2: Write the failing test**

  Append to `crates/calm-core/src/analysis/doc_links.rs` (above the existing
  `#[cfg(test)] mod tests` block, inside the main module body):
  ```rust
  use regex::Regex;
  use std::sync::OnceLock;

  fn link_regex() -> &'static Regex {
      static RE: OnceLock<Regex> = OnceLock::new();
      RE.get_or_init(|| Regex::new(r"\[[^\]]*\]\(([^)]+)\)").unwrap())
  }

  /// Fence-aware scan for markdown links `[text](target)`, returning
  /// `(1-indexed line, raw target)` pairs. Skips external URLs
  /// (`http(s)://`, `mailto:`, protocol-relative `//`) — those aren't
  /// repo-relative references this check can resolve. Line-scoped, like
  /// `extract_markdown_symbols` — a link spanning multiple lines (rare,
  /// non-idiomatic markdown) is not matched, matching that function's own
  /// per-line design instead of adding a second scanning strategy.
  pub fn extract_markdown_links(text: &str) -> Vec<(usize, String)> {
      let stripped = crate::analysis::doc_refs::strip_fenced_code_blocks(text);
      let re = link_regex();
      let mut out = Vec::new();
      for (idx, line) in stripped.lines().enumerate() {
          for cap in re.captures_iter(line) {
              let target = cap.get(1).map(|m| m.as_str().trim()).unwrap_or("");
              if target.is_empty() {
                  continue;
              }
              if target.starts_with("http://")
                  || target.starts_with("https://")
                  || target.starts_with("mailto:")
                  || target.starts_with("//")
              {
                  continue;
              }
              out.push((idx + 1, target.to_string()));
          }
      }
      out
  }

  /// Splits a link target into `(path_part, fragment)`: `path#frag` → both
  /// set, `#frag`-only → `path_part: None`, bare `path` → `fragment: None`.
  pub fn split_link_target(raw: &str) -> (Option<String>, Option<String>) {
      let raw = raw.trim();
      match raw.find('#') {
          Some(idx) => {
              let path = &raw[..idx];
              let frag = &raw[idx + 1..];
              let path_part = if path.is_empty() {
                  None
              } else {
                  Some(path.to_string())
              };
              (path_part, Some(frag.to_string()))
          }
          None => (Some(raw.to_string()), None),
      }
  }
  ```

  Add to the `mod tests` block:
  ```rust
  #[test]
  fn extract_markdown_links_captures_target_and_line() {
      let text = "See [the guide](docs/guide.md) for details.\n";
      let links = extract_markdown_links(text);
      assert_eq!(links, vec![(1, "docs/guide.md".to_string())]);
  }

  #[test]
  fn extract_markdown_links_skips_fenced_code_blocks() {
      let text = "```md\n[not a real link](fake.md)\n```\n[real](real.md)\n";
      let links = extract_markdown_links(text);
      assert_eq!(links, vec![(4, "real.md".to_string())]);
  }

  #[test]
  fn extract_markdown_links_skips_external_urls() {
      let text = "[site](https://example.com/x) [mail](mailto:a@b.com)\n";
      assert!(extract_markdown_links(text).is_empty());
  }

  #[test]
  fn split_link_target_path_and_fragment() {
      assert_eq!(
          split_link_target("docs/guide.md#setup"),
          (Some("docs/guide.md".to_string()), Some("setup".to_string()))
      );
  }

  #[test]
  fn split_link_target_fragment_only() {
      assert_eq!(
          split_link_target("#setup"),
          (None, Some("setup".to_string()))
      );
  }

  #[test]
  fn split_link_target_path_only() {
      assert_eq!(
          split_link_target("docs/guide.md"),
          (Some("docs/guide.md".to_string()), None)
      );
  }
  ```

- [ ] **Step 3: Run — verify FAIL then PASS**
  `cargo test -p calm-core doc_links::` → write Step 2's code first (it will
  fail to compile without it), then verify: expected `12 passed; 0 failed`.

- [ ] **Step 4: Commit**
  `git commit -m "feat(doc-links): extract markdown link targets, fence-aware"`

---

### Task 3: Path resolution — relative-to-file first, clamped to `project_root`

This is the fix for the audit's verified finding: `config_drift::resolve_reference`
has no "relative to the referring file's directory" mode, so `../sibling.md`
false-positives and `./sibling.md` can false-negative-match the wrong file
elsewhere in the repo. This task adds the missing resolution mode as a new,
separate function — `resolve_reference` itself is not modified.

**Files:**
- Modify: `crates/calm-core/src/analysis/config_drift.rs:125,133`
- Modify: `crates/calm-core/src/analysis/doc_links.rs`

- [ ] **Step 1: Loosen visibility on reused test helpers**

  In `crates/calm-core/src/analysis/config_drift.rs`, change:
  ```rust
  fn write(dir: &Path, rel: &str, content: &str) {
  ```
  and
  ```rust
  fn temp_project(name: &str) -> std::path::PathBuf {
  ```
  to `pub(crate) fn` for both (test-only helpers, reused by
  `doc_links.rs`'s tests instead of duplicating them).

- [ ] **Step 2: Write the failing test**

  Add to `crates/calm-core/src/analysis/doc_links.rs` main module body:
  ```rust
  use std::collections::HashSet;
  use std::path::{Component, Path};

  /// Lexically joins `base` (a directory, relative to `project_root`) with
  /// `rel` (which may contain `./`/`../`), normalizing `.`/`..` components
  /// WITHOUT touching the filesystem (the target may not exist — that's
  /// exactly the case this function has to detect). Returns `None` if the
  /// result would walk above `project_root` — the clamp the original
  /// audit-design pass's L5 Security finding required, since an unclamped
  /// `../../../` chain could otherwise resolve outside the repo tree.
  fn normalize_within_root(base: &Path, rel: &str) -> Option<String> {
      let mut parts: Vec<String> = base
          .components()
          .filter_map(|c| match c {
              Component::Normal(s) => Some(s.to_string_lossy().replace('\\', "/")),
              _ => None,
          })
          .collect();
      for comp in Path::new(rel).components() {
          match comp {
              Component::Normal(s) => parts.push(s.to_string_lossy().replace('\\', "/")),
              Component::ParentDir => {
                  parts.pop()?;
              }
              Component::CurDir => {}
              _ => {}
          }
      }
      Some(parts.join("/"))
  }

  /// Resolves a markdown link's path part. Tries relative-to-the-referring-
  /// file's-directory FIRST (the common, correct interpretation of a
  /// markdown `./`/`../`/bare-sibling link) — falling back to
  /// `config_drift::resolve_reference`'s repo-root/suffix logic only when
  /// the relative form doesn't hit a real file, for legacy bare-mention-
  /// shaped links. Reuses `build_real_path_index`'s output (the real-path
  /// universe); does not reuse `resolve_reference` as the primary strategy,
  /// per the spec's Resolution (c) finding that it has no relative-to-file
  /// mode at all.
  pub fn resolve_markdown_link_path(
      project_root: &Path,
      referring_doc: &str,
      real_paths: &HashSet<String>,
      raw_path: &str,
  ) -> Option<String> {
      let referring_dir = Path::new(referring_doc).parent().unwrap_or(Path::new(""));
      if let Some(normalized) = normalize_within_root(referring_dir, raw_path)
          && real_paths.contains(&normalized)
      {
          return Some(normalized);
      }
      crate::analysis::config_drift::resolve_reference(project_root, real_paths, raw_path)
  }

  #[cfg(test)]
  mod resolve_tests {
      use super::*;
      use crate::analysis::config_drift::{build_real_path_index, temp_project, write};

      #[test]
      fn resolves_relative_sibling_link() {
          let dir = temp_project("doc_links_sibling");
          write(&dir, "docs/a.md", "# A\n");
          write(&dir, "docs/b.md", "# B\n");
          let real_paths = build_real_path_index(&dir, &[]);
          let resolved =
              resolve_markdown_link_path(&dir, "docs/a.md", &real_paths, "./b.md");
          assert_eq!(resolved, Some("docs/b.md".to_string()));
      }

      #[test]
      fn resolves_relative_parent_dir_link() {
          let dir = temp_project("doc_links_parent");
          write(&dir, "docs/sub/a.md", "# A\n");
          write(&dir, "docs/root.md", "# Root\n");
          let real_paths = build_real_path_index(&dir, &[]);
          let resolved = resolve_markdown_link_path(
              &dir,
              "docs/sub/a.md",
              &real_paths,
              "../root.md",
          );
          assert_eq!(resolved, Some("docs/root.md".to_string()));
      }

      #[test]
      fn clamps_path_traversal_above_project_root() {
          let dir = temp_project("doc_links_clamp");
          write(&dir, "docs/a.md", "# A\n");
          let real_paths = build_real_path_index(&dir, &[]);
          // Walks above project_root — must not panic, must not resolve.
          let resolved = resolve_markdown_link_path(
              &dir,
              "docs/a.md",
              &real_paths,
              "../../../../etc/passwd",
          );
          assert_eq!(resolved, None);
      }

      #[test]
      fn falls_back_to_repo_root_resolution_for_bare_legacy_mention() {
          let dir = temp_project("doc_links_fallback");
          write(&dir, "docs/a.md", "# A\n");
          write(&dir, "README.md", "# Root\n");
          let real_paths = build_real_path_index(&dir, &[]);
          // "README.md" from docs/a.md isn't a real relative sibling
          // (docs/README.md doesn't exist) — falls back to repo-root match.
          let resolved =
              resolve_markdown_link_path(&dir, "docs/a.md", &real_paths, "README.md");
          assert_eq!(resolved, Some("README.md".to_string()));
      }

      #[test]
      fn returns_none_for_target_that_does_not_exist_anywhere() {
          let dir = temp_project("doc_links_missing");
          write(&dir, "docs/a.md", "# A\n");
          let real_paths = build_real_path_index(&dir, &[]);
          let resolved =
              resolve_markdown_link_path(&dir, "docs/a.md", &real_paths, "./nope.md");
          assert_eq!(resolved, None);
      }
  }
  ```

- [ ] **Step 3: Run — verify PASS**
  `cargo test -p calm-core doc_links::` → expected: `17 passed; 0 failed`

- [ ] **Step 4: Commit**
  `git commit -m "feat(doc-links): relative-to-file link resolution, clamped to project_root"`

---

### Task 4: End-to-end `check_doc_links`

**Files:**
- Modify: `crates/calm-core/src/analysis/doc_links.rs`

- [ ] **Step 1: Write the failing test**

  Add to `crates/calm-core/src/analysis/doc_links.rs` main module body:
  ```rust
  /// One markdown link/anchor that doesn't resolve — either the target file
  /// itself doesn't exist (`reason: "broken_path"`) or it exists but the
  /// `#fragment` doesn't match any heading-derived anchor in it
  /// (`reason: "broken_anchor"`).
  #[derive(Debug, Clone, PartialEq, Eq)]
  pub struct DocLinkFinding {
      pub doc_path: String,
      pub line: usize,
      pub raw_target: String,
      pub reason: String,
  }

  fn headings_in_file(project_root: &Path, target_path: &str) -> Vec<String> {
      let full = project_root.join(target_path);
      let Ok(text) = std::fs::read_to_string(&full) else {
          return Vec::new();
      };
      let symbols = crate::indexer::parser::extract_markdown_symbols(&text, target_path);
      slug_sequence(symbols.iter().map(|s| s.name.as_str()))
  }

  /// Checks every markdown link inside `doc_paths` for a resolvable target
  /// file and (if a `#fragment` is present) a matching heading anchor in
  /// that target — the `config_drift`-shaped, on-demand check this spec's
  /// items 1-2 exist to deliver. Reuses the same `doc_paths` config key
  /// `check_config_drift` already reads (`[config_drift].doc_paths`) rather
  /// than introducing a second, overlapping config list.
  pub fn check_doc_links(
      project_root: &Path,
      doc_paths: &[String],
      ignore_patterns: &[String],
  ) -> Vec<DocLinkFinding> {
      if doc_paths.is_empty() {
          return Vec::new();
      }
      let real_paths = crate::analysis::config_drift::build_real_path_index(
          project_root,
          ignore_patterns,
      );
      let mut findings = Vec::new();
      for doc_path in doc_paths {
          let full = project_root.join(doc_path);
          let Ok(text) = std::fs::read_to_string(&full) else {
              continue;
          };
          for (line, raw_target) in extract_markdown_links(&text) {
              let (path_part, fragment) = split_link_target(&raw_target);
              let target_path = match path_part {
                  None => doc_path.clone(),
                  Some(p) => {
                      match resolve_markdown_link_path(project_root, doc_path, &real_paths, &p) {
                          Some(resolved) => resolved,
                          None => {
                              findings.push(DocLinkFinding {
                                  doc_path: doc_path.clone(),
                                  line,
                                  raw_target,
                                  reason: "broken_path".to_string(),
                              });
                              continue;
                          }
                      }
                  }
              };
              if let Some(frag) = fragment {
                  let anchors = headings_in_file(project_root, &target_path);
                  if !anchors.iter().any(|a| a == &frag) {
                      findings.push(DocLinkFinding {
                          doc_path: doc_path.clone(),
                          line,
                          raw_target,
                          reason: "broken_anchor".to_string(),
                      });
                  }
              }
          }
      }
      findings.sort_by(|a, b| a.doc_path.cmp(&b.doc_path).then(a.line.cmp(&b.line)));
      findings
  }

  #[cfg(test)]
  mod check_tests {
      use super::*;
      use crate::analysis::config_drift::{temp_project, write};

      #[test]
      fn empty_doc_paths_returns_no_findings() {
          let dir = temp_project("doc_links_check_empty");
          assert!(check_doc_links(&dir, &[], &[]).is_empty());
      }

      #[test]
      fn flags_link_to_nonexistent_file() {
          let dir = temp_project("doc_links_check_broken_path");
          write(&dir, "docs/a.md", "[missing](./nope.md)\n");
          let findings = check_doc_links(&dir, &["docs/a.md".to_string()], &[]);
          assert_eq!(findings.len(), 1);
          assert_eq!(findings[0].reason, "broken_path");
      }

      #[test]
      fn flags_link_to_real_file_but_missing_anchor() {
          let dir = temp_project("doc_links_check_broken_anchor");
          write(&dir, "docs/a.md", "[setup](./b.md#setup)\n");
          write(&dir, "docs/b.md", "# Installation\n");
          let findings = check_doc_links(&dir, &["docs/a.md".to_string()], &[]);
          assert_eq!(findings.len(), 1);
          assert_eq!(findings[0].reason, "broken_anchor");
      }

      #[test]
      fn passes_when_target_file_and_anchor_both_exist() {
          let dir = temp_project("doc_links_check_ok");
          write(&dir, "docs/a.md", "[setup](./b.md#installation)\n");
          write(&dir, "docs/b.md", "# Installation\n");
          let findings = check_doc_links(&dir, &["docs/a.md".to_string()], &[]);
          assert!(findings.is_empty());
      }

      #[test]
      fn passes_for_same_document_fragment_only_link() {
          let dir = temp_project("doc_links_check_same_doc");
          write(&dir, "docs/a.md", "# Setup\n\nSee [above](#setup)\n");
          let findings = check_doc_links(&dir, &["docs/a.md".to_string()], &[]);
          assert!(findings.is_empty());
      }
  }
  ```

- [ ] **Step 2: Run — verify PASS**
  `cargo test -p calm-core doc_links::` → expected: `22 passed; 0 failed`

- [ ] **Step 3: Commit**
  `git commit -m "feat(doc-links): end-to-end check_doc_links"`

---

### Task 5: Wire into `run_fitness_check`

**Files:**
- Modify: `crates/calm-core/src/fitness.rs`

- [ ] **Step 1: Write the failing test**

  Add near the other `FitnessThresholds`-based tests in
  `crates/calm-core/src/fitness.rs` (same file, `#[cfg(test)] mod tests`
  block — follow the existing pattern at line ~813 for a temp-dir-backed
  fitness check):
  ```rust
  #[test]
  fn doc_link_count_reflects_broken_markdown_link() {
      let dir = std::env::temp_dir().join(format!(
          "calm_fitness_doc_links_{}",
          std::process::id()
      ));
      std::fs::create_dir_all(dir.join("docs")).unwrap();
      std::fs::write(dir.join("docs/a.md"), "[missing](./nope.md)\n").unwrap();
      let conn = Connection::open_in_memory().unwrap();
      crate::db::schema::init_db(&conn).unwrap();

      let thresholds = FitnessThresholds {
          max_doc_link_count: 0,
          ..Default::default()
      };
      let result = run_fitness_check(
          &conn,
          &thresholds,
          &dir,
          &CoverageData::default(),
          &[],
          &["docs/a.md".to_string()],
      )
      .unwrap();

      assert_eq!(result.doc_links.len(), 1);
      assert!(!result.passed);
      std::fs::remove_dir_all(&dir).ok();
  }
  ```
  (If `CoverageData::default()` isn't the exact constructor other tests in
  this file use, match whichever helper the nearest existing
  `run_fitness_check(...)` call in this file already uses instead — same
  call shape, just this test's own fixture data.)

- [ ] **Step 2: Run — verify FAIL**
  `cargo test -p calm-core fitness:: doc_link_count_reflects_broken_markdown_link`
  → expected: FAIL (`max_doc_link_count` field doesn't exist yet)

- [ ] **Step 3: Add the threshold field**

  In `crates/calm-core/src/fitness.rs`, add to `FitnessThresholds` (after
  `max_config_drift_count` at line 60, before `max_boundary_ambiguous_count`):
  ```rust
      /// Max allowed count of markdown links/anchors inside declared
      /// `[config_drift].doc_paths` docs whose target file or `#fragment`
      /// doesn't resolve (see `analysis::doc_links::check_doc_links`).
      /// Default 0, same reasoning as `max_config_drift_count` — reuses the
      /// same `doc_paths` config key rather than a second, overlapping list.
      pub max_doc_link_count: i64,
  ```

  Add to the `Default` impl (after `max_config_drift_count: 0,` at line 92):
  ```rust
          max_doc_link_count: 0,
  ```

- [ ] **Step 4: Add the result field and wire the check**

  In `crates/calm-core/src/fitness.rs`, add the import near the top:
  ```rust
  use crate::analysis::doc_links::{DocLinkFinding, check_doc_links};
  ```

  Add to `FitnessCheckResult` (after `config_drift: Vec<ConfigDriftFinding>,`
  at line 438):
  ```rust
      /// Full detail behind the `doc_link_count` check — empty whenever
      /// that check passes (including when no `doc_paths` are declared).
      pub doc_links: Vec<DocLinkFinding>,
  ```

  In `run_fitness_check` (line 441-591), add right after the existing
  `config_drift` computation (line 454):
  ```rust
      let doc_links = check_doc_links(project_root, config_drift_doc_paths, &ignore_patterns);
  ```

  Add a new `FitnessCheckItem` push right after the `config_drift_count`
  block (after line 581, before `let passed = ...`):
  ```rust
      checks.push(FitnessCheckItem {
          metric: "doc_link_count".into(),
          value: doc_links.len() as f64,
          threshold: thresholds.max_doc_link_count as f64,
          passed: doc_links.len() as i64 <= thresholds.max_doc_link_count,
          message: format!(
              "Broken markdown links/anchors {} (max {}){}",
              doc_links.len(),
              thresholds.max_doc_link_count,
              if config_drift_doc_paths.is_empty() {
                  " — no config_drift.doc_paths declared"
              } else {
                  ""
              }
          ),
      });
  ```

  Update the `Ok(FitnessCheckResult { ... })` construction (line 584-590) to
  include the new field:
  ```rust
      Ok(FitnessCheckResult {
          passed,
          checks,
          metrics,
          boundary_violations,
          config_drift,
          doc_links,
      })
  ```

- [ ] **Step 5: Run — verify PASS**
  `cargo test -p calm-core fitness::` → expected: all fitness.rs tests pass,
  including the new `doc_link_count_reflects_broken_markdown_link`.

- [ ] **Step 6: Commit**
  `git commit -m "feat(fitness): wire doc-link/anchor check into run_fitness_check"`

---

### Task 6: Surface in the `fitness_report` MCP tool

**Files:**
- Modify: `crates/calm-server/src/tools/orient.rs`
- Regenerate: `crates/calm-server/src/__toolsnaps__/fitness_report.snap`

- [ ] **Step 1: Write the failing test**

  Add near the other `fitness_report`-adjacent tests in
  `crates/calm-server/src/tools.rs` (`#[cfg(test)] mod tests`, matching the
  file's existing test style for this tool):
  ```rust
  #[test]
  fn fitness_report_surfaces_doc_link_findings() {
      let server = test_server_with_thresholds_toml(
          "[config_drift]\ndoc_paths = [\"docs/a.md\"]\n",
      );
      std::fs::create_dir_all(server.project_root.join("docs")).unwrap();
      std::fs::write(
          server.project_root.join("docs/a.md"),
          "[missing](./nope.md)\n",
      )
      .unwrap();

      let Json(outcome) = server.fitness_report();
      let output = outcome.data.expect("fitness_report should succeed");
      assert_eq!(output.doc_links.len(), 1);
  }
  ```
  (If this file doesn't already have a `test_server_with_thresholds_toml`-
  style helper, use whichever existing helper the nearest
  `config_drift`-surfacing `fitness_report` test in this same file already
  uses — match that exact fixture-construction pattern instead of
  introducing a new one.)

- [ ] **Step 2: Run — verify FAIL**
  `cargo test -p calm-server fitness_report_surfaces_doc_link_findings` →
  expected: FAIL (`doc_links` field doesn't exist on `FitnessReportOutput`
  yet)

- [ ] **Step 3: Add the output type and wire it**

  In `crates/calm-server/src/tools/orient.rs`, add after
  `ConfigDriftFindingOutput`'s `From` impl (after line 506):
  ```rust
  #[derive(Serialize, JsonSchema)]
  pub(crate) struct DocLinkFindingOutput {
      pub(crate) doc_path: String,
      pub(crate) line: usize,
      pub(crate) raw_target: String,
      pub(crate) reason: String,
  }

  impl From<calm_core::analysis::doc_links::DocLinkFinding> for DocLinkFindingOutput {
      fn from(f: calm_core::analysis::doc_links::DocLinkFinding) -> Self {
          Self {
              doc_path: f.doc_path,
              line: f.line,
              raw_target: f.raw_target,
              reason: f.reason,
          }
      }
  }
  ```

  Add to `FitnessReportOutput` (after `config_drift` at line 516):
  ```rust
      #[serde(skip_serializing_if = "Vec::is_empty", default)]
      pub(crate) doc_links: Vec<DocLinkFindingOutput>,
  ```

  In the `fitness_report()` method body, update the
  `ToolOutcome::success(FitnessReportOutput { ... })` construction (line
  406-417) to include:
  ```rust
              doc_links: result.doc_links.into_iter().map(Into::into).collect(),
  ```
  (added alongside the existing `config_drift: ...` line, before
  `suggested_next`).

- [ ] **Step 4: Run — verify PASS**
  `cargo test -p calm-server fitness_report_surfaces_doc_link_findings` →
  expected: PASS

- [ ] **Step 5: Regenerate the tool schema snapshot**
  ```
  UPDATE_TOOLSNAPS=1 cargo test -p calm-server tool_schemas_match_committed_snapshots
  ```
  → expected: exits 0, and `git diff --stat` shows only
  `crates/calm-server/src/__toolsnaps__/fitness_report.snap` changed (a new
  `DocLinkFindingOutput` `$defs` entry + a `doc_links` property — same shape
  as `ConfigDriftFindingOutput`'s existing entry).

- [ ] **Step 6: Run the full workspace test suite**
  `cargo test --workspace` → expected: all tests pass, 0 failures (this is
  the mandatory full-suite check before the final commit — matches this
  project's own practice of never committing on a partial test run).

- [ ] **Step 7: Commit**
  `git commit -m "feat(fitness_report): surface doc-link/anchor findings, regen toolsnap"`

---

## Self-Review

**1. Spec coverage.** Spec items 1-2 (heading anchors, link/anchor
resolution wired into `fitness_report`, on-demand) — fully covered, Tasks
1-6. Item 3 (index-time edges, `diff_impact` field) and item 4 (front-matter
YAML) — explicitly out of scope, see Scope Decision; each needs its own spec
revision / audit-design pass before a plan is written for it, not silently
dropped.

**2. Placeholder scan.** No "TBD"/"similar to Task N"/prose-only steps —
every code step above has complete, compilable-as-written Rust. The two
"if this file doesn't already have X helper, match the nearest existing
Y" notes (Tasks 5 and 6) are not placeholders for missing logic — they're
an explicit instruction to match this repo's own existing test-fixture
convention in files this plan's author didn't have byte-exact visibility
into test-helper-wise, rather than inventing a second, divergent fixture
style. The check_doc_links/resolve_markdown_link_path/extract_markdown_links
implementations themselves are complete.

**3. Type consistency.** `DocLinkFinding` (doc_path, line, raw_target,
reason: String) defined once in Task 4, consumed unchanged by Task 5
(`FitnessCheckResult.doc_links: Vec<DocLinkFinding>`) and Task 6
(`DocLinkFindingOutput::from`). `check_doc_links(project_root, doc_paths,
ignore_patterns)` signature matches its one call site in Task 5 exactly.
`resolve_markdown_link_path`'s signature (Task 3) matches its one call site
inside `check_doc_links` (Task 4) exactly.

**4. Risk scoring.** See Risk Summary below.

## Risk Summary

| Task | Description | Risk | Notes |
|---|---|---|---|
| 1 | GitHub-slug algorithm | LOW | Pure function, new file, no existing callers to break. |
| 2 | Link extraction | LOW | New code path; `strip_fenced_code_blocks` visibility widen only (private→`pub(crate)`, same crate). |
| 3 | Path resolution + traversal clamp | MEDIUM | Security-relevant (path clamp) but fully unit-tested including the escape case; touches no existing function's behavior, only adds a new one. `config_drift.rs` test-helper visibility widen only. |
| 4 | `check_doc_links` | LOW | Pure composition of Tasks 1-3; no existing caller. |
| 5 | `fitness.rs` wiring | MEDIUM | Modifies a real, existing, tested function (`run_fitness_check`) and a public struct (`FitnessThresholds`) — additive fields only (no existing field renamed/removed), but `FitnessCheckResult`'s field list changing is a breaking change for any other in-crate caller constructing it by name (checked: `run_fitness_check` is the only constructor). |
| 6 | `fitness_report` MCP surface + toolsnap | MEDIUM | Touches the hook-adjacent tool-schema-snapshot mechanism directly — CROSS boundary (calm-core → calm-server). Handoff: Task 6 cannot start until Task 5's `FitnessCheckResult.doc_links` field exists and compiles; toolsnap regeneration (Step 5) must run and be committed in the same task, never left for a later cleanup, or CI's `tool_schemas_match_committed_snapshots` fails on the next unrelated PR. |

No HIGH tasks. CROSS boundary: Task 6 depends on Task 5's output type
(`calm-core` → `calm-server`), called out explicitly above with its handoff
condition.

---

Plan complete: `docs/superskills/plans/2026-07-16-calm-markdown-link-anchor-integrity.md`
Risk summary: 0 HIGH tasks, 1 CROSS boundary (Task 5 → Task 6, handoff noted above)

Execution options:
1. Subagent-Driven (recommended) — fresh subagent per task, specialist-review between tasks
2. Inline Execution — batch execution with checkpoints

Which approach?
