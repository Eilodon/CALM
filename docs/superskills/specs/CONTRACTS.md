# CONTRACTS.md — Schema Registry
> **DPS VERSION:** `5.0`
> **DPS PROFILE:** `DPS-Standard`
### Code Intelligence MCP · v2.7.2 · compatible with: [BLUEPRINT: architecture-design.md v2.7.2, IMPL: 2026-06-29-full-implementation-design.md]

> **Nguyên tắc vàng:** Mọi type, schema, enum, constant được define **MỘT LẦN DUY NHẤT** tại đây.
> BLUEPRINT.md và code **reference** — không redefine, không copy, không paraphrase.
>
> Khi thấy conflict giữa file này và bất kỳ file nào khác → file này thắng.


> **DPS STATUS:** `IMPLEMENTATION-ACTIVE`
> **PROMOTED BY:** ybao · **PROMOTED AT:** 2026-06-29
> **PROMOTION BASIS:** Derived from approved architecture-design.md v2.7.2 + full-implementation-design.md audit-design PASS WITH FLAGS
> **CURRENT AUTHORITY:** `DPS`

---

## Mục lục

1. [Primitive Types & Constants](#1-primitive-types--constants)
2. [Enums](#2-enums)
3. [Core Schemas](#3-core-schemas)
   - [3.X System Invariants](#3x-system-invariants)
4. [Input / Output Contracts](#4-input--output-contracts)
5. [Error Registry](#5-error-registry)
6. [External Contracts](#6-external-contracts)
7. [Naming Conventions](#7-naming-conventions)
8. [Schema Changelog](#8-schema-changelog)
9. [Deprecation Registry](#9-deprecation-registry)
10. [Glossary](#10-glossary)

---

## 1. PRIMITIVE TYPES & CONSTANTS

> Các kiểu và hằng số dùng xuyên suốt hệ thống.
> Agent KHÔNG hard-code giá trị của các constants này ở bất kỳ nơi nào khác.

```
MAX_MODULES_IN_OVERVIEW :: int = 50
  // module_map cap trong repo_overview. Vượt → truncated: true.

MAX_FETCHED_SESSION :: int = 200
  // FIFO cap cho session.already_fetched. Oldest bị drop silent.

MAX_EXPLORED_SYMBOLS :: int = 50
  // LRU cap cho session.explored_symbols. MRU-ordered output.

DEFAULT_SEARCH_LIMIT :: int = 10
  // search / locate result limit mặc định.

BM25_EXACT_WEIGHT :: float = 1.5
  // Hệ số nhân BM25 score từ fts_exact trong symbol search.

BM25_TOKENS_WEIGHT :: float = 1.0
  // Hệ số nhân BM25 score từ fts_tokens trong symbol search.

RRF_K_DEFAULT :: int = 20
  // Reciprocal Rank Fusion k parameter. k=20 cho discrimination tốt hơn k=60 trên short result lists.

EMBEDDING_DIMENSIONS :: int = 768
  // BAAI/bge-base-en-v1.5 output dimensions.

EMBEDDING_MODEL_DEFAULT :: string = "BAAI/bge-base-en-v1.5"
  // Mặc định. Option: "nomic-ai/nomic-embed-text-v1.5" (768-dim, 8192 context).

DEFAULT_MAX_HOPS :: int = 8
  // path tool: server default khi client không gửi max_hops.

MAX_ALLOWED_HOPS :: int = 20
  // path tool: clamp ceiling. Vượt → capped, hops_clamped: true.

PATH_TIMEOUT_MS :: int = 5000
  // path tool: BFS deadline mặc định.

TRANSITIVE_TIMEOUT_MS :: int = 3000
  // callers/callees transitive BFS timeout.

MAX_DEPTH_CAP :: int = 4
  // callers/callees transitive BFS max depth ceiling.

DEBOUNCE_RECOMPUTE_MS :: float = 500
  // Coreness recompute debounce after file change.

HUB_TOP_PCT :: float = 5.0
  // Top N% caller_count → degree-hub candidate.

HUB_MIN_CALLERS :: int = 5
  // Floor cho degree-hub path.

HUB_MIN_CALLERS_BRIDGE :: int = 2
  // [F12] Floor cho bridge-hub path (moderate in-degree, high coreness).

HUB_CORENESS_PCT :: float = 75.0
  // p75 coreness threshold cho bridge-hub detection.

RISK_CRITICAL_THRESHOLD :: float = 0.75
  // hotspot risk level boundary.

RISK_HIGH_THRESHOLD :: float = 0.50
RISK_MEDIUM_THRESHOLD :: float = 0.25

HOTSPOT_DEFAULT_TOP_N :: int = 10
HOTSPOT_DEFAULT_SINCE :: string = "6 months ago"
HOTSPOT_DEFAULT_MIN_CHURN :: int = 2

TEXT_CHUNK_CONTEXT_LINES :: int = 10
  // source/search text chunk context mở rộng.

TEXT_MAX_CHUNK_LINES :: int = 50
  // source text chunk max.
```

> **Type notation dùng trong file này:**
> ```
> FieldName :: Type                         — required field
> FieldName :: Type?                        — optional field (nullable)
> FieldName :: List<Type>                   — ordered list
> FieldName :: Map<KeyType, ValueType>      — map / dict
> FieldName :: TypeA | TypeB               — union type (chọn một)
> FieldName :: Ref<SchemaName>              — reference đến schema khác
> FieldName :: Result<OkType, ErrCode>     — success hoặc typed error (không dùng exception)
> FieldName :: (TypeA, TypeB, TypeC)       — tuple, thứ tự có ý nghĩa, không thay đổi
> FieldName :: ~ExpressionOrField          — derived/computed từ fields khác, KHÔNG persist vào DB
> ```

---

## 2. ENUMS

> Mọi enum được define tại đây. Không tạo inline enum trong schema.

### IndexingPhase

```
IndexingPhase ::
  | scanning          // File enumeration, hash check, DB init, migration
  | parsing           // Symbol extraction, import/call edge collection
  | building_edges    // Edge resolution, coreness computation, is_hub flag
  | ready             // Full graph sẵn sàng, edges_ready: true
```

**Dùng ở:** `RepoOverviewOutput`, `IndexingStatusOutput`, `IndexerState`
**Không dùng cho:** Embedding status (track riêng bằng EmbedStatus)

### EmbedStatus

```
EmbedStatus ::
  | disabled          // Config semantic_search.enabled = false (default)
  | downloading       // Model đang download lần đầu
  | embedding         // Model loaded, đang embed symbols
  | ready             // Embedding hoàn tất, hybrid/semantic search sẵn sàng
  | failed            // Lỗi — xem EmbedError.reason
```

**Dùng ở:** `RepoOverviewOutput`, `IndexingStatusOutput`, `EmbedderState`

### SearchKind

```
SearchKind ::
  | symbol            // FTS5 dual-column BM25 (fts_exact × 1.5 + fts_tokens × 1.0)
  | text              // FTS5 fts_exact trên docstring column only
  | file              // Path matching
  | semantic          // Cosine similarity trên embedding_vecs (requires embeddings ready)
  | hybrid            // RRF merge: FTS + semantic (degraded → FTS-only khi embeddings chưa ready)
```

**Dùng ở:** `SearchInput`, `LocateInput`, `UnderstandInput`
**Không dùng cho:** file_overview (nhận path trực tiếp, không search)

### EdgeConfidence

```
EdgeConfidence ::
  | resolved          // Callee defined cùng file, explicit import, hoặc alias confirmed. Reliable.
  | inferred          // Callee type inferred qua import + type hints (hoặc alias type). Mostly reliable.
  | textual           // Name-only match. Dễ false positive cả hai chiều.
```

**Dùng ở:** `call_edges.edge_confidence`, `callers.direct[]`, `callees.direct[]`, `path.routes[].steps[]`, `edit_context.callers[]`, `edit_context.callees[]`, `diff_impact.high_risk_callers[]`

### SymbolKind

```
SymbolKind ::
  | function          // Top-level function (Python, Go, Rust fn, JS/TS)
  | class             // Class definition
  | method            // Method inside class/struct/impl
  | interface         // Interface/Protocol/Trait definition
  | type              // Type alias (TS type, Rust type alias)
  | variable          // Module-level variable/constant
  | enum              // Enum definition
  | constructor       // Constructor (Java, TS)
  | struct            // Rust struct, Go struct
  | trait             // Rust trait (alias for interface in Rust context)
  | impl              // Rust impl block
```

**Dùng ở:** `symbols.kind`, `Symbol` dataclass, `SymbolInfoOutput`, `FileOverviewOutput.symbols[]`

### DeadCodeConfidence

```
DeadCodeConfidence ::
  | none              // Has callers OR is entry point — not dead
  | low               // No callers but runtime-covered (dynamic dispatch/reflection), OR scope unclear
  | medium            // No callers, no runtime coverage, scope clear but not private
  | high              // No callers, private scope, not entry point — strongest dead code signal
```

**Dùng ở:** `Health.dead_code_confidence`

### DeadCodeSource

```
DeadCodeSource ::
  | static            // Only static analysis used (no coverage file found)
  | static+coverage   // Static analysis + runtime coverage data cross-referenced
```

**Dùng ở:** `Health.dead_code_source`
**Không dùng cho:** DeadCodeConfidence — khác concept: source = data inputs, confidence = conclusion

### RiskLevel

```
RiskLevel ::
  | low               // score < 0.25 (hotspots) or low blast radius (diff_impact)
  | medium            // score ∈ [0.25, 0.50)
  | high              // score ∈ [0.50, 0.75)
  | critical          // score >= 0.75
  | unknown           // diff_impact only: unindexed files present, cannot estimate
```

**Dùng ở:** `HotspotsOutput.hotspots[].risk_level`, `DiffImpactOutput.aggregate_risk`, `EditContextOutput.risk_assessment`

### LocateDepth

```
LocateDepth ::
  | search_only       // Identical to search tool, no enrichment
  | with_file         // search + file_overview of top result's file
  | with_symbol       // search + file_overview + symbol_info of top result (DEFAULT)
```

**Dùng ở:** `LocateInput.depth`

### HotspotMethod

```
HotspotMethod ::
  | git+index         // Git churn data available + index complexity
  | index_only        // Git unavailable, ranking by complexity only
```

**Dùng ở:** `HotspotsOutput.hotspot_method`

### TerminatedBy

```
TerminatedBy ::
  | timeout           // BFS interrupted by deadline
  | max_hops          // f_depth + b_depth >= max_hops
  | path_count        // Found enough paths (>= max_paths)
```

**Dùng ở:** `PathOutput.terminated_by`

### ChangeType

```
ChangeType ::
  | modified          // Default cho mọi symbol bị touch bởi diff
  | added             // diff header có --- /dev/null (file mới)
  | deleted           // diff header có +++ /dev/null (file xoá)
  | renamed           // git diff -M detect (staged/commits mode only; KHÔNG detect trong diff mode)
```

**Dùng ở:** `DiffImpactOutput.affected_symbols[].change_type`

### FrontierReason

```
FrontierReason ::
  | imported_by_explored          // File imports explored files
  | contains_callers_of_explored  // File contains callers of explored symbols
  | both                          // Both conditions met
```

**Dùng ở:** `SessionContextOutput.frontier[].reason`

### ErrorCode

```
ErrorCode ::
  | NOT_FOUND            // Symbol/file không tồn tại trong index
  | INDEX_PARTIAL        // Index chưa complete; retry sau
  | PARSE_FAILED         // File có syntax error
  | TIMEOUT              // BFS/query timeout
  | DB_LOCKED            // SQLite write contention (rare với WAL)
  | INVALID_INPUT        // Bad params
  | FEATURE_UNAVAILABLE  // Feature cần enable trong config
  | EMBEDDING_FAILED     // Download/embedding lỗi
```

**Dùng ở:** `UnifiedError.error.code`

### ToolPreset

```
ToolPreset ::
  | orient     // {repo_overview, locate, dependencies, hotspots, indexing_status}
  | trace      // {repo_overview, locate, callers, callees, path, session_context}
  | edit       // {repo_overview, locate, source, edit_context, diff_impact, indexing_status}
  | compound   // {repo_overview, locate, hotspots, source, understand, edit_context, diff_impact, session_context, indexing_status}
  | full       // All 16 tools (None = no filter)
```

**Dùng ở:** `Config.preset`, CLI `--preset` flag, `PRESET_TOOL_SETS`

### EmbedErrorReason

```
EmbedErrorReason ::
  | download_failed   // Model download failed (network/auth)
  | model_corrupt     // Downloaded model corrupt
  | oom               // Out of memory during embedding
  | embed_failed      // Other embedding failure
```

**Dùng ở:** `EmbedError.reason`

---

## 3. CORE SCHEMAS

> Schemas được sắp xếp từ primitive → composite.

---

### SuggestedNext

> Sat-Nav hint nhúng vào mọi tool response — agent không cần tự suy luận "gọi gì tiếp".
> **Owner:** server.py
> **Decision origin:** Pre-ADR design — Zero-Round-Trip philosophy: reduce agent inference rounds.

```
SuggestedNext :: {
  tool     :: string                  // Tool name to call next
  reason   :: string                  // Human-readable rationale
  args     :: Map<string, any>?       // Pre-filled args cho next tool — absent khi empty
}
```

**Constraints:**
```
INVARIANT: tool phải nằm trong available_tools (preset-filtered) hoặc bị drop
INVARIANT: args keys phải match target tool's input schema — no unknown keys
```

**Không được nhầm với:** `workflow_guide` — guide là static text, suggested_next là dynamic per-response.

---

### Health

> Health signals cho một symbol — dead code confidence, test coverage, caller breakdown.
> **Owner:** analyzer.py + tool handlers
> **Decision origin:** Pre-ADR design — Coverage Reader cross-reference giải quyết false-positive dead code.

```
Health :: {
  dead_code_confidence      :: Ref<DeadCodeConfidence>          // xem Glossary: Dead Code Confidence
  dead_code_source          :: Ref<DeadCodeSource>              // MANDATORY khi Health present — "static" | "static+coverage"
  caller_count_by_confidence :: Map<string, int>?               // {"resolved": N, "inferred": N, "textual": N} — null khi edges_ready: false
  test_files                :: List<string>                     // Test files that reference this symbol — [] khi none found
}
```

**Constraints:**
```
INVARIANT: dead_code_source luôn present khi Health object tồn tại — mandatory within the type
INVARIANT: caller_count_by_confidence = null khi edges_ready = false
INVARIANT: Health itself optional trong UnderstandOutput (absent khi ambiguous), nhưng khi present, tất cả fields phải có
```

**Không được nhầm với:** `RiskLevel` (hotspots/diff_impact) — Health là symbol-level health, RiskLevel là file-level hoặc change-level risk.

---

### AmbiguousResult

> Trả về khi input match nhiều symbols và không có path disambiguate.
> **Owner:** tools/search.py (_resolve_symbol)
> **Decision origin:** Pre-ADR design — Ambiguity Contract shared bởi 6 tools.

```
AmbiguousResult :: {
  ambiguous   :: bool = true          // Always true khi type này xuất hiện
  candidates  :: List<AmbiguousCandidate>  // Max 10 candidates
}
```

---

### AmbiguousCandidate

> Một candidate trong ambiguous result.
> **Owner:** tools/search.py
> **Decision origin:** Pre-ADR design — Base 5 fields ở mọi tool, extended fields ở symbol-rich tools.

```
AmbiguousCandidate :: {
  // Base fields (có ở tất cả 6 ambiguity-contract tools):
  name          :: string
  path          :: string
  kind          :: Ref<SymbolKind>
  line_start    :: int
  line_end      :: int
  // Extended fields (chỉ ở symbol_info, source, locate, understand):
  class_context :: string?            // Enclosing class name — absent khi top-level
  caller_count  :: int?
  language      :: string?
  signature     :: string?
}
```

---

### ModuleMapEntry

> Entry trong repo_overview module map — summary một file.
> **Owner:** tools/meta.py
> **Decision origin:** Pre-ADR design

```
ModuleMapEntry :: {
  path          :: string
  language      :: string
  symbol_count  :: int
  hub_count     :: int
  inferred_role :: string?            // First-match heuristic — null khi không match
}
```

---

### EntryPoint

> Detected entry point trong codebase.
> **Owner:** tools/meta.py
> **Decision origin:** Pre-ADR design

```
EntryPoint :: {
  name          :: string
  path          :: string
  kind          :: Ref<SymbolKind>
  line_start    :: int
}
```

---

### RepoStats

> Aggregate statistics cho repo_overview.
> **Owner:** tools/meta.py
> **Decision origin:** Pre-ADR design

```
RepoStats :: {
  total_symbols :: int?               // null khi phase != ready
  total_files   :: int
  total_edges   :: int?               // null khi phase != ready
  hub_count     :: int?               // null khi phase != ready
}
```

---

### HealthSummary

> Aggregate health cho repo_overview.
> **Owner:** tools/meta.py
> **Decision origin:** Pre-ADR design

```
HealthSummary :: {
  undocumented_hubs   :: int          // Hubs without docstring
  high_dead_code      :: int          // Symbols with dead_code_confidence = "high"
}
```

---

### ReviewerSuggestion

> Suggested reviewer cho diff_impact khi aggregate_risk >= medium.
> **Owner:** tools/analysis.py + codeowners.py
> **Decision origin:** Pre-ADR design — CODEOWNERS integration, git_blame fallback.

```
ReviewerSuggestion :: {
  name    :: string                   // Owner pattern or git email
  source  :: string                   // "codeowners" | "git_blame"
  files   :: List<string>             // Files matched by this reviewer
}
```

---

### EmbedError

> Error state cho embedding pipeline.
> **Owner:** indexer/embedder.py
> **Decision origin:** Pre-ADR design — State machine for opt-in embeddings.

```
EmbedError :: {
  reason      :: Ref<EmbedErrorReason>
  message     :: string
  retry_count :: int = 0
}
```

---

### CoverageData

> In-memory coverage map. Loaded once at startup, immutable within session.
> **Owner:** coverage_reader.py
> **Decision origin:** Pre-ADR design — H-9: no auto-reload, restart to refresh.

```
CoverageData :: {
  source          :: string                       // "lcov" | "python" | "go" | "cobertura" | "none"
  covered_lines   :: Map<string, Set<int>>        // {absolute_path: set of covered line numbers}
}
```

**Constraints:**
```
INVARIANT: source = "none" ↔ covered_lines is empty
INVARIANT: source immutable within session — "static" → "static+coverage" transition requires server restart
```

---

## DB Schemas (SQLite Tables)

### symbols (table)

> Source of truth cho mọi tool. FTS5-backed via triggers.
> **Owner:** db/schema.py (CREATE), indexer/indexer.py (WRITE), tools/* (READ)
> **Decision origin:** Pre-ADR design — qualified_name UNIQUE cross-file.

```
symbols :: {
  id              :: int AUTOINCREMENT PRIMARY KEY
  qualified_name  :: string NOT NULL UNIQUE    // "pkg.ClassName.method" — unique cross-file — xem Glossary: Qualified Name
  name            :: string NOT NULL           // Bare name "method"
  kind            :: Ref<SymbolKind> NOT NULL
  language        :: string NOT NULL
  path            :: string NOT NULL
  line_start      :: int NOT NULL
  line_end        :: int NOT NULL
  signature       :: string NOT NULL DEFAULT ''
  docstring       :: string NOT NULL DEFAULT ''
  name_tokens     :: string NOT NULL DEFAULT ''  // tokenize_identifier(name), pre-computed — xem Glossary: Name Tokens
  caller_count    :: int NOT NULL DEFAULT 0
  is_hub          :: int NOT NULL DEFAULT 0      // 0 | 1
  coreness        :: int?                        // NULL cho đến khi building_edges xong — xem Glossary: Coreness
  is_entry_point  :: int NOT NULL DEFAULT 0      // 0 | 1
  file_hash       :: string NOT NULL DEFAULT ''
  indexed_at      :: float NOT NULL DEFAULT 0    // time.time() epoch
}
```

**Indexes:**
```
idx_symbols_qualified  ON symbols(qualified_name)          -- UNIQUE
idx_symbols_path       ON symbols(path)
idx_symbols_name       ON symbols(name)
idx_symbols_hub        ON symbols(is_hub) WHERE is_hub = 1 -- partial index
```

**Constraints:**
```
INVARIANT: qualified_name UNIQUE — collision policy: last-write-wins per file (DELETE WHERE path=? then INSERT)
INVARIANT: coreness = NULL khi phase != ready; coreness = 0 khi isolated (phase = ready); coreness > 0 khi in k-core
INVARIANT: is_hub computed by update_is_hub_flags() AFTER coreness — never set independently
```

### call_edges (table)

> Call graph edges from ConservativeResolver.
> **Owner:** db/schema.py (CREATE), indexer/indexer.py (WRITE), tools/graph.py (READ)
> **Decision origin:** Pre-ADR design — 3-tier confidence tracking.

```
call_edges :: {
  id              :: int AUTOINCREMENT PRIMARY KEY
  from_symbol     :: string NOT NULL              // qualified_name of caller
  to_symbol       :: string NOT NULL              // qualified_name of callee
  call_site_line  :: int?                         // Line number of call site
  edge_confidence :: Ref<EdgeConfidence> NOT NULL DEFAULT 'textual'
  from_path       :: string?
  to_path         :: string?
}
```

**Indexes:**
```
idx_call_edges_from  ON call_edges(from_symbol)
idx_call_edges_to    ON call_edges(to_symbol)
idx_call_edges_fpath ON call_edges(from_path)
```

### import_edges (table)

> File-level dependency graph.
> **Owner:** db/schema.py (CREATE), indexer/indexer.py (WRITE), tools/graph.py (READ)
> **Decision origin:** Pre-ADR design

```
import_edges :: {
  id            :: int AUTOINCREMENT PRIMARY KEY
  from_path     :: string NOT NULL
  to_path       :: string?                        // NULL nếu external package / unresolved
  module_name   :: string NOT NULL
  symbols_used  :: string DEFAULT '[]'            // JSON array: ["func_a", "ClassB"]
}
```

**Indexes:**
```
idx_import_from ON import_edges(from_path)
idx_import_to   ON import_edges(to_path)
```

### file_index (table)

> Incremental indexing state tracker.
> **Owner:** db/schema.py (CREATE), indexer/indexer.py (WRITE)
> **Decision origin:** Pre-ADR design

```
file_index :: {
  path          :: string PRIMARY KEY
  hash          :: string NOT NULL                // SHA-256 of file content
  language      :: string?
  symbol_count  :: int NOT NULL DEFAULT 0
  last_indexed  :: float NOT NULL                 // time.time() epoch
  mtime         :: float?                         // File modification time — optional
}
```

### FTS5 Virtual Tables

```
fts_exact :: VIRTUAL TABLE fts5(
  name,                                           // Symbol bare name
  docstring,                                      // Symbol docstring
  content='symbols', content_rowid='id',
  tokenize='unicode61'
)

fts_tokens :: VIRTUAL TABLE fts5(
  name_tokens,                                    // tokenize_identifier(name) output
  content='symbols', content_rowid='id',
  tokenize='unicode61'
)
```

### FTS5 Sync Triggers

```
symbols_ai :: AFTER INSERT ON symbols
  → INSERT INTO fts_exact(rowid, name, docstring)
  → INSERT INTO fts_tokens(rowid, name_tokens)

symbols_ad :: AFTER DELETE ON symbols
  → DELETE from fts_exact (content-sync protocol)
  → DELETE from fts_tokens (content-sync protocol)

symbols_au :: AFTER UPDATE OF name, docstring, name_tokens ON symbols
  → DELETE old + INSERT new for both fts_exact and fts_tokens
```

### embedding_vecs (opt-in virtual table)

```
embedding_vecs :: VIRTUAL TABLE vec0(
  symbol_id   :: int,
  embedding   :: FLOAT[768]                       // Dimensions = config.semantic_search.dimensions
)
// Tạo khi config.semantic_search.enabled = true
// REQUIRES: sqlite_vec.load(conn) BEFORE CREATE VIRTUAL TABLE
```

---

## Indexer Dataclasses (Python)

### Symbol

> Extracted symbol from tree-sitter parse.
> **Owner:** indexer/parsers/base.py
> **Decision origin:** Pre-ADR design

```
Symbol :: {
  name            :: string
  qualified_name  :: string           // "module.Class.method"
  kind            :: Ref<SymbolKind>
  line_start      :: int
  line_end        :: int
  signature       :: string
  docstring       :: string
  name_tokens     :: string           // tokenize_identifier(name), pre-computed
  is_entry_point  :: bool = false
}
```

### CallSite

> Call site extracted from tree-sitter parse.
> **Owner:** indexer/parsers/base.py
> **Decision origin:** Pre-ADR design

```
CallSite :: {
  callee_name :: string               // Name of called function/method
  line        :: int                   // Call site line (singular, not range)
  in_symbol   :: string               // qualified_name of enclosing symbol
}
```

### ImportEdge (dataclass)

> Import relationship extracted from tree-sitter parse.
> **Owner:** indexer/parsers/base.py
> **Decision origin:** Pre-ADR design

```
ImportEdge :: {
  module_name    :: string
  resolved_path  :: string?           // NULL khi external package / unresolved
  symbols_used   :: List<string>
}
```

### ParseResult

> Complete parse output for one file.
> **Owner:** indexer/parsers/base.py
> **Decision origin:** Pre-ADR design

```
ParseResult :: {
  path       :: string
  language   :: string
  file_hash  :: string                // SHA-256
  symbols    :: List<Ref<Symbol>>
  call_sites :: List<Ref<CallSite>>
  imports    :: List<Ref<ImportEdge>>
}
```

### ResolvedEdge

> Output of ConservativeResolver.resolve().
> **Owner:** resolver/conservative.py
> **Decision origin:** Pre-ADR design — 3-tier confidence.

```
ResolvedEdge :: {
  from_symbol     :: string           // qualified_name of caller
  to_symbol       :: string           // qualified_name of callee
  call_site_line  :: int
  confidence      :: Ref<EdgeConfidence>  // resolved | inferred | textual
  from_path       :: string
  to_path         :: string?
}
```

---

## Runtime State Schemas

### ServerContext

> Dependency container cho mọi tool handler. Created once at startup.
> **Owner:** context.py
> **Decision origin:** Pre-ADR design — single writer + N readers pattern.

```
ServerContext :: {
  project_root       :: Path
  db_path            :: Path
  config             :: Ref<Config>
  write_conn         :: sqlite3.Connection    // Dành riêng cho Indexer (1 writer)
  write_lock         :: threading.Lock        // Bảo vệ write_conn
  coverage_data      :: Ref<CoverageData>
  codeowners_patterns :: List<any>
  indexer_state      :: Ref<IndexerState>
  session            :: Ref<SessionState>?    // Khởi tạo sau 1st request
}
```

**Constraints:**
```
INVARIANT: write_conn chỉ dùng trong Indexer pipeline — tool handlers dùng make_read_conn()
INVARIANT: make_read_conn() tạo connection mới mỗi lần, PRAGMA query_only=ON
```

### IndexerState

> Mutable state tracked by indexer pipeline.
> **Owner:** context.py
> **Decision origin:** Pre-ADR design

```
IndexerState :: {
  phase           :: Ref<IndexingPhase> = "scanning"
  files_indexed   :: int = 0
  files_total     :: int = 0
  symbols_indexed :: int?
  edges_indexed   :: int?
  last_updated    :: string = ""
  embedder        :: Ref<EmbedderState>?
}
```

### SessionState

> In-memory session tracking for session_context tool.
> **Owner:** context.py
> **Decision origin:** Pre-ADR design — compact log, no full response.

```
SessionState :: {
  started_at            :: string                          // ISO 8601 UTC
  tool_calls            :: int = 0
  explored_symbols      :: OrderedDict<string, dict>       // LRU-eviction khi > MAX_EXPLORED_SYMBOLS
  explored_files        :: Set<string>
  already_fetched       :: Deque<dict, maxlen=MAX_FETCHED_SESSION>  // FIFO-capped
  unique_files_explored :: int = 0
}
```

### EmbedderState

> State machine cho embedding pipeline.
> **Owner:** indexer/embedder.py
> **Decision origin:** Pre-ADR design

```
EmbedderState :: {
  status :: Ref<EmbedStatus> = "disabled"
  error  :: Ref<EmbedError>?
}
```

---

## Config Schema

### Config

> Root configuration. Loaded from config.json or .codeindex/config.json. Falls back to defaults.
> **Owner:** config.py
> **Decision origin:** Pre-ADR design

```
Config :: {
  preset          :: Ref<ToolPreset> = "full"
  languages       :: List<string> = ["python", "typescript", "javascript", "java", "rust", "go"]
  ignore          :: List<string> = ["node_modules", ".git", "__pycache__", "*.min.js", "dist", "build", ".venv"]
  entry_points    :: List<string> = []
  hub_threshold   :: Ref<HubThresholdConfig>
  call_graph      :: Ref<CallGraphConfig>
  semantic_search :: Ref<SemanticSearchConfig>
  search          :: Ref<SearchConfig>
  path            :: Ref<PathConfig>
  callers         :: Ref<DepthConfig>
  callees         :: Ref<DepthConfig>
  hotspots        :: Ref<HotspotsConfig>
}
```

### HubThresholdConfig

```
HubThresholdConfig :: {
  top_pct             :: float = 5.0       // = HUB_TOP_PCT
  min_callers         :: int = 5           // = HUB_MIN_CALLERS
  min_callers_bridge  :: int = 2           // = HUB_MIN_CALLERS_BRIDGE [F12]
  coreness_pct        :: float = 75.0      // = HUB_CORENESS_PCT
}
```

### CallGraphConfig

```
CallGraphConfig :: {
  resolver              :: string = "conservative"
  confidence_tracking   :: bool = true
}
```

### SemanticSearchConfig

```
SemanticSearchConfig :: {
  enabled           :: bool = false
  model             :: string = "BAAI/bge-base-en-v1.5"     // = EMBEDDING_MODEL_DEFAULT
  dimensions        :: int = 768                              // = EMBEDDING_DIMENSIONS
  index_on_startup  :: bool = false
}
```

### SearchConfig

```
SearchConfig :: {
  text_chunk_context_lines :: int = 10     // = TEXT_CHUNK_CONTEXT_LINES
  text_max_chunk_lines     :: int = 50     // = TEXT_MAX_CHUNK_LINES
  rrf_k                    :: int = 20     // = RRF_K_DEFAULT
}
```

### PathConfig

```
PathConfig :: {
  default_max_hops :: int = 8              // = DEFAULT_MAX_HOPS
  max_allowed_hops :: int = 20             // = MAX_ALLOWED_HOPS
  timeout_ms       :: int = 5000           // = PATH_TIMEOUT_MS
}
```

### DepthConfig

```
DepthConfig :: {
  max_depth_cap        :: int = 4          // = MAX_DEPTH_CAP
  transitive_timeout_ms :: int = 3000      // = TRANSITIVE_TIMEOUT_MS
}
```

### HotspotsConfig

```
HotspotsConfig :: {
  default_top_n           :: int = 10      // = HOTSPOT_DEFAULT_TOP_N
  default_since           :: string = "6 months ago"  // = HOTSPOT_DEFAULT_SINCE
  default_min_churn       :: int = 2       // = HOTSPOT_DEFAULT_MIN_CHURN
  risk_critical_threshold :: float = 0.75  // = RISK_CRITICAL_THRESHOLD
  risk_high_threshold     :: float = 0.50  // = RISK_HIGH_THRESHOLD
  risk_medium_threshold   :: float = 0.25  // = RISK_MEDIUM_THRESHOLD
}
```

---

## 3.X. SYSTEM INVARIANTS

### FTS5_SYNC

```
INVARIANT   : FTS5 virtual tables (fts_exact, fts_tokens) must ALWAYS be in sync with symbols table.
              Every INSERT/DELETE/UPDATE on symbols MUST trigger corresponding FTS5 operations.

SCOPE       : Components : db/schema.py (triggers), indexer/indexer.py (writes)
              Schemas    : `symbols`, `fts_exact`, `fts_tokens`

ENFORCE BY  : db/schema.py (via SQLite triggers — application logic NEVER manually syncs FTS5)

VIOLATED WHEN: symbols table has rows not reflected in FTS5, or FTS5 has orphan entries
TEST REQUIRED: INSERT symbol → fts_exact searchable; DELETE symbol → fts_exact clean; UPDATE name → fts_exact reflects new name
```

### CORENESS_CONSISTENCY

```
INVARIANT   : coreness = NULL ↔ phase != "ready"; coreness = 0 khi isolated (phase = ready); coreness > 0 khi in k-core.
              Never emit coreness: null khi edges_ready: true.

SCOPE       : Components : db_init.py (compute_coreness), indexer/indexer.py (phase transition), tools/* (read)
              Schemas    : `symbols`, `IndexerState`

ENFORCE BY  : indexer/indexer.py (_compute_graph_metrics — sets coreness for ALL symbols before phase → ready)

VIOLATED WHEN: phase = "ready" but any symbol has coreness = NULL
TEST REQUIRED: ASSERT all symbols have coreness != NULL when phase = "ready"
```

### SINGLE_WRITER

```
INVARIANT   : Only one thread writes to the database at any time. All writes go through write_conn + write_lock.

SCOPE       : Components : context.py (ServerContext), indexer/indexer.py, indexer/watcher.py
              Schemas    : All DB tables

ENFORCE BY  : context.py (write_lock: threading.Lock — acquired before any write_conn operation)

VIOLATED WHEN: Two threads execute write_conn operations without acquiring write_lock
TEST REQUIRED: Concurrent indexer + watcher writes do not produce sqlite3.OperationalError
```

### WATCHER_PHASE_GATE

```
INVARIANT   : File watcher MUST NOT write call_edges during building_edges phase.
              Watcher queues events during building_edges, drains after phase = ready.

SCOPE       : Components : indexer/watcher.py, indexer/indexer.py
              Schemas    : `call_edges`, `IndexerState`

ENFORCE BY  : indexer/watcher.py (check phase before writing; queue if building_edges)

VIOLATED WHEN: Watcher writes call_edges while indexer is in building_edges phase → stale file_symbols snapshot
TEST REQUIRED: File change during building_edges → reindex happens AFTER phase = ready
```

### DEAD_CODE_SOURCE_PRESENT

```
INVARIANT   : dead_code_source is ALWAYS present when Health object exists — mandatory within the type.

SCOPE       : Components : tools/search.py (symbol_info), tools/meta.py (source), tools/compound.py (understand)
              Schemas    : `Health`

ENFORCE BY  : Health Pydantic model (dead_code_source is required field, not Optional)

VIOLATED WHEN: Health object emitted without dead_code_source field
TEST REQUIRED: Every symbol_info response with health block has dead_code_source
```

---

## 4. INPUT / OUTPUT CONTRACTS

---

### Tool 1: repo_overview

> Khởi đầu mọi session — overview toàn bộ repo. ALWAYS call FIRST.

```
INPUT  :: {} (no parameters)

OUTPUT :: Ref<RepoOverviewOutput>
       | Ref<UnifiedError>

SIDE EFFECTS: none

PRE-CONDITIONS:
  - ServerContext initialized

POST-CONDITIONS:
  - Session tracking records tool call

IDEMPOTENT: CÓ
```

**RepoOverviewOutput:**
```
RepoOverviewOutput :: {
  languages         :: List<string>
  indexing_phase     :: Ref<IndexingPhase>
  embeddings_status  :: Ref<EmbedStatus>
  module_map         :: List<Ref<ModuleMapEntry>>     // Capped at MAX_MODULES_IN_OVERVIEW, sorted by symbol_count DESC
  total_modules      :: int
  truncated          :: bool
  entry_points       :: List<Ref<EntryPoint>>
  stats              :: Ref<RepoStats>
  workflow_guide     :: string
  health_summary     :: Ref<HealthSummary>?           // ABSENT khi None
  note               :: string?                        // ABSENT khi None
  suggested_next     :: Ref<SuggestedNext>?            // ABSENT khi None
}
```

---

### Tool 2: search

> FTS5 dual-column search. Replaces native grep/text search.

```
INPUT  :: Ref<SearchInput>

OUTPUT :: Ref<SearchOutput>
       | Ref<UnifiedError>    // FEATURE_UNAVAILABLE (semantic khi disabled), EMBEDDING_FAILED, INVALID_INPUT

SIDE EFFECTS: none
PRE-CONDITIONS: none (works during any indexing phase)
POST-CONDITIONS: none
IDEMPOTENT: CÓ
```

**SearchInput:**
```
SearchInput :: {
  query   :: string
  kind    :: Ref<SearchKind> = "symbol"
  limit   :: int = 10                   // = DEFAULT_SEARCH_LIMIT
}
```

**SearchOutput:**
```
SearchOutput :: {
  results        :: List<SearchResult>
  truncated      :: bool
  degraded       :: bool                  // true khi kind=hybrid but embeddings not ready → FTS-only
  suggestions    :: List<string>?         // ABSENT khi None
  note           :: string?              // ABSENT khi None
  suggested_next :: Ref<SuggestedNext>?
}
```

---

### Tool 3: file_overview

> Symbols, structure, inferred role of a file.

```
INPUT  :: Ref<FileOverviewInput>
OUTPUT :: Ref<FileOverviewOutput>
       | Ref<UnifiedError>    // PARSE_FAILED
SIDE EFFECTS: none
PRE-CONDITIONS: none
POST-CONDITIONS: none
IDEMPOTENT: CÓ
```

**FileOverviewInput:**
```
FileOverviewInput :: {
  path :: string
}
```

---

### Tool 4: symbol_info

> Metadata + health signals for a symbol. Ambiguity Contract applies.

```
INPUT  :: Ref<SymbolInfoInput>
OUTPUT :: Ref<SymbolInfoOutput> | Ref<AmbiguousResult>
       | Ref<UnifiedError>    // NOT_FOUND
SIDE EFFECTS: none
PRE-CONDITIONS: none
POST-CONDITIONS: session.explored_symbols updated
IDEMPOTENT: CÓ
```

**SymbolInfoInput:**
```
SymbolInfoInput :: {
  name :: string
  path :: string?                       // Disambiguate — Ambiguity Contract
}
```

---

### Tool 5: source

> Read symbol-precise source code. Replaces native Read file tool.

```
INPUT  :: Ref<SourceInput>
OUTPUT :: Ref<SourceOutput> | Ref<AmbiguousResult>
       | Ref<UnifiedError>    // NOT_FOUND
SIDE EFFECTS: none
PRE-CONDITIONS: none
POST-CONDITIONS: session.already_fetched updated
IDEMPOTENT: CÓ
```

**SourceInput:**
```
SourceInput :: {
  target            :: string           // Symbol name OR "path:line_start-line_end"
  include_metadata  :: bool = false     // true → skip prior symbol_info call
  context_lines     :: int = 10         // = TEXT_CHUNK_CONTEXT_LINES
}
```

---

### Tool 6: callers

> Who calls a symbol. Blast radius scan. Ambiguity Contract applies.

```
INPUT  :: Ref<CallersInput>
OUTPUT :: Ref<CallersOutput> | Ref<AmbiguousResult>
       | Ref<UnifiedError>    // NOT_FOUND, INDEX_PARTIAL, TIMEOUT
SIDE EFFECTS: none
PRE-CONDITIONS: none (degraded results when edges_ready: false)
POST-CONDITIONS: none
IDEMPOTENT: CÓ
```

**CallersInput:**
```
CallersInput :: {
  symbol    :: string
  path      :: string?
  limit     :: int = 10
  max_depth :: int = 1                  // 1 = direct only, no transitive BFS
}
```

---

### Tool 7: callees

> What a symbol calls. Logic flow tracing. Ambiguity Contract applies.

```
INPUT  :: Ref<CalleesInput>
OUTPUT :: Ref<CalleesOutput> | Ref<AmbiguousResult>
       | Ref<UnifiedError>    // NOT_FOUND, INDEX_PARTIAL, TIMEOUT
SIDE EFFECTS: none
PRE-CONDITIONS: none
POST-CONDITIONS: none
IDEMPOTENT: CÓ
```

**CalleesInput:**
```
CalleesInput :: {
  symbol    :: string
  path      :: string?
  limit     :: int = 10
  max_depth :: int = 1
}
```

---

### Tool 8: dependencies

> File-level import graph. No Ambiguity Contract (input is path, not symbol).

```
INPUT  :: Ref<DependenciesInput>
OUTPUT :: Ref<DependenciesOutput>
       | Ref<UnifiedError>
SIDE EFFECTS: none
PRE-CONDITIONS: none
POST-CONDITIONS: none
IDEMPOTENT: CÓ
```

**DependenciesInput:**
```
DependenciesInput :: {
  path :: string
}
```

---

### Tool 9: path

> Bidirectional BFS — trace if and how symbol A reaches symbol B. Ambiguity Contract applies.

```
INPUT  :: Ref<PathInput>
OUTPUT :: Ref<PathOutput> | Ref<AmbiguousResult>
       | Ref<UnifiedError>    // NOT_FOUND, TIMEOUT, INVALID_INPUT
SIDE EFFECTS: none
PRE-CONDITIONS: none
POST-CONDITIONS: none
IDEMPOTENT: CÓ
```

**PathInput:**
```
PathInput :: {
  from_symbol :: string
  to_symbol   :: string
  from_path   :: string?
  to_path     :: string?
  max_hops    :: int?                   // absent → DEFAULT_MAX_HOPS (8); > MAX_ALLOWED_HOPS → clamped
  max_paths   :: int = 3
  timeout_ms  :: int?                   // absent → PATH_TIMEOUT_MS (5000)
}
```

---

### Tool 10: edit_context

> MANDATORY pre-edit. Blast radius + risk assessment. Ambiguity Contract applies.

```
INPUT  :: Ref<EditContextInput>
OUTPUT :: Ref<EditContextOutput> | Ref<AmbiguousResult>
       | Ref<UnifiedError>    // NOT_FOUND, INDEX_PARTIAL
SIDE EFFECTS: none
PRE-CONDITIONS: none
POST-CONDITIONS: none
IDEMPOTENT: CÓ
```

**EditContextInput:**
```
EditContextInput :: {
  symbol :: string
  path   :: string?
}
```

---

### Tool 11: session_context

> Session tracking state. Use after 10+ tool calls without convergence.

```
INPUT  :: {} (no parameters)
OUTPUT :: Ref<SessionContextOutput>
SIDE EFFECTS: Initializes session if first call
PRE-CONDITIONS: none
POST-CONDITIONS: none
IDEMPOTENT: CÓ
```

---

### Tool 12: diff_impact

> Post-edit blast radius verification. MANDATORY before commit/push.

```
INPUT  :: Ref<DiffImpactInput>
OUTPUT :: Ref<DiffImpactOutput>
       | Ref<UnifiedError>    // INVALID_INPUT, FEATURE_UNAVAILABLE

SIDE EFFECTS: none

PRE-CONDITIONS:
  - Exactly ONE of: diff, staged, commits must be provided

POST-CONDITIONS: none
IDEMPOTENT: CÓ
```

**DiffImpactInput:**
```
DiffImpactInput :: {
  diff    :: string?                    // Raw diff text — no git needed
  staged  :: bool?                      // true → git diff --cached -M
  commits :: string?                    // git diff -M <range>
}
```

**Constraints:**
```
INVARIANT: Exactly one of (diff, staged, commits) must be non-null
INVARIANT: diff + staged → INVALID_INPUT; staged + commits → INVALID_INPUT; all three → INVALID_INPUT; none → INVALID_INPUT
```

---

### Tool 13: indexing_status

> File-level index stats, embedding error details, trigger recovery.

```
INPUT  :: Ref<IndexingStatusInput>
OUTPUT :: Ref<IndexingStatusOutput>
       | Ref<UnifiedError>
SIDE EFFECTS: retry_embeddings=true → clear error state, trigger re-download
PRE-CONDITIONS: none
POST-CONDITIONS: embeddings_status may change to "downloading"
IDEMPOTENT: KHÔNG — retry_embeddings=true has side effect
```

**IndexingStatusInput:**
```
IndexingStatusInput :: {
  retry_embeddings :: bool = false
}
```

---

### Tool 14: locate

> Compound: search + file_overview + symbol_info in 1 call.

```
INPUT  :: Ref<LocateInput>
OUTPUT :: Ref<LocateOutput>
       | Ref<UnifiedError>
SIDE EFFECTS: none
PRE-CONDITIONS: none
POST-CONDITIONS: session.explored_symbols, explored_files updated
IDEMPOTENT: CÓ
```

**LocateInput:**
```
LocateInput :: {
  query :: string
  kind  :: Ref<SearchKind>?            // absent → "symbol"
  depth :: Ref<LocateDepth>?           // absent → "with_symbol"
  limit :: int?                        // absent → DEFAULT_SEARCH_LIMIT
}
```

**Constraints:**
```
INVARIANT: kind ∈ {"text", "file"} + depth = "with_symbol" → auto-downgrade to "with_file", set depth_adjusted = "with_file"
```

---

### Tool 15: hotspots

> Proactive churn x complexity analysis. Adam Thornhill's method.

```
INPUT  :: Ref<HotspotsInput>
OUTPUT :: Ref<HotspotsOutput>
       | Ref<UnifiedError>
SIDE EFFECTS: none
PRE-CONDITIONS: none
POST-CONDITIONS: none
IDEMPOTENT: CÓ
```

**HotspotsInput:**
```
HotspotsInput :: {
  top_n            :: int?               // absent → HOTSPOT_DEFAULT_TOP_N
  since            :: string?            // absent → HOTSPOT_DEFAULT_SINCE
  min_churn        :: int?               // absent → HOTSPOT_DEFAULT_MIN_CHURN
  include_symbols  :: bool = false
}
```

---

### Tool 16: understand

> Compound: locate + source + callers summary in 1 call.

```
INPUT  :: Ref<UnderstandInput>
OUTPUT :: Ref<UnderstandOutput>
       | Ref<UnifiedError>
SIDE EFFECTS: none
PRE-CONDITIONS: none
POST-CONDITIONS: session updated
IDEMPOTENT: CÓ
```

**UnderstandInput:**
```
UnderstandInput :: {
  query :: string
  kind  :: Ref<SearchKind>?            // absent → "symbol"
}
```

---

## Private Keys (internal, stripped before MCP response)

> Injected by tool handlers, consumed by compute_suggested_next, stripped by _make_tool_fn.
> NEVER exposed to MCP client.

```
_PRIVATE_KEYS :: frozenset = {
  "_kind"              // search: query kind passed
  "_target"            // source: target symbol/path fetched
  "_include_metadata"  // source: whether include_metadata=True
  "_max_hops"          // path: max_hops value used
  "_from_symbol"       // path: original from_symbol arg
  "_to_symbol"         // path: original to_symbol arg
}
```

---

## 5. ERROR REGISTRY

| Code | Retryable? | Severity | Message Template | Context cần thiết | Khi nào xảy ra |
|---|---|---|---|---|---|
| `NOT_FOUND` | KHÔNG | INFO | `"Symbol '{name}' not found in index"` | `name`, `path` | symbol_info, source, callers, callees, path, edit_context — input không match any symbol |
| `INDEX_PARTIAL` | CÓ (backoff) | WARN | `"Index not complete — edges_ready: false"` | `phase` | callers, callees, path, edit_context, diff_impact — graph tools khi phase != ready |
| `PARSE_FAILED` | KHÔNG | ERROR | `"File has syntax errors: {path}"` | `path`, `error` | source, file_overview — tree-sitter parse failure |
| `TIMEOUT` | CÓ (immediate) | WARN | `"BFS/query timed out after {ms}ms"` | `timeout_ms` | path, callers (transitive), callees (transitive) |
| `DB_LOCKED` | CÓ (backoff) | ERROR | `"SQLite write contention"` | — | Any tool — rare với WAL mode |
| `INVALID_INPUT` | KHÔNG | INFO | `"{validation_message}"` | Pydantic error details | diff_impact (bad input combo), search (bad query), path (bad params) |
| `FEATURE_UNAVAILABLE` | KHÔNG | INFO | `"Feature requires configuration: {feature}"` | `feature`, `suggestions` | search (semantic khi disabled), diff_impact (git unavailable) |
| `EMBEDDING_FAILED` | CÓ (backoff) | ERROR | `"Embedding pipeline failed: {reason}"` | `reason`, `retry_count` | search (semantic/hybrid khi embeddings failed) |

**Error format chuẩn:**
```
UnifiedError :: {
  error :: {
    code        :: Ref<ErrorCode>
    message     :: string
    recoverable :: bool
    suggestions :: List<string>?       // ABSENT khi None — không emit null
  }
}
```

---

## 6. EXTERNAL CONTRACTS

### tree-sitter-languages

**Package Version expected:** 1.10.2+
**Purpose:** 80+ pre-compiled grammars for AST parsing.
**Last verified:** 2026-06-29

```
REQUEST :: tree_sitter_languages.get_language(language_name: str) → Language
REQUEST :: tree_sitter_languages.get_parser(language_name: str) → Parser

FAILURES ::
  | ImportError        // Package not installed → Indexer cannot parse, all tools return empty
  | LANGUAGE_NOT_FOUND // Unknown language string → fallback to generic parser
```

### fastembed (opt-in)

**Package Version expected:** 0.8.0+
**Purpose:** ONNX embeddings without PyTorch. ~60-100MB.
**Last verified:** 2026-06-29

```
REQUEST :: fastembed.TextEmbedding(model_name: str) → model
REQUEST :: model.embed(texts: List[str]) → Iterator[ndarray]

FAILURES ::
  | ImportError        // Not installed → embeddings disabled, SearchKind.semantic unavailable
  | MemoryError        // OOM → EmbedStatus.FAILED, reason="oom"
  | NetworkError       // Download failed → EmbedStatus.FAILED, reason="download_failed"
```

### sqlite-vec (opt-in)

**Package Version expected:** 0.1.9+
**Purpose:** KNN vector search in SQLite.
**Last verified:** 2026-06-29

```
REQUEST :: sqlite_vec.load(conn: Connection)
  // MUST call BEFORE CREATE VIRTUAL TABLE embedding_vecs

FAILURES ::
  | ImportError        // Not installed → embeddings disabled
  | OperationalError   // "no such module: vec0" → sqlite_vec.load not called before CREATE TABLE
```

### watchfiles

**Package Version expected:** 1.1.1+
**Purpose:** File system watcher for incremental indexing.
**Last verified:** 2026-06-29

```
REQUEST :: watchfiles.awatch(path: str) → AsyncIterator[Set[Tuple[Change, str]]]

FAILURES ::
  | ImportError        // Not installed → no file watching, manual reindex only
```

### git CLI

**Version expected:** any
**Purpose:** diff_impact (staged/commits mode), hotspots (churn), codeowners (blame fallback).

```
REQUEST :: subprocess.run(["git", "diff", ...], cwd=project_root) → CompletedProcess
REQUEST :: subprocess.run(["git", "log", ...], cwd=project_root) → CompletedProcess

FAILURES ::
  | FileNotFoundError  // git not in PATH → diff_impact: FEATURE_UNAVAILABLE; hotspots: index_only; codeowners: empty
  | TimeoutExpired     // git timeout → hotspots: index_only fallback
  | Non-zero exit      // git error → diff_impact returns (None, error_msg)
```

---

## 7. NAMING CONVENTIONS

| Context | Convention | Ví dụ |
|---|---|---|
| Schema names | `PascalCase` | `RepoOverviewOutput`, `SearchInput` |
| Field names | `snake_case` | `qualified_name`, `is_hub`, `dead_code_confidence` |
| Constants | `SCREAMING_SNAKE` | `MAX_HOPS`, `BM25_EXACT_WEIGHT` |
| Functions | `snake_case` with prefix convention | `_handle_xxx`, `_xxx_logic`, `_xxx_sync` |
| Error codes | `SCREAMING_SNAKE` | `NOT_FOUND`, `DB_LOCKED` |
| File / module names | `snake_case`, language suffix with underscore | `python_.py`, `typescript_.py` |
| DB table names | `snake_case` | `call_edges`, `file_index` |
| DB index names | `idx_{table}_{column}` | `idx_symbols_path`, `idx_call_edges_to` |
| Pydantic models | `{ToolName}{Input\|Output}` | `SearchInput`, `CallersOutput` |

**Domain-specific rules:**

```
HANDLER PATTERN: Mỗi tool có 3 functions:
  _handle_xxx(params: dict, *, ctx: ServerContext) -> dict     — async MCP handler
  _xxx_sync(params: XxxInput, ctx: ServerContext) -> dict      — sync wrapper (asyncio.to_thread)
  _xxx_logic(params: XxxInput, conn, config) -> XxxOutput      — pure sync logic (dùng bởi compound tools in-process)
  ✅ _handle_search, _search_sync, _search_logic
  ❌ handle_search, search_handler, do_search
```

```
PARSER REGISTRATION: Language parser files suffixed with underscore to avoid shadowing stdlib.
  ✅ python_.py, typescript_.py, java_.py
  ❌ python.py, ts.py, javascript_parser.py
```

```
PRIVATE KEYS: Prefixed with underscore, used only by compute_suggested_next.
  ✅ _kind, _target, _include_metadata
  ❌ kind_hint, internal_kind, __kind
```

```
TOKENIZE FUNCTION: Single canonical implementation in db_init.py, imported by all consumers.
  ✅ from codeindex.db_init import tokenize_identifier
  ❌ Re-implementing tokenize logic in parser files
```

---

## 8. SCHEMA CHANGELOG

| Version | Date | Schema | Thay đổi | Breaking? | ADR Ref |
|---|---|---|---|---|---|
| v1.0 | 2026-06-29 | — | Init schema registry from architecture-design.md v2.7.2 | — | — |
| v2.6 | 2026-06-29 | `symbols` | ADDED: `coreness` INTEGER column | KHÔNG | — |
| v2.6 | 2026-06-29 | `call_edges` | ADDED: `idx_call_edges_to` index | KHÔNG | — |
| v2.7 | 2026-06-29 | `symbols` | ADDED: `name_tokens` TEXT column | KHÔNG | — |
| v2.7 | 2026-06-29 | `file_index` | ADDED: `mtime` REAL column | KHÔNG | — |
| v2.7 | 2026-06-29 | `fts_tokens` | CHANGED: from `symbol_id UNINDEXED, name_tokens` to content-backed `name_tokens` only | CÓ | — |

---

## 9. DEPRECATION REGISTRY

*(Không có deprecation nào hiện tại)*

---

## 10. GLOSSARY

| Term | Định nghĩa | Không nhầm với | Status |
|---|---|---|---|
| **Qualified Name** | Unique cross-file identifier cho symbol: `"module.ClassName.method"`. Dùng làm primary key trong symbols table và join key với call_edges. | `name` (bare name, không unique) | `STABLE` |
| **Name Tokens** | Pre-tokenized output của `tokenize_identifier(name)`. Ví dụ: `"getUserByEmail"` → `"get user by email"`. Stored trong `symbols.name_tokens`, indexed bởi `fts_tokens`. | `name` (original, untokenized) | `STABLE` |
| **Coreness** | K-core decomposition value. Measures mutual reinforcement: node A có coreness cao khi liên kết với nhiều nodes có coreness cao (recursive). `0` = isolated, `NULL` = chưa computed. | `caller_count` (in-degree only, misses bridge functions) | `STABLE` |
| **Hub** | Symbol có `is_hub = true`. Hai paths: degree-hub (top 5% caller_count, >= min_callers) OR bridge-hub (>= min_callers_bridge AND coreness >= p75). | "popular function" — hub captures cả bridge functions với moderate call count | `STABLE` |
| **Bridge Hub** | Hub detected qua coreness path: caller_count thấp-trung bình nhưng coreness cao — kết nối modules. `is_hub: true` + `caller_count < min_callers`. | Degree hub (high caller_count) | `STABLE` |
| **Edge Confidence** | 3-tier label cho call graph edges: resolved (same file/explicit import), inferred (import+type), textual (name-only). | Risk level (khác concept: confidence = certainty of edge existence) | `STABLE` |
| **Blast Radius** | Tổng symbols/files bị ảnh hưởng khi modify một symbol. Measured by callers (direct + transitive) và edge confidence. | Risk level (blast radius là input cho risk, không phải risk itself) | `STABLE` |
| **Compound Tool** | Tool gộp nhiều internal logic calls trong 1 MCP call (locate, understand). In-process orchestration, không có MCP round-trips giữa steps. | Pipeline (compound tool trả 1 response, pipeline trả nhiều) | `STABLE` |
| **Preset** | CLI flag lọc tools nào được register. orient/trace/edit/compound/full. Set một lần khi boot, không đổi giữa session. | Profile/config (preset chỉ filter tool registration, không thay đổi behavior) | `STABLE` |
| **Frontier** | Files chưa explore, connected với explored files via imports. Used by session_context. | "Next files to read" (frontier algorithm-specific: import-connected only) | `STABLE` |
| **RRF** | Reciprocal Rank Fusion. Merge technique: `RRF(d) = Σ 1/(k + rank(d, L))`. k=20 default. | BM25 (scoring within single list, not merge across lists) | `STABLE` |
| **WAL Mode** | SQLite Write-Ahead Logging. Enables concurrent readers + one writer without blocking. | Journal mode (WAL is a specific journal mode) | `STABLE` |
| **Content-backed FTS5** | FTS5 virtual table with `content='symbols'` — does NOT duplicate text, fetches via content_rowid. Requires sync triggers. | Standalone FTS5 (duplicates all text) | `STABLE` |
| **Phase Ladder** | 4-phase indexing progression: scanning → parsing → building_edges → ready. Embeddings track separately. | Embedding pipeline (parallel, not part of phase ladder) | `STABLE` |
| **Debounce** | 500ms cooldown on coreness recompute after file change. Prevents thrashing in active coding sessions. | Throttle (debounce resets timer on each event, throttle caps rate) | `STABLE` |
