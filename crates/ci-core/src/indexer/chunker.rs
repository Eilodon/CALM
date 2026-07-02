//! Code-body chunking for the semantic search Layer 2 (`code_chunks` /
//! `code_chunk_vecs`).
//!
//! Layer 1 (`embedding::symbol_doc`) embeds *symbol identity* — name,
//! signature, docstring. It never sees the code inside a function body, so a
//! query that only matches implementation vocabulary (a library name, a
//! variable, a control-flow idiom used *inside* a function) has nothing to
//! match against, even though the symbol is exactly what the query means.
//! This module slices actual source text into windows — one whole-body chunk
//! for short symbols, a sliding window for longer ones, plus windows over the
//! code between symbols (module scaffolding, field blocks, decorators) — so
//! Layer 2 can embed real code text, the same granularity a raw sliding-window
//! indexer would use.

use crate::indexer::parser::ParsedSymbol;
use crate::types::SymbolKind;

/// Chunk target size, in source lines.
const CHUNK_MAX_LINES: usize = 30;
/// Step between successive windows inside a body longer than
/// `CHUNK_MAX_LINES` (10-line overlap at the default size, so a match near a
/// window boundary is still fully contained in at least one window).
const CHUNK_STRIDE: usize = 20;

/// One code-body slice ready to embed and persist into `code_chunks`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodeChunk {
    /// 1-indexed, inclusive.
    pub line_start: usize,
    /// 1-indexed, inclusive.
    pub line_end: usize,
    pub chunk_text: String,
    /// Enclosing function/method's qualified_name, when this chunk came from
    /// a symbol body rather than the gap-filling pass.
    pub symbol_qn: Option<String>,
}

/// True for symbol kinds that carry an executable body worth chunking on
/// their own. Containers (class/struct/interface/trait/impl/enum/type) are
/// excluded — their own code is either delegated to nested leaf symbols
/// (already chunked individually) or picked up by the gap-filling pass below,
/// which avoids embedding a class body once as a whole *and* once per method.
fn is_body_bearing(kind: SymbolKind) -> bool {
    matches!(
        kind,
        SymbolKind::Function | SymbolKind::Method | SymbolKind::Constructor | SymbolKind::Variable
    )
}

/// Chunk one file's source into code-body windows for Layer-2 semantic
/// embedding.
///
/// `symbols` is the file's already-extracted, fully-qualified symbol list
/// (see `indexer::pipeline::extract_file_data`) — reused here instead of
/// re-parsing so chunking stays pure CPU work, safe to run in the same
/// parallel extraction pass as the rest of that function.
pub fn chunk_file(source: &str, symbols: &[ParsedSymbol]) -> Vec<CodeChunk> {
    let lines: Vec<&str> = source.lines().collect();
    if lines.is_empty() {
        return Vec::new();
    }

    let mut spans: Vec<(usize, usize, &str)> = symbols
        .iter()
        .filter(|s| is_body_bearing(s.kind) && s.line_start >= 1 && s.line_end >= s.line_start)
        .map(|s| {
            (
                s.line_start,
                s.line_end.min(lines.len()),
                s.qualified_name.as_str(),
            )
        })
        .collect();
    spans.sort_by_key(|(start, ..)| *start);

    let mut out = Vec::new();
    for &(start, end, qn) in &spans {
        window_span(&lines, start, end, Some(qn), &mut out);
    }

    // Gap-fill: anything not covered by a body-bearing symbol span (module
    // scaffolding, field-only classes, decorators, imports, blank-separated
    // top-level statements) gets windowed the same way, untagged. `cursor`
    // only ever moves forward, so nested/overlapping spans (a closure defined
    // inside its enclosing function) can't produce a negative-width gap.
    let mut cursor = 1usize;
    for &(start, end, _) in &spans {
        if start > cursor {
            window_span(&lines, cursor, start - 1, None, &mut out);
        }
        cursor = cursor.max(end + 1);
    }
    if cursor <= lines.len() {
        window_span(&lines, cursor, lines.len(), None, &mut out);
    }

    out
}

/// Slice `[start, end]` (1-indexed, inclusive, clamped to `lines`) into one
/// chunk when the span already fits `CHUNK_MAX_LINES`, or a `CHUNK_STRIDE`
/// sliding window when it's longer — tagging every produced chunk with
/// `symbol_qn`.
fn window_span(
    lines: &[&str],
    start: usize,
    end: usize,
    symbol_qn: Option<&str>,
    out: &mut Vec<CodeChunk>,
) {
    let end = end.min(lines.len());
    if start == 0 || start > end {
        return;
    }
    if end - start < CHUNK_MAX_LINES {
        push_chunk(lines, start, end, symbol_qn, out);
        return;
    }
    let mut win_start = start;
    loop {
        let win_end = (win_start + CHUNK_MAX_LINES - 1).min(end);
        push_chunk(lines, win_start, win_end, symbol_qn, out);
        if win_end >= end {
            break;
        }
        win_start += CHUNK_STRIDE;
    }
}

/// Slice and push lines `[start, end]` (1-indexed, inclusive) as one chunk,
/// skipping blank/whitespace-only ranges — they carry no searchable signal.
fn push_chunk(
    lines: &[&str],
    start: usize,
    end: usize,
    symbol_qn: Option<&str>,
    out: &mut Vec<CodeChunk>,
) {
    if start == 0 || start > end || end > lines.len() {
        return;
    }
    let text = lines[start - 1..end].join("\n");
    if text.trim().is_empty() {
        return;
    }
    out.push(CodeChunk {
        line_start: start,
        line_end: end,
        chunk_text: text,
        symbol_qn: symbol_qn.map(str::to_string),
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sym(name: &str, kind: SymbolKind, line_start: usize, line_end: usize) -> ParsedSymbol {
        ParsedSymbol {
            qualified_name: format!("test.py::{name}"),
            name: name.to_string(),
            kind,
            language: "python".to_string(),
            path: "test.py".to_string(),
            line_start,
            line_end,
            signature: String::new(),
            docstring: String::new(),
            name_tokens: name.to_string(),
            is_entry_point: false,
            is_test: false,
            class_context: None,
            complexity: 1,
        }
    }

    fn lines(n: usize, prefix: &str) -> String {
        (1..=n)
            .map(|i| format!("{prefix}_{i}"))
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn empty_source_yields_no_chunks() {
        assert!(chunk_file("", &[]).is_empty());
        assert!(chunk_file("   \n\n\t\n", &[]).is_empty());
    }

    #[test]
    fn short_symbol_body_becomes_one_whole_chunk() {
        let source = "def f():\n    a = 1\n    return a\n";
        let symbols = vec![sym("f", SymbolKind::Function, 1, 3)];
        let chunks = chunk_file(source, &symbols);

        let body: Vec<_> = chunks
            .iter()
            .filter(|c| c.symbol_qn.as_deref() == Some("test.py::f"))
            .collect();
        assert_eq!(body.len(), 1, "a <=30 line body is one chunk: {chunks:?}");
        assert_eq!(body[0].line_start, 1);
        assert_eq!(body[0].line_end, 3);
        assert_eq!(body[0].chunk_text, "def f():\n    a = 1\n    return a");
    }

    #[test]
    fn long_symbol_body_becomes_sliding_window() {
        // 100-line body: windows of <=30 lines, stride 20, full coverage to line_end.
        let source = lines(100, "line");
        let symbols = vec![sym("big", SymbolKind::Function, 1, 100)];
        let chunks = chunk_file(&source, &symbols);
        let body: Vec<_> = chunks
            .iter()
            .filter(|c| c.symbol_qn.as_deref() == Some("test.py::big"))
            .collect();

        assert!(
            body.len() > 1,
            "long body must be split: {} chunks",
            body.len()
        );
        for c in &body {
            assert!(
                c.line_end - c.line_start < CHUNK_MAX_LINES,
                "chunk {}-{} exceeds max window size",
                c.line_start,
                c.line_end
            );
        }
        assert_eq!(body.first().unwrap().line_start, 1);
        assert_eq!(
            body.last().unwrap().line_end,
            100,
            "coverage must reach line_end"
        );

        // Consecutive windows overlap (stride < max) so no boundary match is lost.
        for w in body.windows(2) {
            assert!(w[1].line_start > w[0].line_start, "windows must advance");
            assert!(
                w[1].line_start <= w[0].line_end,
                "adjacent windows should overlap: {:?} -> {:?}",
                w[0],
                w[1]
            );
        }
    }

    #[test]
    fn gap_between_symbols_is_chunked_untagged() {
        // 5-line header/import block, then a function starting at line 6.
        let header = "import os\nimport sys\n\nCONST = 1\n\n";
        let body = "def f():\n    pass\n";
        let source = format!("{header}{body}");
        let symbols = vec![sym("f", SymbolKind::Function, 6, 7)];
        let chunks = chunk_file(&source, &symbols);

        let gap: Vec<_> = chunks.iter().filter(|c| c.symbol_qn.is_none()).collect();
        assert_eq!(gap.len(), 1, "one untagged gap chunk before the function");
        assert_eq!(gap[0].line_start, 1);
        assert_eq!(gap[0].line_end, 5);
        assert!(gap[0].chunk_text.contains("import os"));

        let body_chunks: Vec<_> = chunks
            .iter()
            .filter(|c| c.symbol_qn.as_deref() == Some("test.py::f"))
            .collect();
        assert_eq!(body_chunks.len(), 1);
        assert_eq!(body_chunks[0].line_start, 6);
    }

    #[test]
    fn trailing_gap_after_last_symbol_is_chunked() {
        let source = "def f():\n    pass\n\n# trailing comment\n";
        let symbols = vec![sym("f", SymbolKind::Function, 1, 2)];
        let chunks = chunk_file(source, &symbols);

        let gap: Vec<_> = chunks.iter().filter(|c| c.symbol_qn.is_none()).collect();
        assert_eq!(gap.len(), 1);
        assert_eq!(gap[0].line_start, 3);
        assert!(gap[0].chunk_text.contains("trailing comment"));
    }

    #[test]
    fn blank_only_gap_is_skipped() {
        let source = "def f():\n    pass\n\n\ndef g():\n    pass\n";
        let symbols = vec![
            sym("f", SymbolKind::Function, 1, 2),
            sym("g", SymbolKind::Function, 5, 6),
        ];
        let chunks = chunk_file(source, &symbols);
        assert!(
            chunks.iter().all(|c| c.symbol_qn.is_some()),
            "a purely-blank gap must not produce a chunk: {chunks:?}"
        );
        assert_eq!(chunks.len(), 2);
    }

    #[test]
    fn no_symbols_falls_back_to_whole_file_windowing() {
        // A script with only top-level statements — no function/class symbols.
        let source = "x = 1\ny = 2\nprint(x + y)\n";
        let chunks = chunk_file(source, &[]);
        assert_eq!(chunks.len(), 1);
        assert!(chunks[0].symbol_qn.is_none());
        assert_eq!(chunks[0].line_start, 1);
        assert_eq!(chunks[0].line_end, 3);
    }

    #[test]
    fn container_kinds_are_not_chunked_as_their_own_span_but_gap_fills() {
        // A class with one method: the class's own 4-line span (header + method
        // + closer) must not appear as a duplicate whole-class chunk. Its
        // non-method lines (header, blank) still surface via gap-filling.
        let source = "class Greeter:\n    def hello(self):\n        pass\n";
        let symbols = vec![
            sym("Greeter", SymbolKind::Class, 1, 3),
            sym("hello", SymbolKind::Method, 2, 3),
        ];
        let chunks = chunk_file(source, &symbols);
        assert!(
            chunks
                .iter()
                .all(|c| c.symbol_qn.as_deref() != Some("test.py::Greeter")),
            "container kind must not be chunked as its own span: {chunks:?}"
        );
        let gap: Vec<_> = chunks.iter().filter(|c| c.symbol_qn.is_none()).collect();
        assert_eq!(gap.len(), 1, "class header line gap-fills");
        assert_eq!(gap[0].line_start, 1);
        assert_eq!(gap[0].line_end, 1);
    }

    #[test]
    fn nested_function_produces_its_own_chunk_in_addition_to_outer() {
        let source = "def outer():\n    def inner():\n        pass\n    inner()\n";
        let symbols = vec![
            sym("outer", SymbolKind::Function, 1, 4),
            sym("inner", SymbolKind::Function, 2, 3),
        ];
        let chunks = chunk_file(source, &symbols);
        assert!(
            chunks
                .iter()
                .any(|c| c.symbol_qn.as_deref() == Some("test.py::outer"))
        );
        assert!(
            chunks
                .iter()
                .any(|c| c.symbol_qn.as_deref() == Some("test.py::inner"))
        );
        // No panics / no negative-width gap between the overlapping spans.
        assert!(chunks.iter().all(|c| c.line_start <= c.line_end));
    }
}
