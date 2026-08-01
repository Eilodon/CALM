//! Decode a SCIP `Index` into flat occurrences the ingester can match against
//! `ci`'s call sites and symbols. We keep only file/line/symbol/role — SCIP's
//! rich moniker string is preserved verbatim as the identity key.

/// One SCIP occurrence, normalized to 1-based line and `ci`'s conventions.
#[derive(Debug, Clone)]
pub struct ScipOccurrence {
    pub file: String,
    /// 1-based line of the occurrence start.
    pub line: usize,
    /// Zero-based, half-open UTF-8 byte span when the SCIP document provides a
    /// usable source range and position encoding. `None` fails closed during
    /// call-site matching; legacy line-only consumers may still inspect `line`.
    pub start_byte: Option<usize>,
    pub end_byte: Option<usize>,
    /// Hash of the exact source text used to turn SCIP's line/character range
    /// into the byte span. It is a freshness precondition, never a CallSite key.
    pub source_file_hash: Option<String>,
    /// SCIP symbol moniker (opaque identity string).
    pub symbol: String,
    pub is_def: bool,
    /// True for `local N` monikers (function-scoped, not cross-file useful).
    pub is_local: bool,
}

pub fn parse_index(
    index: &scip::types::Index,
    rebase_prefix: &std::path::Path,
) -> Vec<ScipOccurrence> {
    // Some indexers run against a subdirectory rather than the repo root
    // CALM's DB paths are keyed on (e.g. a `go.work` module, or a nested
    // Maven module) — `rebase_prefix` is that subdirectory, relative to the
    // repo root, so occurrences land on the same path convention as the rest
    // of the graph. Empty prefix (Rust's runner today, which always runs at
    // the repo root) is the identity transform: behavior unchanged.
    let project_root = file_uri_to_path(&index.metadata.project_root);
    let mut out = Vec::new();
    for doc in &index.documents {
        let source = document_source(doc, project_root.as_deref());
        let source_hash = source
            .as_deref()
            .map(crate::indexer::pipeline::hash_content);
        for occ in &doc.occurrences {
            // SCIP range is [startLine, startChar, endLine, endChar] (0-based) or
            // [startLine, startChar, endChar] when single-line.
            let Some(&start_line) = occ.range.first() else {
                continue;
            };
            let is_def = occ.symbol_roles & (scip::types::SymbolRole::Definition as i32) != 0;
            // A handful of indexers emit an absolute `relative_path` instead
            // of a true relative one — strip the index's own project root
            // first so both shapes end up rebased the same way.
            let relative = if std::path::Path::new(&doc.relative_path).is_absolute() {
                strip_project_root(&doc.relative_path, project_root.as_deref())
            } else {
                doc.relative_path.as_str()
            };
            let byte_range = source.as_deref().and_then(|source| {
                occurrence_byte_range(source, occ, doc.position_encoding.enum_value().ok()?)
            });
            out.push(ScipOccurrence {
                file: rebase_path(relative, rebase_prefix),
                line: (start_line as usize) + 1,
                start_byte: byte_range.map(|(start, _)| start),
                end_byte: byte_range.map(|(_, end)| end),
                source_file_hash: byte_range.as_ref().map(|_| source_hash.clone()).flatten(),
                symbol: occ.symbol.clone(),
                is_def,
                is_local: occ.symbol.starts_with("local "),
            });
        }
    }
    out
}

/// Prefer embedded SCIP document text; otherwise read the document from the
/// protocol's project root, exactly as SCIP specifies for consumers. Failed or
/// non-UTF-8 reads are deliberately represented as `None` so range matching
/// cannot fall back to a lossy line-only approximation.
fn document_source(
    doc: &scip::types::Document,
    project_root: Option<&std::path::Path>,
) -> Option<String> {
    if !doc.text.is_empty() {
        return Some(doc.text.clone());
    }
    let path = if std::path::Path::new(&doc.relative_path).is_absolute() {
        std::path::PathBuf::from(&doc.relative_path)
    } else {
        project_root?.join(&doc.relative_path)
    };
    std::fs::read_to_string(path).ok()
}

/// Resolve a SCIP half-open range to absolute UTF-8 byte offsets. Typed ranges
/// have protocol precedence; malformed, multi-line, unknown-encoding, or
/// non-boundary positions fail closed rather than becoming a line-key match.
fn occurrence_byte_range(
    source: &str,
    occurrence: &scip::types::Occurrence,
    encoding: scip::types::PositionEncoding,
) -> Option<(usize, usize)> {
    let (start_line, start_character, end_line, end_character) = occurrence_range(occurrence)?;
    let start = byte_offset_at(source, start_line, start_character, encoding)?;
    let end = byte_offset_at(source, end_line, end_character, encoding)?;
    (start <= end).then_some((start, end))
}

fn occurrence_range(occurrence: &scip::types::Occurrence) -> Option<(usize, usize, usize, usize)> {
    use scip::types::occurrence::Typed_range;

    let to_usize = |n: i32| usize::try_from(n).ok();
    match occurrence.typed_range.as_ref() {
        Some(Typed_range::SingleLineRange(range)) => Some((
            to_usize(range.line)?,
            to_usize(range.start_character)?,
            to_usize(range.line)?,
            to_usize(range.end_character)?,
        )),
        Some(Typed_range::MultiLineRange(range)) => Some((
            to_usize(range.start_line)?,
            to_usize(range.start_character)?,
            to_usize(range.end_line)?,
            to_usize(range.end_character)?,
        )),
        Some(_) => None,
        None => match occurrence.range.as_slice() {
            [start_line, start_character, end_character] => Some((
                to_usize(*start_line)?,
                to_usize(*start_character)?,
                to_usize(*start_line)?,
                to_usize(*end_character)?,
            )),
            [start_line, start_character, end_line, end_character] => Some((
                to_usize(*start_line)?,
                to_usize(*start_character)?,
                to_usize(*end_line)?,
                to_usize(*end_character)?,
            )),
            _ => None,
        },
    }
}

fn byte_offset_at(
    source: &str,
    line: usize,
    character: usize,
    encoding: scip::types::PositionEncoding,
) -> Option<usize> {
    use scip::types::PositionEncoding;

    let line_start = if line == 0 {
        0
    } else {
        source.match_indices('\n').nth(line - 1)?.0 + 1
    };
    let line_end = source[line_start..]
        .find('\n')
        .map(|offset| line_start + offset)
        .unwrap_or(source.len());
    let line_text = &source[line_start..line_end];
    let relative = match encoding {
        PositionEncoding::UTF8CodeUnitOffsetFromLineStart => (character <= line_text.len()
            && line_text.is_char_boundary(character))
        .then_some(character),
        PositionEncoding::UTF16CodeUnitOffsetFromLineStart => {
            offset_for_units(line_text, character, |character| character.len_utf16())
        }
        PositionEncoding::UTF32CodeUnitOffsetFromLineStart => {
            offset_for_units(line_text, character, |_| 1)
        }
        PositionEncoding::UnspecifiedPositionEncoding => None,
    }?;
    Some(line_start + relative)
}

fn offset_for_units(
    text: &str,
    wanted_units: usize,
    units_for: impl Fn(char) -> usize,
) -> Option<usize> {
    let mut units = 0;
    if wanted_units == 0 {
        return Some(0);
    }
    for (byte, character) in text.char_indices() {
        if units == wanted_units {
            return Some(byte);
        }
        units = units.checked_add(units_for(character))?;
        if units > wanted_units {
            return None;
        }
    }
    (units == wanted_units).then_some(text.len())
}

/// Join `prefix` onto a (already project-root-relative) SCIP path and
/// normalize the result to a `/`-separated string with no `.`/`..`
/// segments, so it matches the plain relative-path convention the rest of
/// the graph (`file_index`, `call_edges`) is keyed on. `prefix` empty is the
/// identity transform.
fn rebase_path(relative_path: &str, prefix: &std::path::Path) -> String {
    let joined = if prefix.as_os_str().is_empty() {
        std::path::PathBuf::from(relative_path)
    } else {
        prefix.join(relative_path)
    };
    normalize_to_slash_path(&joined)
}

fn normalize_to_slash_path(path: &std::path::Path) -> String {
    // Tracked separately from `parts` (rather than pushed as an empty
    // sentinel) so a `..` component can never accidentally pop off the
    // "this path is absolute" marker itself.
    let mut is_absolute = false;
    let mut parts: Vec<std::ffi::OsString> = Vec::new();
    for comp in path.components() {
        match comp {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                parts.pop();
            }
            std::path::Component::Normal(s) => parts.push(s.to_os_string()),
            std::path::Component::RootDir => is_absolute = true,
            // Windows drive letters — this feature targets Linux/macOS
            // indexers only; ignore rather than mishandle.
            std::path::Component::Prefix(_) => {}
        }
    }
    let joined = parts
        .iter()
        .map(|s| s.to_string_lossy())
        .collect::<Vec<_>>()
        .join("/");
    // Preserve absoluteness: an unresolved absolute path (unknown
    // project_root, see `strip_project_root`) must stay visibly absolute
    // rather than silently degrade into a relative-looking string that
    // could spuriously collide with an unrelated real file at that path.
    if is_absolute {
        format!("/{joined}")
    } else {
        joined
    }
}

/// Make an absolute SCIP `relative_path` (some indexers emit one instead of
/// a true relative path) relative to `project_root`, so it can be rebased
/// like a normal one. Falls back to the original string, unchanged, when
/// `project_root` is unknown or isn't actually a prefix of `abs_path` — a
/// best-effort degrade rather than a hard failure over one malformed field.
fn strip_project_root<'a>(abs_path: &'a str, project_root: Option<&std::path::Path>) -> &'a str {
    let Some(root) = project_root else {
        return abs_path;
    };
    match std::path::Path::new(abs_path).strip_prefix(root) {
        Ok(stripped) => stripped.to_str().unwrap_or(abs_path),
        Err(_) => abs_path,
    }
}

/// Percent-decode a `file://` URI, as emitted in SCIP's
/// `Metadata.project_root`, into a plain filesystem path. Only unescapes the
/// minimal `%XX` sequences real indexers actually emit (e.g. spaces as
/// `%20`) — not a full URI parser, since pulling in a URL crate for this one
/// field isn't worth it. Returns `None` for an empty/missing `project_root`
/// (some indexers, or hand-built test fixtures, don't set it) or one that
/// isn't a `file://` URI.
fn file_uri_to_path(uri: &str) -> Option<std::path::PathBuf> {
    let rest = uri.strip_prefix("file://")?;
    if rest.is_empty() {
        return None;
    }
    Some(std::path::PathBuf::from(percent_decode(rest)))
}

fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && i + 2 < bytes.len()
            && let Ok(byte) = u8::from_str_radix(&s[i + 1..i + 3], 16)
        {
            out.push(byte);
            i += 3;
            continue;
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

pub fn parse_scip_file(
    path: &std::path::Path,
    rebase_prefix: &std::path::Path,
) -> anyhow::Result<Vec<ScipOccurrence>> {
    let bytes = std::fs::read(path)?;
    use protobuf::Message;
    let index = scip::types::Index::parse_from_bytes(&bytes)?;
    Ok(parse_index(&index, rebase_prefix))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_definition_and_reference_occurrences() {
        // Minimal hand-built SCIP index: one doc, one def + one ref.
        let mut index = scip::types::Index::new();
        let mut doc = scip::types::Document::new();
        doc.relative_path = "core/src/engine.rs".into();
        doc.text = "\n\n    start\n\n\n\n        start\n".into();
        doc.position_encoding = protobuf::EnumOrUnknown::new(
            scip::types::PositionEncoding::UTF8CodeUnitOffsetFromLineStart,
        );
        let mut def = scip::types::Occurrence::new();
        def.range = vec![2, 4, 2, 7]; // line 2 (0-based), cols
        def.symbol = "rust-analyzer cargo demo-core 0.1.0 engine/impl#[Engine]start().".into();
        def.symbol_roles = scip::types::SymbolRole::Definition as i32;
        let mut rf = scip::types::Occurrence::new();
        rf.range = vec![5, 8, 5, 13];
        rf.symbol = def.symbol.clone();
        doc.occurrences = vec![def, rf];
        index.documents = vec![doc];

        let occ = parse_index(&index, std::path::Path::new(""));
        assert_eq!(occ.len(), 2);
        let def = occ.iter().find(|o| o.is_def).unwrap();
        assert_eq!(def.file, "core/src/engine.rs");
        assert_eq!(def.line, 3); // 1-based
        assert_eq!(def.start_byte, Some(6));
        assert_eq!(def.end_byte, Some(9));
        assert!(def.source_file_hash.is_some());
        assert_eq!(
            def.symbol,
            "rust-analyzer cargo demo-core 0.1.0 engine/impl#[Engine]start()."
        );
    }

    #[test]
    fn rebase_prefix_joins_onto_a_subroot() {
        let mut index = scip::types::Index::new();
        let mut doc = scip::types::Document::new();
        doc.relative_path = "helper.go".into();
        let mut def = scip::types::Occurrence::new();
        def.range = vec![0, 0, 0, 4];
        def.symbol = "go gomod example.com/mod v0.0.0 helper.".into();
        def.symbol_roles = scip::types::SymbolRole::Definition as i32;
        doc.occurrences = vec![def];
        index.documents = vec![doc];

        let occ = parse_index(&index, std::path::Path::new("services/api"));
        assert_eq!(occ.len(), 1);
        assert_eq!(occ[0].file, "services/api/helper.go");
    }

    #[test]
    fn rebase_prefix_normalizes_dot_segments() {
        let mut index = scip::types::Index::new();
        let mut doc = scip::types::Document::new();
        doc.relative_path = "./helper.go".into();
        let mut def = scip::types::Occurrence::new();
        def.range = vec![0, 0, 0, 4];
        def.symbol = "go gomod example.com/mod v0.0.0 helper.".into();
        doc.occurrences = vec![def];
        index.documents = vec![doc];

        let occ = parse_index(&index, std::path::Path::new("services/api/"));
        assert_eq!(occ[0].file, "services/api/helper.go");
    }

    #[test]
    fn absolute_relative_path_is_stripped_of_project_root_then_rebased() {
        // Some indexers emit an absolute `relative_path` instead of a true
        // relative one; `Metadata.project_root` is the file:// URI the
        // ingester strips it against before rebasing onto `prefix`.
        let mut index = scip::types::Index::new();
        index.metadata.mut_or_insert_default().project_root = "file:///repo/services/api".into();
        let mut doc = scip::types::Document::new();
        doc.relative_path = "/repo/services/api/helper.go".into();
        let mut def = scip::types::Occurrence::new();
        def.range = vec![0, 0, 0, 4];
        def.symbol = "go gomod example.com/mod v0.0.0 helper.".into();
        doc.occurrences = vec![def];
        index.documents = vec![doc];

        let occ = parse_index(&index, std::path::Path::new("services/api"));
        assert_eq!(occ[0].file, "services/api/helper.go");
    }

    #[test]
    fn absolute_relative_path_with_unknown_project_root_falls_back_unchanged() {
        // No project_root at all (some indexers/fixtures omit it) — the
        // absolute path can't be safely stripped, so it degrades to
        // passing the raw absolute string through `rebase_path` rather
        // than panicking or silently dropping the occurrence.
        let mut index = scip::types::Index::new();
        let mut doc = scip::types::Document::new();
        doc.relative_path = "/some/other/tree/helper.go".into();
        let mut def = scip::types::Occurrence::new();
        def.range = vec![0, 0, 0, 4];
        def.symbol = "go gomod example.com/mod v0.0.0 helper.".into();
        doc.occurrences = vec![def];
        index.documents = vec![doc];

        let occ = parse_index(&index, std::path::Path::new(""));
        assert_eq!(occ[0].file, "/some/other/tree/helper.go");
    }

    #[test]
    fn empty_prefix_is_identity_rust_runner_behavior_unchanged() {
        assert_eq!(
            rebase_path("core/src/engine.rs", std::path::Path::new("")),
            "core/src/engine.rs"
        );
    }

    #[test]
    fn file_uri_to_path_decodes_percent_escapes() {
        assert_eq!(
            file_uri_to_path("file:///repo%20dir/sub"),
            Some(std::path::PathBuf::from("/repo dir/sub"))
        );
        assert_eq!(file_uri_to_path(""), None);
        assert_eq!(file_uri_to_path("not-a-uri"), None);
    }
}
