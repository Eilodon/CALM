use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
pub struct CoverageData {
    pub source: String,
    pub covered_lines: HashMap<String, HashSet<i64>>,
}

impl CoverageData {
    pub fn none() -> Self {
        Self {
            source: "none".to_string(),
            covered_lines: HashMap::new(),
        }
    }

    pub fn is_covered(&self, abs_path: &str, line_start: i64, line_end: i64) -> bool {
        let Some(file_cov) = self.covered_lines.get(abs_path) else {
            return false;
        };
        (line_start..=line_end).any(|ln| file_cov.contains(&ln))
    }
}

const COVERAGE_SEARCH_PATHS: &[(&str, &str)] = &[
    ("lcov.info", "lcov"),
    ("coverage/lcov.info", "lcov"),
    (".nyc_output/lcov.info", "lcov"),
    (".coverage", "python"),
    ("coverage.out", "go"),
    ("coverage/coverage.out", "go"),
    ("coverage.xml", "cobertura"),
    ("coverage/coverage.xml", "cobertura"),
];

pub fn load_coverage(project_root: &Path) -> CoverageData {
    for &(relative, fmt) in COVERAGE_SEARCH_PATHS {
        let path = project_root.join(relative);
        if !path.exists() {
            continue;
        }
        let result = match fmt {
            "lcov" => parse_lcov(&path, project_root),
            "python" => parse_python_coverage(&path, project_root),
            "go" => parse_go_coverage(&path, project_root),
            "cobertura" => parse_cobertura(&path, project_root),
            _ => continue,
        };
        match result {
            Ok(data) => return data,
            Err(e) => {
                tracing::warn!("Cannot read coverage file {}: {e}", path.display());
                continue;
            }
        }
    }
    CoverageData::none()
}

fn resolve_path(raw: &str, project_root: &Path) -> String {
    let p = PathBuf::from(raw);
    if p.is_absolute() {
        raw.to_string()
    } else {
        project_root.join(raw).to_string_lossy().to_string()
    }
}

fn parse_lcov(path: &Path, project_root: &Path) -> anyhow::Result<CoverageData> {
    let content = std::fs::read_to_string(path)?;
    let mut covered: HashMap<String, HashSet<i64>> = HashMap::new();
    let mut current_file: Option<String> = None;

    for line in content.lines() {
        let line = line.trim();
        if let Some(sf) = line.strip_prefix("SF:") {
            let abs = resolve_path(sf, project_root);
            current_file = Some(abs.clone());
            covered.entry(abs).or_default();
        } else if let Some(da) = line.strip_prefix("DA:") {
            if let Some(ref cf) = current_file {
                let parts: Vec<&str> = da.splitn(2, ',').collect();
                if parts.len() == 2
                    && let (Ok(line_no), Ok(hits)) =
                        (parts[0].parse::<i64>(), parts[1].parse::<i64>())
                    && hits > 0
                {
                    covered.entry(cf.clone()).or_default().insert(line_no);
                }
            }
        } else if line == "end_of_record" {
            current_file = None;
        }
    }
    Ok(CoverageData {
        source: "lcov".to_string(),
        covered_lines: covered,
    })
}

fn parse_python_coverage(path: &Path, project_root: &Path) -> anyhow::Result<CoverageData> {
    use rusqlite::Connection;
    let conn = Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    let mut covered: HashMap<String, HashSet<i64>> = HashMap::new();

    let files: HashMap<i64, String> = {
        let mut stmt = conn.prepare("SELECT id, path FROM file")?;
        stmt.query_map([], |row| {
            Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
        })?
        .filter_map(|r| r.ok())
        .collect()
    };

    // Try line_bits table first (coverage.py 6+)
    let line_bits_ok = conn.prepare("SELECT 1 FROM line_bits LIMIT 1").is_ok();

    if line_bits_ok {
        for (&file_id, file_path) in &files {
            let abs_path = resolve_path(file_path, project_root);
            let mut lines = HashSet::new();
            let mut stmt = conn.prepare("SELECT numbits FROM line_bits WHERE file_id=?")?;
            let rows = stmt.query_map([file_id], |row| row.get::<_, Vec<u8>>(0))?;
            for numbits in rows.flatten() {
                for (byte_idx, byte_val) in numbits.iter().enumerate() {
                    for bit in 0..8 {
                        if byte_val & (1 << bit) != 0 {
                            lines.insert((byte_idx * 8 + bit + 1) as i64);
                        }
                    }
                }
            }
            covered.insert(abs_path, lines);
        }
    } else {
        // Fallback: arc table (coverage.py 5.x branch-coverage)
        let arc_ok = conn.prepare("SELECT 1 FROM arc LIMIT 1").is_ok();
        if arc_ok {
            for (&file_id, file_path) in &files {
                let abs_path = resolve_path(file_path, project_root);
                let mut lines = HashSet::new();
                let mut stmt = conn.prepare("SELECT fromno, tono FROM arc WHERE file_id=?")?;
                let rows = stmt.query_map([file_id], |row| {
                    Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?))
                })?;
                for (from_l, to_l) in rows.flatten() {
                    if from_l > 0 {
                        lines.insert(from_l);
                    }
                    if to_l > 0 {
                        lines.insert(to_l);
                    }
                }
                covered.insert(abs_path, lines);
            }
        } else {
            // Final fallback: line_data table
            for (&file_id, file_path) in &files {
                let abs_path = resolve_path(file_path, project_root);
                let mut lines = HashSet::new();
                let mut stmt = conn.prepare("SELECT lineno FROM line_data WHERE file_id=?")?;
                let rows = stmt.query_map([file_id], |row| row.get::<_, i64>(0))?;
                for line_no in rows.flatten() {
                    if line_no > 0 {
                        lines.insert(line_no);
                    }
                }
                covered.insert(abs_path, lines);
            }
        }
    }

    Ok(CoverageData {
        source: "python".to_string(),
        covered_lines: covered,
    })
}

fn parse_go_coverage(path: &Path, project_root: &Path) -> anyhow::Result<CoverageData> {
    let content = std::fs::read_to_string(path)?;
    let mut covered: HashMap<String, HashSet<i64>> = HashMap::new();

    for line in content.lines() {
        let line = line.trim();
        if line.starts_with("mode:") || line.is_empty() {
            continue;
        }
        let parts: Vec<&str> = line.split(' ').collect();
        if parts.len() != 3 {
            continue;
        }
        let hit_count: i64 = match parts[2].parse() {
            Ok(c) => c,
            Err(_) => continue,
        };
        if hit_count == 0 {
            continue;
        }
        let location = parts[0];
        let Some(colon_idx) = location.rfind(':') else {
            continue;
        };
        let file_part = &location[..colon_idx];
        let range_part = &location[colon_idx + 1..];
        let Some((start_str, end_str)) = range_part.split_once(',') else {
            continue;
        };
        let start_line: i64 = match start_str.split('.').next().and_then(|s| s.parse().ok()) {
            Some(l) => l,
            None => continue,
        };
        let end_line: i64 = match end_str.split('.').next().and_then(|s| s.parse().ok()) {
            Some(l) => l,
            None => continue,
        };

        // Try to match Go package path to local file
        let file_parts: Vec<&str> = file_part.split('/').collect();
        let mut matched_candidate: Option<PathBuf> = None;
        for n in (1..=file_parts.len()).rev() {
            let suffix: PathBuf = file_parts[file_parts.len() - n..].iter().collect();
            let candidate = project_root.join(&suffix);
            if candidate.exists() {
                matched_candidate = Some(candidate);
                break;
            }
        }
        if let Some(candidate) = matched_candidate {
            let abs_path = candidate.to_string_lossy().to_string();
            let entry = covered.entry(abs_path).or_default();
            for ln in start_line..=end_line {
                entry.insert(ln);
            }
        }
    }
    Ok(CoverageData {
        source: "go".to_string(),
        covered_lines: covered,
    })
}

fn parse_cobertura(path: &Path, project_root: &Path) -> anyhow::Result<CoverageData> {
    let content = std::fs::read_to_string(path)?;
    let mut covered: HashMap<String, HashSet<i64>> = HashMap::new();

    // Simple XML parsing without full DOM — iterate lines for <class> and <line> elements.
    // This avoids pulling in an XML crate dependency for a single use case.
    // Pattern matches the subset of Cobertura XML that coverage tools actually produce.
    let mut current_file: Option<String> = None;
    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("<class ") || trimmed.starts_with("<class\t") {
            if let Some(filename) = extract_xml_attr(trimmed, "filename") {
                current_file = Some(resolve_path(filename, project_root));
            }
        } else if trimmed == "</class>" || trimmed.starts_with("</class>") {
            current_file = None;
        } else if trimmed.starts_with("<line ")
            && let Some(ref cf) = current_file
        {
            let hits: i64 = extract_xml_attr(trimmed, "hits")
                .and_then(|s| s.parse().ok())
                .unwrap_or(0);
            if hits > 0 {
                let line_no: i64 = extract_xml_attr(trimmed, "number")
                    .and_then(|s| s.parse().ok())
                    .unwrap_or(0);
                if line_no >= 1 {
                    covered.entry(cf.clone()).or_default().insert(line_no);
                }
            }
        }
    }
    Ok(CoverageData {
        source: "cobertura".to_string(),
        covered_lines: covered,
    })
}

fn extract_xml_attr<'a>(element: &'a str, attr_name: &str) -> Option<&'a str> {
    let pattern = format!("{attr_name}=\"");
    let start = element.find(&pattern)? + pattern.len();
    let rest = &element[start..];
    let end = rest.find('"')?;
    Some(&rest[..end])
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn test_coverage_data_none() {
        let cov = CoverageData::none();
        assert_eq!(cov.source, "none");
        assert!(!cov.is_covered("/foo.py", 1, 10));
    }

    #[test]
    fn test_is_covered() {
        let mut lines = HashSet::new();
        lines.insert(5);
        lines.insert(10);
        let cov = CoverageData {
            source: "lcov".to_string(),
            covered_lines: HashMap::from([("/foo.py".to_string(), lines)]),
        };
        assert!(cov.is_covered("/foo.py", 4, 6));
        assert!(cov.is_covered("/foo.py", 10, 10));
        assert!(!cov.is_covered("/foo.py", 6, 9));
        assert!(!cov.is_covered("/bar.py", 1, 100));
    }

    #[test]
    fn test_parse_lcov() {
        let dir = tempfile::tempdir().unwrap();
        let lcov = dir.path().join("lcov.info");
        fs::write(
            &lcov,
            "SF:src/main.rs\nDA:1,1\nDA:2,0\nDA:3,5\nend_of_record\n",
        )
        .unwrap();
        let data = parse_lcov(&lcov, dir.path()).unwrap();
        assert_eq!(data.source, "lcov");
        let key = dir.path().join("src/main.rs").to_string_lossy().to_string();
        let lines = data.covered_lines.get(&key).unwrap();
        assert!(lines.contains(&1));
        assert!(!lines.contains(&2));
        assert!(lines.contains(&3));
    }

    #[test]
    fn test_parse_cobertura() {
        let dir = tempfile::tempdir().unwrap();
        let xml = dir.path().join("coverage.xml");
        fs::write(
            &xml,
            r#"<?xml version="1.0"?>
<coverage>
  <packages>
    <package>
      <classes>
        <class filename="src/lib.rs">
          <lines>
            <line number="1" hits="3"/>
            <line number="2" hits="0"/>
            <line number="5" hits="1"/>
          </lines>
        </class>
      </classes>
    </package>
  </packages>
</coverage>"#,
        )
        .unwrap();
        let data = parse_cobertura(&xml, dir.path()).unwrap();
        assert_eq!(data.source, "cobertura");
        let key = dir.path().join("src/lib.rs").to_string_lossy().to_string();
        let lines = data.covered_lines.get(&key).unwrap();
        assert!(lines.contains(&1));
        assert!(!lines.contains(&2));
        assert!(lines.contains(&5));
    }

    #[test]
    fn test_parse_go_coverage() {
        let dir = tempfile::tempdir().unwrap();
        // Create a file so the path matching works
        let src_dir = dir.path().join("pkg");
        fs::create_dir_all(&src_dir).unwrap();
        fs::write(src_dir.join("handler.go"), "package pkg").unwrap();

        let cov_file = dir.path().join("coverage.out");
        fs::write(
            &cov_file,
            "mode: set\nexample.com/pkg/handler.go:10.1,15.1 1 1\nexample.com/pkg/handler.go:20.1,20.1 1 0\n",
        )
        .unwrap();
        let data = parse_go_coverage(&cov_file, dir.path()).unwrap();
        assert_eq!(data.source, "go");
        let key = src_dir.join("handler.go").to_string_lossy().to_string();
        let lines = data.covered_lines.get(&key).unwrap();
        assert!(lines.contains(&10));
        assert!(lines.contains(&15));
        assert!(!lines.contains(&20));
    }

    #[test]
    fn test_load_coverage_not_found() {
        let dir = tempfile::tempdir().unwrap();
        let data = load_coverage(dir.path());
        assert_eq!(data.source, "none");
    }
}
