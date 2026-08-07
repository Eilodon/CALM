# CALM ← Pecorino: Roadmap học hỏi có căn cứ

**Ngày:** 2026-08-07
**Trạng thái:** Research / proposal (chưa code)
**Phương pháp:** đọc source pecorino thật (`master`, AGPL-3.0) + verify từng claim trên source CALM thật. Không tin bản proposal gốc — dùng nó làm giả thuyết, rồi kiểm chứng.

---

## TL;DR — chốt một câu

Giá trị thực dụng nhất CALM lấy được từ pecorino **không** nằm ở hạ tầng (DuckDB/Kùzu/Tantivy/ProNE/ramdisk) hay ở lớp ranking ML (LTR/PPR/cross-encoder), mà ở **3 loại "fact" mà CALM hiện đang mù hoàn toàn**:

1. **Type relations** (INHERITS / IMPLEMENTS) — "cái gì kế thừa/implement cái này?"
2. **Explicit throws** (RAISES) — "hàm này ném lỗi gì?"
3. **Write effects** (mutations) — "hàm này đổi state nào?"

Cộng thêm **1 ý tưởng đóng gói** (Verified Index Bundles) có ROI vận hành cao và độc lập.

Phần còn lại của pecorino hoặc **CALM đã làm tốt hơn**, hoặc là **giả thuyết chưa được chứng minh ngay trong chính pecorino** (xem phần "đã kiểm chứng" bên dưới).

---

## 1. Những gì đã kiểm chứng trực tiếp trên source

### 1.1. Pecorino (AGPL-3.0 — ràng buộc pháp lý, không chỉ phong cách)

Pecorino là AGPL. **Không được port code** vào CALM (permissive OSS). Mọi thứ adopt phải clean-room: quan sát hành vi → tự thiết kế lại. Ràng buộc này là **bắt buộc**, và may là những thứ đáng lấy đều là *khái niệm/thuật toán phổ quát*, không phải code.

Stack pecorino (xác nhận từ repo): Python + **DuckDB** (metadata + vector) + **Tantivy** (BM25F) + **Kùzu/Gorgonzola** (property graph openCypher) + tree-sitter + ONNX cross-encoder. 9 tool: `browse`, `search`, `query_graph`, `update_index`, `set_workspace`, `metrics`, `detect_changes`, `manage_adr`, `manage_snapshot`.

**Edge taxonomy thật** (16 loại, `index_pipeline.py:1064`):
`DEPENDS_ON, CONTAINS, DEFINES, INHERITS, IMPLEMENTS, CALLS, FILE_CHANGES_WITH, RAISES, TESTS, HTTP_CALLS, IMPORTS, READS, WRITES, RETURNS, HAS_PARAMETER, USES`.

Các claim của proposal về pecorino — mình kiểm tra từng module, **đúng gần như 100%**:

| Claim proposal | Verify trên source pecorino | Kết luận |
|---|---|---|
| HCGS bẻ cycle heuristic, không SCC | `hcgs.py build_levels`: "pick node with fewest unprocessed callees" | ✅ ĐÚNG |
| `max_depth` không được dùng | `process_levels_static(max_depth=2)` — param nhận nhưng thân hàm không đọc | ✅ ĐÚNG |
| reads/writes có API nhưng chưa dùng hết | `build_static_summary` có param `reads/writes`, `process_levels_static` **không truyền** | ✅ ĐÚNG |
| Leiden gamma sweep không truyền gamma | `sweep_gamma`: query = `CALL leiden('{graph_name}')` — **gamma không hề nội suy vào call** → mọi γ ra cùng partition, ARI luôn = 1.0 | ✅ ĐÚNG — **bug thật trong pecorino** |
| PPR mọi edge ngang nhau | `compute_ppr_scores`: `prob=(1-alpha)*p[u]/len(neighbors)` — không có trọng số confidence | ✅ ĐÚNG |
| LTR chỉ "learn" khi có model, mặc định linear | `compute_ltr_score`: dùng XGBoost nếu có `.predict`, else weighted-sum 20 trọng số tay | ✅ ĐÚNG |
| domain/entity inference quá heuristic | `infer_domain`: match substring path (`/auth`→auth); `canonicalize_entity`: singularize + strip `info/data/repository/...` | ✅ ĐÚNG |
| ProNE nặng | `compute_prone_embeddings`: scipy SVD + Chebyshev + int8 | ✅ ĐÚNG (thật nhưng nặng) |

**Đính chính quan trọng vs proposal:** pecorino's `build_static_summary` **đã là non-LLM/factual assembly rồi** (header + docstring + params + "Calls:" + "Reads/Mutates state:" + complexity). Nên "CALM không hallucinate còn pecorino thì có" **KHÔNG phải điểm khác biệt**. Điểm khác biệt thật của Digest-CALM (xem §4.T2) là: (a) SCC condensation vs bẻ-cycle-heuristic, (b) lọc theo edge-confidence vs mọi-edge-ngang-nhau, (c) incremental + generation-fence vs recompute. Reframe này đổi cả cách thiết kế benchmark.

### 1.2. CALM (verify các điểm hook cho roadmap)

| Thứ cần cho roadmap | Vị trí thật | Ý nghĩa |
|---|---|---|
| Nơi extract symbol/call/import | `ParsedSymbol` [parser.rs:3](../../crates/calm-core/src/indexer/parser.rs#L3); `ExtractedFile` [pipeline.rs:313](../../crates/calm-core/src/indexer/pipeline.rs#L313) | Thêm `type_relations`/`effects` vào `ExtractedFile`, persist cùng source transaction |
| Anchor cho INHERITS/IMPLEMENTS | `ParsedSymbol.class_context` (đã track enclosing class/impl) | Parser **đã walk class decl** → extract superclass/interface là add nhỏ, không phải pass mới |
| Template schema + provenance | `external_proofs` [schema.rs:184](../../crates/calm-core/src/db/schema.rs#L184) | `source_file_hash`/`provider`/`graph_generation`/`status CHECK`/`UNIQUE` — copy y khuôn cho bảng fact mới |
| Confidence taxonomy | `EdgeConfidence{Formal,Resolved,Inferred,Textual,Ambiguous,Unresolved}` [types.rs:32](../../crates/calm-core/src/types.rs#L32) | Dùng lại cho `to_symbol`/`target_text` split |
| Generation fence | bảng `graph_generation_state` [schema.rs:224](../../crates/calm-core/src/db/schema.rs#L224) | Digest reuse pattern này |
| Guarantee taxonomy + doc-drift | [docs/guarantee-levels.toml](../guarantee-levels.toml) → `status.generated.md` | Mỗi fact mới khai báo 1 guarantee level ở đây |
| CE backend (nếu tới Tier 4) | `tract-onnx` optional, feature `onnx-embeddings` [Cargo.toml:53](../../crates/calm-core/Cargo.toml#L53) | Pure-Rust ONNX sẵn — nhưng CE workload là đất mới, chưa battle-test |
| Harness validate | `tests/lang_feature_parity.rs`, `martin_cross_language.rs`, `golden_graph_equivalence.rs`, `tests/fixtures/` | **Golden fixtures = kênh validate cho Tier 1, KHÔNG cần retrieval corpus** |
| Integration point | `SymbolInfoOutput` [detail.rs:279](../../crates/calm-server/src/tools/detail.rs#L279); `understand`/`symbol_info` ở inspect.rs | Thêm field ở đây |
| Boundary cứng | `indexer/ → analysis/` bị cấm ([churn.rs:9](../../crates/calm-core/src/graph/churn.rs#L9)) | Syntax-fact ở `indexer/`, graph-fact ở `graph/` |

---

## 2. Decision matrix — adopt / adapt / reject (đã có căn cứ)

Nguyên tắc lọc: **chỉ lấy fact mà ta phát biểu được ngữ nghĩa chính xác, và có thể verify bằng compiler/fixture — hoặc ý tưởng vận hành độc lập.** Loại mọi thứ chỉ "nghe hay" mà ngay trong pecorino cũng chưa chứng minh được value.

| Pecorino | Quyết định | Lý do (đã verify) |
|---|---|---|
| INHERITS / IMPLEMENTS | 🟢 **Adopt (Tier 1)** | CALM mù hoàn toàn; `class_context` đã có anchor; verify bằng fixture |
| RAISES (explicit throw) | 🟢 **Adopt (Tier 1)** | Cú pháp tường minh, FP ~0; rẻ nhất → làm trước writes |
| WRITES (mutation) | 🟢 **Adopt (Tier 1, thận trọng)** | Density cao nhất, nhưng cần tách "syntactic write" vs "shared-state mutation" |
| HCGS → Architecture Digest | 🟢 **Adopt concept, viết lại (Tier 2)** | Value = compression + callee rollup; wins thật = SCC + confidence + incremental |
| Verified Index Bundles | 🟢 **Adopt (Tier 3)** | ROI vận hành cao, độc lập, không cần gate; CALM đã có index/state split |
| Verb canonicalization | 🟡 **Optional/Tier 4 (hạ bậc)** | Embedding của CALM đã bắt synonymy ngầm; chỉ verb-token expansion có chút giá trị, chưa chứng minh |
| Federation (read-only) | 🟡 **Tier 4, tách 3 nhánh** | Chiến lược, nhưng scope lớn; package-graph trước |
| PPR / weighted-BFS / CE / digest-search | 🟡 **Shadow lab (Tier 4)** | Pecorino PPR unweighted + LTR không ship model → **không có win đã chứng minh để copy**, chỉ có giả thuyết để test |
| READS | 🔴 Defer | Density thấp; làm sau khi WRITES chứng minh value |
| DATA_FLOWS_TO / taint | 🔴 Reject (ở scope này) | Là initiative static-analysis riêng, không phải "thêm 1 edge" |
| HTTP routes / env | 🔴 Reject core (provider sau) | Framework-specific; chỉ làm khi có demand + fixture thật |
| Leiden (như đang ship) | 🔴 Reject | Bug gamma → chính pecorino không vary resolution; community detection = offline research |
| ProNE | 🔴 Reject | Nặng (scipy/SVD/Chebyshev), CALM không có numpy; value không chứng minh |
| LTR (handcrafted 20 weights) | 🔴 Reject | Cần labels chưa có; churn/complexity ≠ relevance; pecorino cũng fallback linear |
| detect_changes | 🔴 Reject | `diff_impact` của CALM đã mạnh hơn (rename-aware, quoted-path, signature) |
| DuckDB / Kùzu / Tantivy / ramdisk / raw Cypher MCP / ADR CRUD | 🔴 Reject | Consistency/build tax; single-SQLite của CALM là **lợi thế**, đừng phá |

---

## 3. Vì sao đây là roadmap "value thực dụng nhất"

Ba fact Tier 1 trả lời **câu hỏi kỹ thuật hằng ngày** mà CALM hôm nay *không thể*:

- Sửa một interface → "ai implement nó?" (hiện phải grep tay, mất tính formal)
- Sửa error handling → "hàm này ném gì, ai catch?"
- Sửa một hàm → "nó đụng state nào ngoài local?"

Chúng **rẻ** (bám traversal có sẵn), **verify được** (fixture, không cần corpus đắt), **blast radius thấp** (advisory-only, không đụng write gate), và **bật default sớm được**. Đó là định nghĩa của ROI cao / rủi ro thấp.

Ngược lại, mọi thứ ranking-ML của pecorino là *giả thuyết chưa validate ngay trong pecorino* — copy chúng là nhập khẩu rủi ro chứ không phải nhập khẩu value.

---

## 4. Roadmap cụ thể (tiered + sequenced)

### Phase 0-lite (vài tuần, KHÔNG phải vài tháng)
Chỉ dựng đủ để mở khoá Tier 1 — **không** yêu cầu external retrieval corpus ở đây:
- `analysis_version` per-analyzer (không bump toàn DB schema khi 1 heuristic đổi).
- Feature lifecycle `disabled → shadow → advisory → active` (mirror guarantee-levels).
- Golden-fixture harness mở rộng cho type/effect (dùng lại `lang_feature_parity.rs` + `martin_cross_language.rs`).
- `SemanticFactExtractor` trait trong `indexer/` (typed output, **không** EAV, **không** đụng `analysis/`).

---

### TIER 1 — Ba fact factual (làm trước, gate bằng fixture)

Thứ tự trong Tier 1 theo **rủi ro FP tăng dần**: Types → Throws → Writes.

#### T1.1 — Type Relations (Package B) ⭐ ROI cao nhất/effort
```
Bảng: type_relations
  from_symbol      TEXT NOT NULL     -- qualified name của lớp/loại nguồn
  target_text      TEXT NOT NULL     -- text cú pháp của base/interface
  to_symbol        TEXT NULL         -- resolve được thì gắn, không thì NULL
  relation_kind    TEXT NOT NULL     -- 'extends' | 'implements'
  confidence       TEXT NOT NULL     -- 'resolved' | 'textual' (dùng EdgeConfidence)
  source_path      TEXT NOT NULL
  line             INTEGER
  source_hash      TEXT NOT NULL     -- như external_proofs
  evidence_source  TEXT NOT NULL     -- 'ast' (v1)
  analysis_version INTEGER NOT NULL
```
- **Hook:** node lớp/impl mà `class_context` đã đi qua trong `parser.rs`; thêm đọc mệnh đề `extends`/`implements`/`impl Trait for Type`. Persist qua `ExtractedFile` → cùng transaction xoá/ghi khi file đổi (§43 lifecycle).
- **Ngôn ngữ v1:** Java, TypeScript, Python (bases), Rust (`impl Trait for Type`), JavaScript (`extends`). **Go: chỉ struct embedding** — KHÔNG infer `implements` (structural typing → tạo fact mạnh hơn evidence). Nói thẳng Go v1 coverage mỏng.
- **`target_text` + `to_symbol` split:** `class Foo(ExternalLibBase)` mà không resolve được → `target_text="ExternalLibBase", to_symbol=NULL, confidence=textual`. Không đoán bừa.
- **Integration:** thêm `type_relations{extends,implements}` vào `SymbolInfoOutput` + `understand`. **Không** đụng write gate. `reference_impact` chỉ dùng ở tầng `likely_change/review`, không `must_change`.
- **Graduation:** zero known FP trên golden fixtures 5 lang; không tool mới.
- **Effort:** THẤP. **Đây là "first PR" nên làm.**

#### T1.2 — Explicit Throws (Package C phần 1)
```
Bảng: symbol_effects  (dùng chung cho throws + writes)
  symbol_qn        TEXT NOT NULL
  effect_kind      TEXT NOT NULL     -- v1: 'explicit_throw'
  target_text      TEXT NOT NULL     -- tên exception, vd 'InvalidToken'
  line             INTEGER
  confidence       TEXT NOT NULL     -- 'syntax_exact'
  source_path/source_hash/evidence_source/analysis_version ...
```
- **Hook:** node `raise X(...)` / `throw new X(...)` — single AST node kind, FP gần 0. **Không** suy transitive, **không** checked-exception flow. Chỉ fact trực tiếp.
- **Integration:** `understand` hiển thị `Throws: InvalidToken, ExpiredToken`.
- **Vì sao trước Writes:** rẻ hơn và không có mơ hồ init-vs-mutation.

#### T1.3 — Write Effects (Package C phần 2)
```
effect_kind ∈ {'write_field', 'write_global', 'mutate_receiver'}
```
- **Hook:** `self.x = …` / `this.x = …` / `self.x = …` (Rust `&mut self`) / `r.x = …` (Go) / `global x` (Py). Map (`m[k]=v` Go) có ngữ nghĩa riêng.
- **Bắt buộc — hai tầng ngữ nghĩa (điểm mình bổ sung so với proposal):**
  - `write_field` = **syntactic write fact** (rẻ, exact — ship). 
  - **KHÔNG** để `understand` render nó thành "mutates shared state" — đó là claim mạnh hơn evidence (Python `self.x=…` trong `__init__` là init; Rust builder `fn with_x(mut self)->Self` khác `&mut self`). Phân biệt init-vs-mutation cần receiver/lifetime → **không** làm ở v1; giữ nhãn trung thực.
  - `self.cache.put(x)` → **không** tuyên bố mutation ở v1 (cần biết method signature của receiver).
- **Graduation:** nếu 1 language provider tạo quá nhiều FP → **disable riêng provider đó**, không hạ confidence giả để giữ coverage.

**Sau Tier 1:** bật default `resolved type relations` + `explicit throws` + `high-confidence write_field` trong `understand`/`symbol_info` (deterministic, local, không đổi write policy). Cập nhật `guarantee-levels.toml` + `indexing_status.intelligence.{types,effects}`.

---

### TIER 2 — Architecture Digest (Package D) — sau khi Tier 1 chín

**Reframe value (từ đính chính §1.1):** đừng bán Digest bằng "no hallucination". Bán bằng 3 win thật vs pecorino:
1. **SCC condensation** (Tarjan, O(V+E) — với 5376 symbol là *rẻ như không*) thay cho bẻ-cycle heuristic. → Recompute condensation **toàn bộ mỗi generation** (rẻ); chỉ **incremental** phần đắt là render+embed. *(Giải quyết mâu thuẫn SCC-global vs invalidation-local: SCC không cần incremental, chỉ digest-hash cần.)*
2. **Confidence-filtered:** core digest chỉ `formal`+`resolved`; `Possible calls: foo [inferred]` show riêng, không trộn.
3. **Incremental + generation fence:** `input_digest = hash(source_hash ∪ confirmed callee ids ∪ callee digest hashes ∪ effects ∪ type_relations ∪ analysis_version)`; commit chỉ khi `expected_generation == current`.

```
Bảng: symbol_digests
  symbol_qn PRIMARY KEY, facts_json TEXT, rendered_text TEXT,
  source_hash, input_digest, graph_generation, analysis_version, truncated
```
- `facts_json` = `{direct_callees, callee_role_tags, type_relations, effects, complexity, recursive_component}`. `rendered_text` chỉ là deterministic rendering.
- **Propagation tiết chế:** chỉ `callee name + role_tags ngắn` (role_tags = name-tokens của callee, thừa nhận thẳng đây là lexical, không phải hiểu ngữ nghĩa). Không propagate raw child digest → chặn vocabulary bleed.
- **Failure mode cứng:** digest chết → search/callers/edit vẫn chạy; `understand` trả `architecture_digest:null, digest_status:"stale"`. Không fabricate.
- **Chỉ tích hợp `understand`** — KHÔNG tool thứ 38.

**Benchmark ĐÚNG (sửa lỗi baseline của proposal):** Digest gói callees+effects+types mà `understand` hiện *chưa* show → nếu chỉ so `source+callers` vs `+digest`, benchmark sẽ credit nhầm "thông tin mới" thay vì "summarization". Phải có **3 nhánh**:
```
A: source + callers                                  (baseline hiện tại)
B: source + callers + callees + effects + types THÔ  (kiểm soát)
C: source + callers + digest (đã pre-roll)           (feature)
```
Chỉ **delta(C − B)** mới là value thật của Digest-as-summarization. Nếu C ≈ B → chỉ cần show callees+effects (rẻ hơn nhiều), không cần Digest.

**Đây là chỗ external retrieval corpus mới trở thành prerequisite** — không phải trước Tier 1.

---

### TIER 3 — Verified Index Bundles (Package H) — độc lập, promote lên

Mình **nâng bậc** cái này so với proposal (họ để #10) vì: concrete, demand rõ (onboard monorepo, CI build 1 lần, seed federation), **không cần benchmark gate** (verify-thì-activate).
```
calm-index.tar.zst = manifest.json + index.db   (KHÔNG bao giờ state.db/memory.key/audit.key)
manifest: calm_version, schema_version, analysis_versions, git tree/commit,
          enabled langs/features, embedding model fingerprint, index_db_sha256, created_at
import: extract temp → verify manifest/checksum/schema≤binary/repo identity/SQLite integrity_check
        → atomic replace; nếu tree không khớp → import as seed + incremental reindex changed files
```
CALM's index/state split khiến "bundle chỉ index.db" sạch tự nhiên. Đây là operational optimization, không P0, nhưng ROI/effort tốt hơn cả search-lab.

---

### TIER 4 — Research funnel (shadow, có thể không bao giờ ship)

Chỉ mở **sau** Tier 1-2, và chỉ giữ nếu thắng baseline ngu:
- **Federation** — tách 3: (a) package-dependency-graph (evidence-based, làm trước) → (b) federated-search fanout với per-repo RRF (repo_id = hash root-commit `git rev-list --max-parents=0 HEAD`, fallback remote URL) → (c) cross-repo call graph (**đừng**, tới khi có evidence). Read-only tuyệt đối; muốn edit thì chuyển sang CALM instance của repo đó.
- **Shadow retrieval lab** — search observability trước; rồi digest-leg (RRF thứ 4), weighted-BFS, PPR-challenger (dùng edge-confidence làm trọng số, khác pecorino), CE-challenger (qua `tract`). **Exact-name path là bất khả xâm phạm** — `EXACT_NAME_BONUS` không bao giờ bị AI-ranking phá. Kill: nếu weighted-BFS nằm trong ~1-2% PPR → ship BFS.
- **Lexical verb canonicalization** — hạ bậc: chỉ token-expansion khi index (leg phụ), không query-rewrite; drop hoàn toàn domain/entity heuristics (đã verify là rác).

### TIER 5 — Reject (xem matrix §2)
ProNE, Leiden-as-shipped, LTR, DuckDB/Kùzu/Tantivy/ramdisk/Cypher/ADR-CRUD, detect_changes, taint, READS, framework routes/env.

---

## 5. Mình lệch gì so với proposal gốc (và vì sao)

| Điểm | Proposal gốc | Roadmap này | Lý do (verified) |
|---|---|---|---|
| Thứ tự trong effects | Writes trước | **Throws trước** | Throws FP~0 (cú pháp tường minh); writes có mơ hồ init/alias |
| Index Bundles | #10 | **Tier 3 (promote)** | Concrete, ungated, demand rõ; independent |
| Lexical semantics | #4 | **Tier 4/optional (demote)** | Embedding CALM đã bắt synonymy; domain/entity heuristics = rác |
| Digest value pitch | "no LLM/no hallucination" | **SCC + confidence + incremental** | pecorino cũng đã factual → "no-hallucination" không phải differentiator |
| Digest benchmark | `source+callers` vs `+digest` | thêm nhánh B (callees+effects thô) | Tránh credit nhầm info mới thành summarization |
| SCC vs incremental | như 2 thứ độc lập | recompute SCC full/generation, chỉ incremental render+embed | Tarjan rẻ; incremental-SCC là bài toán vô ích |
| Phase 0 | gate cho mọi thứ | **Phase 0-lite; corpus chỉ chặn Tier 2** | Types/throws/writes gate bằng fixture, không cần corpus |

---

## 6. Sequencing + reality check

```
Phase 0-lite (analysis_version + lifecycle + fixture harness + SemanticFactExtractor)
   └─► T1.1 Type Relations ──► T1.2 Throws ──► T1.3 Writes     [gate: golden fixtures]
            │  (bật default trong understand/symbol_info)
            ▼
        [dựng external retrieval corpus — GIỜ mới cần]
            ▼
   T2 Architecture Digest (SCC + confidence + incremental)      [gate: benchmark 3-nhánh]
            ▼
   T3 Verified Index Bundles (song song được, không phụ thuộc)  [gate: verify-then-activate]
            ▼
   T4 Federation (package-graph → search fanout) + Shadow lab   [gate: thắng baseline ngu]
```

**Reality check thẳng thắn:** đây là chương trình **nhiều tháng** cho team nhỏ. Nhưng ~80% value hằng ngày nằm ở **Tier 1**, mà Tier 1 gate bằng fixture (rẻ) và ship *tăng dần* được — không phải chờ cỗ máy benchmark. Nếu budget hữu hạn: **làm hết Tier 1, dừng lại đánh giá thực địa, rồi mới quyết Tier 2+.** Đừng khởi động Tier 4 trước khi có usage data — pecorino đã chứng minh ranking-ML không có win sẵn để copy.

## 7. First PR (wedge cụ thể)

> **Type Relations cho Java + TypeScript + Python + Rust**, extract từ node `class_context` đã đi qua, persist qua `ExtractedFile` trong source transaction, surface `extends`/`implements` trong `symbol_info` + `understand`, gate bằng golden fixtures, `analysis_version=1`, **không** đụng ranking/write-gate. Go = struct embedding only, nói rõ.

Nhỏ, verify được, giá trị ngay, và dựng sẵn `SemanticFactExtractor` để Throws/Writes theo sau gần như miễn phí.
