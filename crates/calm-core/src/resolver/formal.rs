use std::collections::HashMap;
use std::time::Duration;

use stack_graphs::CancelAfterDuration as StackGraphCancelAfterDuration;
use stack_graphs::arena::Handle;
use stack_graphs::graph::{File, Node, StackGraph};
use stack_graphs::partial::{PartialPath, PartialPaths};
use stack_graphs::stitching::{
    Database, DatabaseCandidates, ForwardCandidates, ForwardPartialPathStitcher,
    GraphEdgeCandidates,
};
use tree_sitter_stack_graphs::CancelAfterDuration as TsgCancelAfterDuration;
use tree_sitter_stack_graphs::CancellationFlag as TsgCancellationFlag;
use tree_sitter_stack_graphs::NoCancellation as TsgNoCancellation;
use tree_sitter_stack_graphs::StackGraphLanguage;
use tree_sitter_stack_graphs::Variables;

fn cancellation_flag() -> &'static dyn TsgCancellationFlag {
    &TsgNoCancellation
}

pub struct FormalResolver {
    configs: HashMap<String, FormalLanguageConfig>,
}

struct FormalLanguageConfig {
    sgl: StackGraphLanguage,
    builtins: StackGraph,
    #[allow(dead_code)]
    no_similar_paths_in_file: bool,
    /// `.tsx` uses a different `StackGraphLanguage`/builtins pair than plain
    /// `.ts` (separate grammar variant upstream — `tree-sitter-typescript`
    /// ships `LANGUAGE_TYPESCRIPT` and `LANGUAGE_TSX` as distinct grammars).
    /// `ci`'s language string for both is the single `"typescript"`
    /// (`lang_constants.rs`), so the split is resolved here, keyed off
    /// `file_path`'s extension, rather than by adding a second top-level
    /// `configs` entry no caller would know to ask for.
    tsx: Option<TsxVariant>,
}

struct TsxVariant {
    sgl: StackGraphLanguage,
    builtins: StackGraph,
}

#[derive(Debug, Clone)]
pub struct FormalEdge {
    pub reference_symbol: String,
    pub definition_symbol: String,
}

/// Per-file deadline for Tier-3 formal resolution. Stack-graphs' path
/// stitching can take unbounded time on pathological files (e.g. one very
/// large class with thousands of self-references creates a combinatorial
/// blowup in the partial-path stitcher) — confirmed in practice on CPython's
/// `_pydecimal.py` (6.4k lines, ~9800 reference nodes), which never finished
/// stitching even after minutes. Tier-3 is a best-effort upgrade on top of
/// Tier-1/2 results, so a file that blows this budget simply falls back to
/// whatever Tier-1/2 already determined (see callers of `resolve_file`)
/// rather than stalling indexing indefinitely.
const RESOLVE_TIMEOUT: Duration = Duration::from_secs(3);

/// Cap on the amount of stitching work performed in a single phase before
/// yielding control back to check the cancellation deadline. stack-graphs
/// leaves `max_work_per_phase` unbounded (`usize::MAX`) by default and only
/// checks cancellation *between* phases — so without this cap, a single
/// phase on a pathological file can itself run for the file's entire blowup
/// before the deadline check below ever gets a chance to fire.
const MAX_WORK_PER_PHASE: usize = 4096;

/// Synthetic source for a virtual "<builtins>.py" file, standing in for
/// `tree-sitter-stack-graphs-python` 0.3.0's own bundled `src/builtins.py`
/// (which ships empty — see DEBT-005 / the regression test below for the
/// original investigation). Bodies are `pass` — only the *names* need to
/// exist as top-level definitions for references to resolve to; nothing
/// calls into these bodies. Not exhaustive, but covers the builtins that
/// show up in real code by a wide margin.
const PYTHON_BUILTINS_STUB: &str = r#"
def print(*args, **kwargs): pass
def len(obj): pass
def range(*args): pass
def isinstance(obj, cls): pass
def issubclass(cls, classinfo): pass
def super(*args): pass
def open(*args, **kwargs): pass
def enumerate(iterable, start=0): pass
def zip(*iterables): pass
def map(func, *iterables): pass
def filter(func, iterable): pass
def sorted(iterable, *, key=None, reverse=False): pass
def reversed(seq): pass
def min(*args, **kwargs): pass
def max(*args, **kwargs): pass
def sum(iterable, start=0): pass
def abs(x): pass
def round(number, ndigits=None): pass
def all(iterable): pass
def any(iterable): pass
def iter(obj, *args): pass
def next(iterator, *args): pass
def hasattr(obj, name): pass
def getattr(obj, name, *args): pass
def setattr(obj, name, value): pass
def delattr(obj, name): pass
def callable(obj): pass
def repr(obj): pass
def format(value, spec=""): pass
def id(obj): pass
def hash(obj): pass
def vars(*args): pass
def dir(*args): pass
def input(*args): pass
def staticmethod(func): pass
def classmethod(func): pass
def property(*args): pass

class object: pass
class type: pass
class str: pass
class int: pass
class float: pass
class bool: pass
class complex: pass
class list: pass
class dict: pass
class set: pass
class frozenset: pass
class tuple: pass
class bytes: pass
class bytearray: pass

class BaseException: pass
class Exception: pass
class ValueError: pass
class TypeError: pass
class KeyError: pass
class IndexError: pass
class AttributeError: pass
class StopIteration: pass
class RuntimeError: pass
class NotImplementedError: pass
class ZeroDivisionError: pass
class NameError: pass
class ImportError: pass
class OSError: pass
class FileNotFoundError: pass
class KeyboardInterrupt: pass
"#;

/// Builds a `StackGraph` holding definitions for `PYTHON_BUILTINS_STUB`,
/// reusing the *same* compiled TSG rules (`sgl`) the upstream crate uses for
/// ordinary files — no grammar patch needed. The FILE_PATH `"<builtins>.py"`
/// is the load-bearing part: the grammar's per-file module-path rule (the
/// branch that turns a file's relative path into a `pop_symbol` chain
/// anchored at ROOT_NODE) turns this exact path into a single
/// `pop_symbol = "<builtins>"` node hanging directly off ROOT_NODE — which is
/// precisely the counterpart every file's `push_symbol = "<builtins>"`
/// fallback edge (the one stack-graphs.tsg wires up for any reference that
/// falls through local scope) was missing.
fn build_python_builtins_graph(sgl: &StackGraphLanguage) -> anyhow::Result<StackGraph> {
    let mut graph = StackGraph::new();
    let file = graph.get_or_create_file("<builtins>.py");

    let mut globals = Variables::new();
    globals
        .add("FILE_PATH".into(), "<builtins>.py".into())
        .map_err(|_| anyhow::anyhow!("Failed to set FILE_PATH global for builtins"))?;

    let deadline = TsgCancelAfterDuration::new(RESOLVE_TIMEOUT);
    sgl.build_stack_graph_into(&mut graph, file, PYTHON_BUILTINS_STUB, &globals, &deadline)
        .map_err(|e| anyhow::anyhow!("Failed to build Python builtins stack graph: {e:?}"))?;

    Ok(graph)
}

/// Same as `ForwardPartialPathStitcher::find_minimal_partial_path_set_in_file`
/// (stack-graphs 0.14), but with `max_work_per_phase` bounded — see
/// `MAX_WORK_PER_PHASE` for why.
fn index_partial_paths_in_file(
    graph: &StackGraph,
    partials: &mut PartialPaths,
    file: Handle<File>,
    db: &mut Database,
    cancellation_flag: &dyn stack_graphs::CancellationFlag,
) -> Result<(), stack_graphs::CancellationError> {
    fn as_complete_as_necessary(graph: &StackGraph, path: &PartialPath) -> bool {
        path.starts_at_endpoint(graph) && (path.ends_at_endpoint(graph) || path.ends_in_jump(graph))
    }

    let initial_paths = graph
        .nodes_for_file(file)
        .chain(std::iter::once(StackGraph::root_node()))
        .filter(|node| graph[*node].is_endpoint())
        .map(|node| PartialPath::from_node(graph, partials, node))
        .collect::<Vec<_>>();
    let mut stitcher =
        ForwardPartialPathStitcher::from_partial_paths(graph, partials, initial_paths);
    stitcher.set_similar_path_detection(true); // matches StitcherConfig::default()
    stitcher.set_max_work_per_phase(MAX_WORK_PER_PHASE);
    stitcher.set_check_only_join_nodes(true);

    while !stitcher.is_complete() {
        cancellation_flag.check("indexing partial paths")?;
        stitcher.process_next_phase(
            &mut GraphEdgeCandidates::new(graph, partials, Some(file)),
            |g, _ps, p| !as_complete_as_necessary(g, p),
        );
        for path in stitcher.previous_phase_partial_paths() {
            if as_complete_as_necessary(graph, path) {
                db.add_partial_path(graph, partials, path.clone());
            }
        }
    }
    Ok(())
}

/// Same as `ForwardPartialPathStitcher::find_all_complete_partial_paths`
/// (stack-graphs 0.14), but with `max_work_per_phase` bounded — see
/// `MAX_WORK_PER_PHASE` for why.
fn find_all_complete_partial_paths_bounded<F>(
    candidates: &mut DatabaseCandidates<'_>,
    starting_nodes: Vec<Handle<Node>>,
    cancellation_flag: &dyn stack_graphs::CancellationFlag,
    mut visit: F,
) -> Result<(), stack_graphs::CancellationError>
where
    F: FnMut(&StackGraph, &mut PartialPaths, &PartialPath),
{
    let (graph, partials, _) = candidates.get_graph_partials_and_db();
    let initial_paths = starting_nodes
        .into_iter()
        .filter(|n| graph[*n].is_reference())
        .map(|n| {
            let mut p = PartialPath::from_node(graph, partials, n);
            p.eliminate_precondition_stack_variables(partials);
            p
        })
        .collect::<Vec<_>>();
    let mut stitcher =
        ForwardPartialPathStitcher::from_partial_paths(graph, partials, initial_paths);
    stitcher.set_similar_path_detection(true); // matches StitcherConfig::default()
    stitcher.set_max_work_per_phase(MAX_WORK_PER_PHASE);
    stitcher.set_check_only_join_nodes(true);

    while !stitcher.is_complete() {
        cancellation_flag.check("finding complete partial paths")?;
        for path in stitcher.previous_phase_partial_paths() {
            candidates.load_forward_candidates(path, cancellation_flag)?;
        }
        stitcher.process_next_phase(candidates, |_, _, _| true);
        let (graph, partials, _) = candidates.get_graph_partials_and_db();
        for path in stitcher.previous_phase_partial_paths() {
            if path.is_complete(graph) {
                visit(graph, partials, path);
            }
        }
    }
    Ok(())
}

impl FormalResolver {
    pub fn new() -> Self {
        Self {
            configs: HashMap::new(),
        }
    }

    pub fn load_python(&mut self) -> anyhow::Result<()> {
        let lc = tree_sitter_stack_graphs_python::try_language_configuration(cancellation_flag())
            .map_err(|e| anyhow::anyhow!("Failed to load Python stack-graphs config: {e}"))?;
        // Upstream's own `lc.builtins` is built from its bundled (empty)
        // src/builtins.py — replace it with our own, built through the same
        // `sgl` rules. See `build_python_builtins_graph` for why this alone
        // is enough to make builtins resolve, with no grammar patch.
        let builtins = build_python_builtins_graph(&lc.sgl)?;
        self.configs.insert(
            "python".to_string(),
            FormalLanguageConfig {
                sgl: lc.sgl,
                builtins,
                no_similar_paths_in_file: lc.no_similar_paths_in_file,
                tsx: None,
            },
        );
        Ok(())
    }

    /// TypeScript formal resolution, covering both `.ts` and `.tsx`.
    /// Unlike Python (`load_python`), upstream's bundled builtins source
    /// (`tree-sitter-stack-graphs-typescript`'s `src/builtins.ts`, ~10KB) is
    /// non-empty, so `lc.builtins` is used directly — no synthetic stub graph
    /// needed here (see `PYTHON_BUILTINS_STUB` for why Python needed one).
    pub fn load_typescript(&mut self) -> anyhow::Result<()> {
        let lc_ts = tree_sitter_stack_graphs_typescript::try_language_configuration_typescript(
            cancellation_flag(),
        )
        .map_err(|e| anyhow::anyhow!("Failed to load TypeScript stack-graphs config: {e}"))?;
        let lc_tsx = tree_sitter_stack_graphs_typescript::try_language_configuration_tsx(
            cancellation_flag(),
        )
        .map_err(|e| anyhow::anyhow!("Failed to load TSX stack-graphs config: {e}"))?;

        self.configs.insert(
            "typescript".to_string(),
            FormalLanguageConfig {
                sgl: lc_ts.sgl,
                builtins: lc_ts.builtins,
                no_similar_paths_in_file: lc_ts.no_similar_paths_in_file,
                tsx: Some(TsxVariant {
                    sgl: lc_tsx.sgl,
                    builtins: lc_tsx.builtins,
                }),
            },
        );
        Ok(())
    }

    pub fn resolve_file(
        &self,
        language: &str,
        file_path: &str,
        source: &str,
    ) -> anyhow::Result<Vec<FormalEdge>> {
        let config = self
            .configs
            .get(language)
            .ok_or_else(|| anyhow::anyhow!("No formal resolver for language: {language}"))?;

        // `.tsx` swaps in the TSX-specific `StackGraphLanguage`/builtins pair
        // (see `FormalLanguageConfig::tsx`) — every other caller (plain `.ts`,
        // Python, ...) falls through to the primary `sgl`/`builtins` below.
        let (sgl, builtins) = match &config.tsx {
            Some(tsx) if file_path.ends_with(".tsx") => (&tsx.sgl, &tsx.builtins),
            _ => (&config.sgl, &config.builtins),
        };

        // Fresh per-file deadlines for both the TSG graph build and the
        // path-stitching stages — see `RESOLVE_TIMEOUT`.
        let tsg_deadline = TsgCancelAfterDuration::new(RESOLVE_TIMEOUT);
        let sg_deadline = StackGraphCancelAfterDuration::new(RESOLVE_TIMEOUT);

        let mut graph = StackGraph::new();

        // Merge builtins (e.g. Python's `len`, `print`, `range`) into the working
        // graph so references to them can resolve — without this, `graph` only
        // ever contains the single file being analyzed and every builtin
        // reference is unresolvable by construction. `add_from_graph` copies the
        // builtins' files/nodes/edges in and returns their new file handles,
        // which also need indexing into `db` below so they're stitchable.
        let builtin_files = graph
            .add_from_graph(builtins)
            .map_err(|h| anyhow::anyhow!("Duplicate builtin file: {}", &graph[h]))?;

        let file = graph.get_or_create_file(file_path);

        let mut globals = Variables::new();
        globals
            .add("FILE_PATH".into(), file_path.into())
            .map_err(|_| anyhow::anyhow!("Failed to set FILE_PATH global"))?;

        sgl.build_stack_graph_into(&mut graph, file, source, &globals, &tsg_deadline)
            .map_err(|e| anyhow::anyhow!("Stack graph build error: {e:?}"))?;

        let mut partials = PartialPaths::new();
        let mut db = Database::new();

        // Index: find partial paths in this file.
        index_partial_paths_in_file(&graph, &mut partials, file, &mut db, &sg_deadline)
            .map_err(|e| anyhow::anyhow!("Partial path extraction cancelled: {e}"))?;

        // Also index partial paths for the merged builtins files, so a
        // reference in `file` can stitch all the way to a builtin definition.
        for builtin_file in &builtin_files {
            index_partial_paths_in_file(
                &graph,
                &mut partials,
                *builtin_file,
                &mut db,
                &sg_deadline,
            )
            .map_err(|e| anyhow::anyhow!("Builtins partial path extraction cancelled: {e}"))?;
        }

        // Resolve: find complete paths from references to definitions
        let reference_nodes: Vec<_> = graph
            .nodes_for_file(file)
            .filter(|&n| graph[n].is_reference())
            .collect();

        if reference_nodes.is_empty() {
            return Ok(Vec::new());
        }

        let mut edges = Vec::new();
        find_all_complete_partial_paths_bounded(
            &mut DatabaseCandidates::new(&graph, &mut partials, &mut db),
            reference_nodes,
            &sg_deadline,
            |g, _ps, path: &PartialPath| {
                let start = path.start_node;
                let end = path.end_node;

                if g[start].is_reference() && g[end].is_definition() {
                    let ref_sym = g[start].symbol().map(|s| g[s].to_string());
                    let def_sym = g[end].symbol().map(|s| g[s].to_string());

                    if let (Some(r), Some(d)) = (ref_sym, def_sym) {
                        edges.push(FormalEdge {
                            reference_symbol: r,
                            definition_symbol: d,
                        });
                    }
                }
            },
        )
        .map_err(|e| anyhow::anyhow!("Path stitching cancelled: {e}"))?;

        Ok(edges)
    }

    pub fn has_language(&self, language: &str) -> bool {
        self.configs.contains_key(language)
    }

    pub fn supported_languages(&self) -> Vec<&str> {
        self.configs.keys().map(|s| s.as_str()).collect()
    }
}

impl Default for FormalResolver {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Regression: a single large class with many self-references used to
    /// make stack-graphs' path stitching run unbounded (confirmed on
    /// CPython's `_pydecimal.py`, 6.4k lines / ~9800 reference nodes, which
    /// never completed even after minutes of wall-clock time). `resolve_file`
    /// must now bound this via `RESOLVE_TIMEOUT` + `MAX_WORK_PER_PHASE`
    /// instead of hanging indefinitely — see those constants for why.
    #[test]
    fn test_resolve_file_bounded_on_pathological_class() {
        let mut source = String::from("class Big:\n");
        for m in 0..300 {
            source.push_str(&format!("    def m{m}(self):\n        x = (\n"));
            for a in 0..30 {
                source.push_str(&format!("            self.a{a} +\n"));
            }
            source.push_str("            0\n        )\n");
        }

        let mut resolver = FormalResolver::new();
        resolver.load_python().unwrap();
        let t0 = std::time::Instant::now();
        let _ = resolver.resolve_file("python", "big.py", &source);
        assert!(
            t0.elapsed() < RESOLVE_TIMEOUT + std::time::Duration::from_secs(2),
            "resolve_file must respect its deadline even on pathological input, took {:?}",
            t0.elapsed()
        );
    }

    #[test]
    fn test_formal_resolver_new() {
        let resolver = FormalResolver::new();
        assert!(!resolver.has_language("python"));
        assert!(resolver.supported_languages().is_empty());
    }

    #[test]
    fn test_load_python() {
        let mut resolver = FormalResolver::new();
        resolver.load_python().unwrap();
        assert!(resolver.has_language("python"));
    }

    #[test]
    fn test_resolve_no_language() {
        let resolver = FormalResolver::new();
        let result = resolver.resolve_file("python", "test.py", "x = 1");
        assert!(result.is_err());
    }

    #[test]
    fn test_resolve_simple_python_def_ref() {
        let mut resolver = FormalResolver::new();
        resolver.load_python().unwrap();
        let source = r#"
def foo():
    pass

def bar():
    foo()
"#;
        let edges = resolver.resolve_file("python", "test.py", source).unwrap();
        let has_foo_edge = edges
            .iter()
            .any(|e| e.definition_symbol == "foo" || e.reference_symbol == "foo");
        assert!(
            has_foo_edge,
            "Should resolve foo() call to foo definition. Edges: {edges:?}"
        );
    }

    /// Regression for B6: `builtins` was loaded into `FormalLanguageConfig`
    /// but never merged into the per-file working graph (`StackGraph::new()`
    /// started empty every time), so nothing in `builtins` could ever be
    /// referenced, and a malformed/duplicate builtins graph would have gone
    /// unnoticed since it was never touched.
    ///
    /// NOTE on scope: this only verifies the merge happens correctly (file
    /// count grows, no `add_from_graph` error) — DEBT-005 covers the actual
    /// builtin *resolution* (see `test_resolve_file_resolves_python_builtins`
    /// below), since `config.builtins` here is now `ci`'s own
    /// `build_python_builtins_graph` output, not upstream's empty one.
    #[test]
    fn test_resolve_file_merges_builtins_without_error() {
        let mut resolver = FormalResolver::new();
        resolver.load_python().unwrap();
        let config = resolver.configs.get("python").unwrap();
        let builtins_file_count = config.builtins.iter_files().count();

        // Sanity: the merge path only matters once builtins actually has
        // file(s) to merge — if this ever goes back to 0, the test below
        // would pass vacuously.
        assert!(
            builtins_file_count > 0,
            "builtins graph should contain at least the <builtins> file"
        );

        let mut graph = StackGraph::new();
        let merged = graph
            .add_from_graph(&config.builtins)
            .expect("merging builtins into a fresh graph should never collide");
        assert_eq!(merged.len(), builtins_file_count);

        // resolve_file() must still work normally with the merge in place.
        let edges = resolver
            .resolve_file(
                "python",
                "test.py",
                "def foo():\n    pass\n\ndef bar():\n    foo()\n",
            )
            .unwrap();
        assert!(
            edges.iter().any(|e| e.definition_symbol == "foo"),
            "same-file resolution must still work after merging builtins. Edges: {edges:?}"
        );
    }

    /// DEBT-005: `len()` and `print()` must resolve to the synthetic
    /// `build_python_builtins_graph` definitions through the `formal` tier —
    /// the actual fix, not just "the merge doesn't crash" (see
    /// `test_resolve_file_merges_builtins_without_error` above).
    #[test]
    fn test_resolve_file_resolves_python_builtins() {
        let mut resolver = FormalResolver::new();
        resolver.load_python().unwrap();

        let edges = resolver
            .resolve_file(
                "python",
                "test.py",
                "def use_builtins():\n    print(len([1, 2, 3]))\n",
            )
            .unwrap();

        assert!(
            edges
                .iter()
                .any(|e| e.reference_symbol == "len" && e.definition_symbol == "len"),
            "len() must resolve through the formal tier. Edges: {edges:?}"
        );
        assert!(
            edges
                .iter()
                .any(|e| e.reference_symbol == "print" && e.definition_symbol == "print"),
            "print() must resolve through the formal tier. Edges: {edges:?}"
        );
    }

    /// A genuinely undefined name must still fail to resolve — the builtins
    /// fix must not make FormalResolver resolve *everything*.
    #[test]
    fn test_resolve_file_does_not_resolve_undefined_name() {
        let mut resolver = FormalResolver::new();
        resolver.load_python().unwrap();

        let edges = resolver
            .resolve_file(
                "python",
                "test.py",
                "def use_undefined():\n    return totally_undefined_xyz()\n",
            )
            .unwrap();

        assert!(
            !edges
                .iter()
                .any(|e| e.reference_symbol == "totally_undefined_xyz"),
            "a genuinely undefined name must not resolve. Edges: {edges:?}"
        );
    }

    #[test]
    fn test_resolve_python_no_refs() {
        let mut resolver = FormalResolver::new();
        resolver.load_python().unwrap();
        let source = "x = 1\n";
        let edges = resolver.resolve_file("python", "test.py", source).unwrap();
        // Simple assignment may or may not produce edges depending on rules
        // Just verify it doesn't crash
        let _ = edges;
    }

    #[test]
    fn test_resolve_python_class() {
        let mut resolver = FormalResolver::new();
        resolver.load_python().unwrap();
        let source = r#"
class MyClass:
    def method(self):
        pass

def use_class():
    obj = MyClass()
"#;
        let edges = resolver.resolve_file("python", "test.py", source).unwrap();
        let has_class_edge = edges
            .iter()
            .any(|e| e.definition_symbol == "MyClass" || e.reference_symbol == "MyClass");
        assert!(
            has_class_edge,
            "Should resolve MyClass() to class definition. Edges: {edges:?}"
        );
    }

    #[test]
    fn test_load_typescript() {
        let mut resolver = FormalResolver::new();
        resolver.load_typescript().unwrap();
        assert!(resolver.has_language("typescript"));
    }

    #[test]
    fn test_resolve_simple_typescript_def_ref() {
        let mut resolver = FormalResolver::new();
        resolver.load_typescript().unwrap();
        let source = r#"
function foo() {}

function bar() {
    foo();
}
"#;
        let edges = resolver
            .resolve_file("typescript", "test.ts", source)
            .unwrap();
        let has_foo_edge = edges
            .iter()
            .any(|e| e.definition_symbol == "foo" || e.reference_symbol == "foo");
        assert!(
            has_foo_edge,
            "Should resolve foo() call to foo definition. Edges: {edges:?}"
        );
    }

    #[test]
    fn test_resolve_typescript_class() {
        let mut resolver = FormalResolver::new();
        resolver.load_typescript().unwrap();
        let source = r#"
class MyClass {
    method(): void {}
}

function useClass() {
    const obj = new MyClass();
}
"#;
        let edges = resolver
            .resolve_file("typescript", "test.ts", source)
            .unwrap();
        let has_class_edge = edges
            .iter()
            .any(|e| e.definition_symbol == "MyClass" || e.reference_symbol == "MyClass");
        assert!(
            has_class_edge,
            "Should resolve MyClass() to class definition. Edges: {edges:?}"
        );
    }

    /// `.tsx` must resolve through the TSX-specific `sgl`/builtins pair, not
    /// silently fall through to the plain `.ts` one (which would fail to
    /// parse JSX syntax and produce zero edges instead of erroring loudly).
    #[test]
    fn test_resolve_tsx_def_ref() {
        let mut resolver = FormalResolver::new();
        resolver.load_typescript().unwrap();
        let source = r#"
function Foo() {
    return <div>hi</div>;
}

function Bar() {
    return Foo();
}
"#;
        let edges = resolver
            .resolve_file("typescript", "test.tsx", source)
            .unwrap();
        let has_foo_edge = edges
            .iter()
            .any(|e| e.definition_symbol == "Foo" || e.reference_symbol == "Foo");
        assert!(
            has_foo_edge,
            "Should resolve Foo() call to Foo definition in a .tsx file. Edges: {edges:?}"
        );
    }

    /// Unlike Python (DEBT-005), TypeScript's bundled builtins source is
    /// non-empty upstream, so `load_typescript` uses `lc.builtins` as-is
    /// (no synthetic stub). This locks in that global built-ins actually
    /// resolve through it rather than assuming so from source file size.
    ///
    /// Asserts on `Array`/`isArray`, not `console` — `console` is a host
    /// (DOM/Node) global, not part of core ECMAScript, so it's correctly
    /// absent from `builtins.ts` (confirmed empirically: an earlier version
    /// of this test asserted on `console` and failed, while `Array`/
    /// `isArray` resolved in the same run).
    #[test]
    fn test_resolve_file_resolves_typescript_builtins() {
        let mut resolver = FormalResolver::new();
        resolver.load_typescript().unwrap();

        let edges = resolver
            .resolve_file(
                "typescript",
                "test.ts",
                "function useBuiltins() {\n    return Array.isArray([1, 2, 3]);\n}\n",
            )
            .unwrap();

        assert!(
            edges
                .iter()
                .any(|e| e.reference_symbol == "Array" && e.definition_symbol == "Array"),
            "Array must resolve through the formal tier. Edges: {edges:?}"
        );
        assert!(
            edges
                .iter()
                .any(|e| e.reference_symbol == "isArray" && e.definition_symbol == "isArray"),
            "Array.isArray must resolve through the formal tier. Edges: {edges:?}"
        );
    }

    #[test]
    fn test_resolve_typescript_does_not_resolve_undefined_name() {
        let mut resolver = FormalResolver::new();
        resolver.load_typescript().unwrap();

        let edges = resolver
            .resolve_file(
                "typescript",
                "test.ts",
                "function useUndefined() {\n    return totallyUndefinedXyz();\n}\n",
            )
            .unwrap();

        assert!(
            !edges
                .iter()
                .any(|e| e.reference_symbol == "totallyUndefinedXyz"),
            "a genuinely undefined name must not resolve. Edges: {edges:?}"
        );
    }
}
