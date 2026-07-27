use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};

/// One commit's author, ISO-8601 author date, and the files it touched
/// (`--name-only`) — the shared unit `hotspot::collect_git_churn`,
/// `cochange::compute_co_changes`, and `ownership_entropy` all fold over,
/// so the `git log` call and its `|||author|||date` marker-line parsing
/// exist in exactly one place.
#[derive(Debug, Clone)]
pub struct GitCommit {
    pub author: Option<String>,
    pub date: Option<String>,
    pub files: Vec<String>,
}

/// Runs `git log --since=<since> --name-only` and groups the output into
/// one `GitCommit` per commit. Returns `(commits, git_available)` —
/// `git_available: false` when git isn't present or this isn't a git repo
/// (not an error to propagate; callers degrade gracefully, same as
/// `hotspot`'s existing fallback).
/// Hard wall-clock budget for `commits_with_files`'s `git log` subprocess.
/// Measured against a synthetic 60,000-commit repo with realistic activity
/// density (git fast-import, timestamps spanning the actual 6-month
/// `default_since` window, ~2000 distinct files) as part of the 2026-07-27
/// martin/entropy/churn plan's Abductive Hypothesis 2 gate: cold cost was
/// 9.3-9.7s wall time (peak RSS stayed a modest ~50MB -- memory was never
/// the real risk, latency was). That number matters here specifically
/// because this function's cached wrapper is now on the mandatory,
/// per-edit `edit_context` path (ownership entropy) in addition to its
/// original advisory-only hotspot/cochange callers -- an unbounded git-log
/// call blocking every edit on a large, very active repo is not acceptable
/// even though it was already technically possible (and unguarded) before
/// this plan. 5s leaves comfortable headroom over every repo shape
/// measured here while still bounding the worst case; a timeout degrades
/// exactly like "git unavailable" (`git_available: false`) -- honest
/// (\"couldn't get this signal within budget\"), not a fabricated empty
/// answer presented as ground truth.
const GIT_LOG_TIMEOUT: Duration = Duration::from_secs(5);

/// Runs `git log --since=<since> --name-only` and groups the output into
/// one `GitCommit` per commit. Returns `(commits, git_available)` —
/// `git_available: false` when git isn't present, this isn't a git repo,
/// or the subprocess didn't finish within `GIT_LOG_TIMEOUT` (not an error
/// to propagate; callers degrade gracefully, same as `hotspot`'s existing
/// fallback). On timeout the child process is left to finish on its own in
/// the background (git log is read-only, so an abandoned wait is safe) —
/// only this call's *wait* is bounded, not the OS process itself.
pub fn commits_with_files(project_root: &Path, since: &str) -> (Vec<GitCommit>, bool) {
    let child = match Command::new("git")
        .args([
            "log",
            &format!("--since={since}"),
            "--name-only",
            "--format=|||%ae|||%aI",
        ])
        .current_dir(project_root)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
    {
        Ok(c) => c,
        Err(_) => return (Vec::new(), false),
    };

    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        // `wait_with_output` reads stdout to completion internally before
        // waiting, so a large output can never deadlock against a full
        // pipe buffer the way a naive take()+wait() split could.
        let result = child.wait_with_output();
        let _ = tx.send(result);
    });

    let output = match rx.recv_timeout(GIT_LOG_TIMEOUT) {
        Ok(Ok(o)) if o.status.success() => o,
        _ => return (Vec::new(), false),
    };

    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut commits: Vec<GitCommit> = Vec::new();

    for line in stdout.lines() {
        if line.starts_with("|||") {
            let parts: Vec<&str> = line.split("|||").collect();
            commits.push(GitCommit {
                author: parts.get(1).map(|s| s.trim().to_string()),
                date: parts.get(2).map(|s| s.trim().to_string()),
                files: Vec::new(),
            });
        } else if !line.trim().is_empty()
            && let Some(commit) = commits.last_mut()
        {
            commit.files.push(line.trim().to_string());
        }
    }

    (commits, true)
}

const GIT_SIGNAL_CACHE_TTL: Duration = Duration::from_secs(60);

type GitSignalCacheKey = (PathBuf, String);
type GitSignalCacheEntry = (GitSignalCacheKey, Instant, Arc<Vec<GitCommit>>, bool);

static GIT_SIGNAL_CACHE: RwLock<Option<GitSignalCacheEntry>> = RwLock::new(None);

/// Cached `commits_with_files` (2026-07-27 Phase 0, martin/entropy/churn
/// plan): `cochange::compute_co_changes`, `hotspot::collect_git_churn`, and
/// `ownership_entropy` all fold over the same repo-wide `git log` pass —
/// before this, each ran (or, for cochange, cached at the server layer
/// under its own key) an independent subprocess. A single process-wide
/// cache keyed on `(project_root, since)` means all consumers read the
/// exact same commit set within one TTL window, so they can never disagree
/// about a file's churn/authorship — by construction, not by convention.
/// TTL mirrors `CalmServer::co_changes_cached`'s `CO_CHANGE_CACHE_TTL`
/// (60s): git history only changes on a new commit, so a short TTL is
/// plenty fresh. Returns `Arc` rather than cloning: a large history (this
/// is O(commits) in memory) should be paid for once per TTL window, not
/// once per consumer per call — and a read-lock holder must never clone a
/// multi-MB `Vec` while holding the lock, which would serialize every
/// caller (including the mandatory-per-edit `edit_context` path) behind
/// one slow clone.
pub fn commits_with_files_cached(project_root: &Path, since: &str) -> (Arc<Vec<GitCommit>>, bool) {
    commits_with_files_cached_in(&GIT_SIGNAL_CACHE, project_root, since)
}

/// Same logic as `commits_with_files_cached`, but takes the cache slot as a
/// parameter instead of reaching for the process-wide `static`. Exists so
/// tests can pass in their own private `RwLock`, isolated from every other
/// test in this binary — the real cache is intentionally a single
/// process-wide slot (there is realistically one `(project_root, since)`
/// pair in play per daemon), which under `cargo test`'s default parallelism
/// means two unrelated tests calling the `static`-backed version race on
/// the same slot and can evict each other's entry. That's harmless in
/// production (a miss just recomputes) but makes "does it actually stay
/// cached within the TTL" unverifiable against the shared static.
fn commits_with_files_cached_in(
    cache: &RwLock<Option<GitSignalCacheEntry>>,
    project_root: &Path,
    since: &str,
) -> (Arc<Vec<GitCommit>>, bool) {
    let key = (project_root.to_path_buf(), since.to_string());
    if let Ok(guard) = cache.read()
        && let Some((cached_key, at, commits, git_available)) = guard.as_ref()
        && *cached_key == key
        && at.elapsed() < GIT_SIGNAL_CACHE_TTL
    {
        return (commits.clone(), *git_available);
    }
    let (commits, git_available) = commits_with_files(project_root, since);
    let commits = Arc::new(commits);
    if let Ok(mut guard) = cache.write() {
        *guard = Some((key, Instant::now(), commits.clone(), git_available));
    }
    (commits, git_available)
}

/// Per-file signals derived from a shared `commits_with_files` pass: commit
/// count, per-author commit counts (the raw input `ownership_entropy`
/// needs), and the most recent commit date. One derivation feeds every
/// consumer (`hotspot::ChurnInfo`, `ownership_entropy`, `graph::churn`) so
/// none of them can compute a different answer for the same file.
#[derive(Debug, Clone, Default)]
pub struct FileGitSignals {
    pub commit_count: u32,
    pub author_commits: HashMap<String, u32>,
    pub last_changed: Option<String>,
}

/// Folds a commit list (newest-first, per `git log`'s default order) into
/// per-file signals. `or_insert_with` only fires on a file's first
/// occurrence, so `last_changed` naturally ends up as the most recent
/// commit's date — the same trick `hotspot::collect_git_churn` used
/// before this was centralized here.
pub fn file_signals(commits: &[GitCommit]) -> HashMap<String, FileGitSignals> {
    let mut map: HashMap<String, FileGitSignals> = HashMap::new();
    for commit in commits {
        for path in &commit.files {
            let entry = map.entry(path.clone()).or_insert_with(|| FileGitSignals {
                commit_count: 0,
                author_commits: HashMap::new(),
                last_changed: commit.date.clone(),
            });
            entry.commit_count += 1;
            if let Some(author) = &commit.author {
                *entry.author_commits.entry(author.clone()).or_insert(0) += 1;
            }
        }
    }
    map
}

/// Shannon entropy of a file's per-author commit distribution, normalized
/// to `[0, 1]` by `ln(distinct_authors)` so files with different author
/// counts stay comparable (a 2-author 50/50 split and a 5-author uniform
/// split both read as `1.0` — maximally distributed for their own author
/// count).
///
/// `None` when `commit_count < min_commits` — mirrors the
/// `default_min_churn` floor `hotspot.rs` already applies, since a file
/// touched once has no meaningful ownership distribution yet, just a
/// single data point.
///
/// A single distinct author yields `Some(0.0)` (maximally concentrated),
/// *not* `None` — this is deliberate: "one author, several commits" is
/// exactly the low-bus-factor signal callers need to detect, and
/// collapsing it to `None` would erase it.
pub fn ownership_entropy(signals: &FileGitSignals, min_commits: u32) -> Option<f64> {
    if signals.commit_count < min_commits {
        return None;
    }
    let distinct_authors = signals.author_commits.len();
    if distinct_authors <= 1 {
        return Some(0.0);
    }
    let total = signals.commit_count as f64;
    let raw_entropy: f64 = signals
        .author_commits
        .values()
        .map(|&count| {
            let p = count as f64 / total;
            -p * p.log2()
        })
        .sum();
    let max_entropy = (distinct_authors as f64).log2();
    Some((raw_entropy / max_entropy).clamp(0.0, 1.0))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    fn run_git(dir: &Path, args: &[&str]) {
        let status = Command::new("git")
            .args(args)
            .current_dir(dir)
            .status()
            .unwrap();
        assert!(status.success(), "git {args:?} failed");
    }

    #[test]
    fn test_commits_with_files_groups_by_commit() {
        let dir = std::env::temp_dir().join(format!("ci_gitlog_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        run_git(&dir, &["init", "-q"]);
        run_git(&dir, &["config", "user.email", "test@example.com"]);
        run_git(&dir, &["config", "user.name", "Test"]);

        std::fs::write(dir.join("a.py"), "1").unwrap();
        std::fs::write(dir.join("b.py"), "1").unwrap();
        run_git(&dir, &["add", "a.py", "b.py"]);
        run_git(&dir, &["commit", "-q", "-m", "first"]);

        std::fs::write(dir.join("a.py"), "2").unwrap();
        run_git(&dir, &["commit", "-q", "-am", "second"]);

        let (commits, available) = commits_with_files(&dir, "1 year");
        assert!(available);
        assert_eq!(commits.len(), 2);
        // git log lists most recent first.
        assert_eq!(commits[0].files, vec!["a.py"]);
        assert_eq!(commits[1].files, vec!["a.py", "b.py"]);
        assert!(commits[0].author.as_deref() == Some("test@example.com"));
        assert!(commits[0].date.is_some());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_commits_with_files_no_git_repo() {
        let dir = std::env::temp_dir().join(format!("ci_gitlog_none_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let (commits, available) = commits_with_files(&dir, "1 year");
        assert!(!available);
        assert!(commits.is_empty());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_commits_with_files_cached_serves_stale_result_within_ttl() {
        // Uses commits_with_files_cached_in with a private cache slot, not
        // the process-wide `static` -- cargo test runs this binary's tests
        // concurrently, and several other tests (cochange::, hotspot::)
        // also drive the static-backed cache. A shared single-slot cache
        // hit by two different (project_root, since) keys at once evicts
        // whichever entry lost the race, which made this assertion flaky
        // under `cargo test`'s default parallelism (verified: failed with
        // "left: 2, right: 1" when run alongside the full suite). The
        // production cache stays a single static -- only this test's view
        // of it is swapped out, so real cross-module cache sharing in
        // `commits_with_files_cached` itself is untouched.
        let cache: RwLock<Option<GitSignalCacheEntry>> = RwLock::new(None);

        let dir = std::env::temp_dir().join(format!(
            "ci_gitlog_cache_{}_{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        run_git(&dir, &["init", "-q"]);
        run_git(&dir, &["config", "user.email", "test@example.com"]);
        run_git(&dir, &["config", "user.name", "Test"]);
        std::fs::write(dir.join("a.py"), "1").unwrap();
        run_git(&dir, &["add", "a.py"]);
        run_git(&dir, &["commit", "-q", "-m", "first"]);

        let (first, available1) = commits_with_files_cached_in(&cache, &dir, "1 year");
        assert!(available1);
        assert_eq!(first.len(), 1);

        // A second commit lands, but within the TTL the cache must still
        // serve the stale (pre-second-commit) result — same contract as
        // `co_changes_cached`.
        std::fs::write(dir.join("b.py"), "1").unwrap();
        run_git(&dir, &["add", "b.py"]);
        run_git(&dir, &["commit", "-q", "-m", "second"]);

        let (second, available2) = commits_with_files_cached_in(&cache, &dir, "1 year");
        assert!(available2);
        assert_eq!(
            second.len(),
            first.len(),
            "within TTL, cache must serve the stale commit count"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_file_signals_groups_by_path_and_author() {
        let commits = vec![
            GitCommit {
                author: Some("alice@example.com".into()),
                date: Some("2026-07-20".into()),
                files: vec!["a.rs".into()],
            },
            GitCommit {
                author: Some("bob@example.com".into()),
                date: Some("2026-07-10".into()),
                files: vec!["a.rs".into(), "b.rs".into()],
            },
        ];
        let signals = file_signals(&commits);
        let a = signals.get("a.rs").unwrap();
        assert_eq!(a.commit_count, 2);
        assert_eq!(a.author_commits.get("alice@example.com"), Some(&1));
        assert_eq!(a.author_commits.get("bob@example.com"), Some(&1));
        // Newest-first input -> last_changed is the newest commit's date.
        assert_eq!(a.last_changed.as_deref(), Some("2026-07-20"));

        let b = signals.get("b.rs").unwrap();
        assert_eq!(b.commit_count, 1);
    }

    #[test]
    fn test_ownership_entropy_below_min_commits_is_none() {
        let sig = FileGitSignals {
            commit_count: 1,
            author_commits: HashMap::from([("alice".to_string(), 1)]),
            last_changed: None,
        };
        assert_eq!(ownership_entropy(&sig, 2), None);
    }

    #[test]
    fn test_ownership_entropy_single_author_is_zero_not_none() {
        let sig = FileGitSignals {
            commit_count: 5,
            author_commits: HashMap::from([("alice".to_string(), 5)]),
            last_changed: None,
        };
        assert_eq!(ownership_entropy(&sig, 2), Some(0.0));
    }

    #[test]
    fn test_ownership_entropy_uniform_split_is_one() {
        let sig = FileGitSignals {
            commit_count: 4,
            author_commits: HashMap::from([("alice".to_string(), 2), ("bob".to_string(), 2)]),
            last_changed: None,
        };
        let e = ownership_entropy(&sig, 2).unwrap();
        assert!((e - 1.0).abs() < 1e-9, "expected ~1.0, got {e}");
    }

    #[test]
    fn test_ownership_entropy_skewed_split_between_zero_and_one() {
        let sig = FileGitSignals {
            commit_count: 10,
            author_commits: HashMap::from([("alice".to_string(), 9), ("bob".to_string(), 1)]),
            last_changed: None,
        };
        let e = ownership_entropy(&sig, 2).unwrap();
        assert!(
            e > 0.0 && e < 1.0,
            "expected strictly between 0 and 1, got {e}"
        );
    }
}
