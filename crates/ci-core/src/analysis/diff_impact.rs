use std::path::Path;
use std::process::Command;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RiskOrder {
    Low = 0,
    Medium = 1,
    High = 2,
    Critical = 3,
}

impl RiskOrder {
    pub fn parse(s: &str) -> Self {
        match s {
            "medium" => Self::Medium,
            "high" => Self::High,
            "critical" => Self::Critical,
            _ => Self::Low,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
            Self::Critical => "critical",
        }
    }
}

pub fn get_git_diff(
    project_root: &Path,
    staged: bool,
    commits: Option<&str>,
    timeout_secs: u64,
) -> (Option<String>, Option<String>) {
    let cmd_args: Vec<String> = if staged {
        vec!["diff".into(), "--cached".into(), "-M".into()]
    } else if let Some(range) = commits {
        vec!["diff".into(), "-M".into(), range.into()]
    } else {
        return (
            None,
            Some("Provide exactly one of staged=true or commits=<range>.".into()),
        );
    };

    let result = Command::new("git")
        .args(&cmd_args)
        .current_dir(project_root)
        .output();

    match result {
        Ok(output) if output.status.success() => (
            Some(String::from_utf8_lossy(&output.stdout).to_string()),
            None,
        ),
        Ok(output) => {
            let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
            let msg = if stderr.is_empty() {
                format!("git exited {}", output.status.code().unwrap_or(-1))
            } else {
                stderr
            };
            (None, Some(msg))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            (None, Some("git not found in PATH".into()))
        }
        Err(_) => (
            None,
            Some(format!("git diff timed out after {timeout_secs}s")),
        ),
    }
}

pub fn is_signature_changed(signature_range: (i64, i64), hunk_ranges: &[(i64, i64)]) -> bool {
    let (sig_start, sig_end) = signature_range;
    hunk_ranges
        .iter()
        .any(|&(hunk_start, hunk_end)| !(hunk_end < sig_start || hunk_start > sig_end))
}

pub fn compute_aggregate_risk(
    affected_symbols: &[HashMap<String, serde_json::Value>],
    unindexed_files: &[String],
) -> String {
    if !unindexed_files.is_empty() {
        return "unknown".to_string();
    }
    affected_symbols
        .iter()
        .filter_map(|s| {
            s.get("risk_assessment")
                .and_then(|r| r.get("level"))
                .and_then(|l| l.as_str())
        })
        .max_by_key(|level| RiskOrder::parse(level) as u8)
        .unwrap_or("low")
        .to_string()
}

pub fn escalate_risk_if_signature_changed(
    signature_changed: bool,
    level: &str,
    reasons: &mut Vec<String>,
) -> String {
    if signature_changed {
        reasons.push("signature modified — all call sites may need update".to_string());
        let current = RiskOrder::parse(level);
        if (current as u8) < (RiskOrder::High as u8) {
            return "high".to_string();
        }
    }
    level.to_string()
}

pub fn sort_affected_symbols(
    symbols: &mut Vec<HashMap<String, serde_json::Value>>,
    max_affected: usize,
) {
    symbols.sort_by(|a, b| {
        let risk_a = a
            .get("risk_assessment")
            .and_then(|r| r.get("level"))
            .and_then(|l| l.as_str())
            .unwrap_or("low");
        let risk_b = b
            .get("risk_assessment")
            .and_then(|r| r.get("level"))
            .and_then(|l| l.as_str())
            .unwrap_or("low");
        let sig_a = a
            .get("signature_changed")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let sig_b = b
            .get("signature_changed")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let blast_a = a
            .get("blast_radius")
            .and_then(|br| br.get("direct_callers"))
            .and_then(|v| v.as_i64())
            .unwrap_or(0);
        let blast_b = b
            .get("blast_radius")
            .and_then(|br| br.get("direct_callers"))
            .and_then(|v| v.as_i64())
            .unwrap_or(0);

        (RiskOrder::parse(risk_b) as u8, sig_b as u8, blast_b).cmp(&(
            RiskOrder::parse(risk_a) as u8,
            sig_a as u8,
            blast_a,
        ))
    });
    symbols.truncate(max_affected);
}

use std::collections::HashMap;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_signature_changed_overlap() {
        assert!(is_signature_changed((5, 10), &[(8, 15)]));
        assert!(is_signature_changed((5, 10), &[(1, 6)]));
        assert!(is_signature_changed((5, 10), &[(5, 10)]));
    }

    #[test]
    fn test_is_signature_changed_no_overlap() {
        assert!(!is_signature_changed((5, 10), &[(11, 20)]));
        assert!(!is_signature_changed((5, 10), &[(1, 4)]));
    }

    #[test]
    fn test_is_signature_changed_empty_hunks() {
        assert!(!is_signature_changed((5, 10), &[]));
    }

    #[test]
    fn test_escalate_risk() {
        let mut reasons = Vec::new();
        let result = escalate_risk_if_signature_changed(true, "low", &mut reasons);
        assert_eq!(result, "high");
        assert_eq!(reasons.len(), 1);

        let mut reasons = Vec::new();
        let result = escalate_risk_if_signature_changed(true, "critical", &mut reasons);
        assert_eq!(result, "critical");

        let mut reasons = Vec::new();
        let result = escalate_risk_if_signature_changed(false, "low", &mut reasons);
        assert_eq!(result, "low");
        assert!(reasons.is_empty());
    }

    #[test]
    fn test_compute_aggregate_risk_with_unindexed() {
        let result = compute_aggregate_risk(&[], &["new_file.rs".to_string()]);
        assert_eq!(result, "unknown");
    }

    #[test]
    fn test_compute_aggregate_risk_max() {
        let s1 = HashMap::from([(
            "risk_assessment".to_string(),
            serde_json::json!({"level": "low"}),
        )]);
        let s2 = HashMap::from([(
            "risk_assessment".to_string(),
            serde_json::json!({"level": "high"}),
        )]);
        let result = compute_aggregate_risk(&[s1, s2], &[]);
        assert_eq!(result, "high");
    }

    #[test]
    fn test_get_git_diff_no_args() {
        let dir = tempfile::tempdir().unwrap();
        let (diff, err) = get_git_diff(dir.path(), false, None, 10);
        assert!(diff.is_none());
        assert!(err.is_some());
    }
}
