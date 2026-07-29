# CALM — Nghiên cứu nâng cấp cực hạn độ chính xác & hiệu quả (2026-07-29)

> Mục tiêu: tìm giải pháp tối ưu / nâng cấp để tối đa hoá **độ chính xác** và **hiệu quả**
> của CALM khi giúp coding agent *khám phá – nắm bắt – tìm kiếm – đọc hiểu* một codebase.
> Phương pháp: (1) audit nội bộ trực tiếp trên code + docs qua chính CALM's tools; (2) đối
> chiếu SOTA — paper học thuật + tool hàng đầu cùng category. Mọi claim nội bộ đều grounded
> theo `file:line`; mọi claim ngoài đều có nguồn ở cuối.

> **⚠️ ĐÍNH CHÍNH (Round 2, 2026-07-29 — self-audit qua super-skills + query thẳng `.calm/index.db`):**
> Đề xuất **A (arity-gate)** ở bản gốc **SAI TIỀN ĐỀ** — arity gate **đã tồn tại** (`pipeline.rs:949-983`),
> `symbols.arity` + `call_sites.arg_count` đã persist; nó chỉ đang **Elixir-only có chủ đích**. Việc thật
> là *tổng quát hoá*, không phải *xây mới*. Con số ambiguous 54-92% chỉ đúng cho **case không có SCIP
> overlay** (repo này, Tier-0 Rust có overlay: chỉ 12.9%). Xem **§6 — Round 2** ở cuối để biết đầy đủ
> cái đúng/sai. Đọc §6 TRƯỚC khi hành động theo bảng dưới.

---

## 0. TL;DR — đòn bẩy lớn nhất, xếp theo leverage/effort

| # | Đòn bẩy | Vấn đề nó giải | Effort | Rủi ro | Loại |
|---|---|---|---|---|---|
| **A** | **Arity-gate cho candidate selection** (ngôn-ngữ-độc-lập) | `ambiguous` chiếm 54–92% edge ở mọi ngôn ngữ chưa có SCIP — trần chính của call-graph accuracy | Thấp–TB | Thấp | Accuracy |
| **B** | **Mở rộng stack-graphs formal tier** sang nhiều ngôn ngữ hơn (không cần toolchain SCIP của user) | Cùng vấn đề với A, nhưng là lời giải "compiler-grade" | Cao | TB | Accuracy |
| **C** | **Reranker stage sau RRF** (hoặc fold `coreness` vào hybrid RRF) | Hybrid search thiếu tầng rerank; `coreness` boost mới chỉ áp cho symbol-search, chưa áp cho hybrid | Thấp–TB | Thấp | Efficiency/Recall |
| **D** | **Optional high-accuracy embedding backend** (giữ potion làm mặc định) | `potion-code-16M` (static/model2vec) nhanh nhưng recall thấp hơn transformer code-embedder SOTA | TB | Thấp | Recall |
| **E** | **Đóng nốt các finding self-audit còn mở** (F3/F4/F5) | Bất nhất nội bộ trong `edit_context`/`path`/`repo_overview` | Thấp | Thấp | Accuracy/UX |
| **F** | **B7/B8 benchmark** (task-correctness + model-tier leveling) | Chưa có con số chứng minh CALM tăng *tỷ lệ hoàn thành task*, đúng loại bằng chứng thị trường định giá cao nhất | Cao | Thấp | Chiến lược |

**Kết luận 1 câu:** trần chính của CALM *không* nằm ở search hay UX (cả hai đã rất tốt) mà ở
**độ chính xác của call-graph cho ~19 ngôn ngữ chưa có SCIP provider** — và tồn tại một đòn bẩy
rẻ, ngôn-ngữ-độc-lập, được paper 2026 xác nhận, mà CALM *chưa* dùng: **arity matching** (mục A).

---

## 1. CALM hiện tại — kiến trúc & quyết định thiết kế (grounded)

Nguồn: `docs/architecture.md`, `AGENTS.md`, đọc code trực tiếp.

### 1.1 Indexing đa tầng
- **6 Tier-0** (Py/TS/JS/Java/Rust/Go): tree-sitter AST đầy đủ + call graph + import graph + multi-tier resolution, always-on.
- **18 Tier-0.5** (C/C++/C#/Ruby/PHP/Shell/R on-by-default; Kotlin/Swift/Scala/Dart/Lua/Elixir/Haskell/OCaml/Zig/PowerShell/Groovy opt-in feature): tree-sitter thật, call+import khi feature bật; fallback regex/line-scan khi không.
- **SQL** có indexer riêng (`sqlparser`), không call graph (đúng bản chất SQL).
- **Incremental watcher** FNV-1a content-hash diff, rebuild call graph incrementally, song song hoá `rayon`.

### 1.2 Call graph có confidence label — điểm mạnh nhất về mặt "trust"
- Mỗi edge mang nhãn `resolved`/`inferred`/`formal`/`textual` + fallback `ambiguous`/`unresolved`.
- **SCIP overlay** — 9 provider / 12 ngôn ngữ, data-driven `ScipProvider` (`crates/calm-core/src/scip/provider.rs`), auto-detect binary, hard-timeout, cache theo fingerprint.
- **LSP overlay** — `gopls`/`clangd`/`rust-analyzer` live-session, `on_demand` (ADR-0004).
- **Formal tier qua tree-sitter-stack-graphs** (ADR-0002) — name resolution "declarative DSL", không cần build process (đúng cách GitHub Precise Code Nav làm).
- Graph metrics: **`coreness` (k-core)** + `is_hub` để flag symbol trung tâm.

### 1.3 Search fusion
- **3-way RRF**: FTS5/BM25 + symbol-identity vector + code-chunk vector (`rrf_merge_n`, `crates/calm-core/src/search.rs`).
- Embedding: **`minishlab/potion-code-16M`** (model2vec, **static/pure-Rust**, không ONNX) vendored vào binary qua `build.rs` + SHA256 verify. KNN brute-force cosine, in-RAM cache — musl-safe (thay cho `sqlite-vec` từng chết trên musl/Docker).
- `search(kind="grep")` — regex+glob thật off-disk, phủ cả file parser bỏ qua.
- **Noise-penalty ranking** + (mới, self-audit C5) **exact-name seed + `CORENESS_WEIGHT` + `EXACT_NAME_BONUS`** trong `search_symbol` (`search.rs:262–331`).

### 1.4 Edit safety net (khác biệt hoá lớn nhất so với đối thủ)
- 1 write path `edit_lines`/`edit_symbol`, FNV-1a hash conflict guard, tree-sitter syntax-validate trước khi ghi, atomic write + reindex ngay.
- **3-layer gate** cho hub/high-fan-in: `edit_context` phải chạy đúng symbol này *trong session này* + `confirm:true` + `reason` cite tên caller thật. Hook-enforced (`.claude/hooks/calm-nudge.sh`).

### 1.5 Cơ sở hạ tầng: daemon/forwarder (ADR-0005), cross-process `flock`, single-instance indexing lock + promotion, SIGTERM watchdog, concurrent-agent awareness (`session_context.other_active_sessions`).

### 1.6 Self-grading + memory: `fitness_report` 9-metric, coverage-aware dead-code, architecture boundaries, doc-drift; `remember`/`recall`; `pattern_debt_register`; git co-change mining; `possibly_stuck`.

**Sức khoẻ hiện tại** (`fitness_report`, HEAD): PASS toàn bộ — hub 7.7%, dead-code 4.9%, edge-coverage 75.8%, high-complexity 2.7%, boundary/config-drift 0. Codebase rất lành mạnh; đây *không* phải nơi cần tối ưu.

---

## 2. Vấn đề chính đo được — `ambiguous` là trần accuracy thật

Nguồn: `benchmarks/resolution/README.md` (8+ repo OSS thật, không oracle → đo phân bố tier).

| lang | ambiguous% | ghi chú |
|---|---:|---|
| cpp (fmt) | **92.5%** | → **56.6%** khi wire `scip-clang` thật (formal 41.2%) |
| kotlin (kotlinpoet) | **89.6%** | fluent API trùng tên |
| elixir | 87.0% | |
| ocaml (lwt) | 86.3% | monadic `bind`/`map`/`return` |
| dart (args) | 80.9% | sau khi thêm C3 call-edge extraction |
| swift | 74.9% | |
| js (express) | 66.7% | |
| go (gin) | 54.3% | |

**Cơ chế:** khi callee bare-name fan-out ra nhiều same-named symbol, `MAX_CALLEE_CANDIDATES=20`
(`pipeline.rs:20`) khiến resolver **từ chối đoán bừa** → gắn `ambiguous` (hoặc drop nếu >20).
Đây là quyết định *đúng* về precision, nhưng để lại call-graph rất nhiễu cho agent: `callers`/
`callees`/`path`/`edit_context.blast_radius` bị độn bởi name-collision noise.

**Cascade narrowing hiện có** (`resolve_sites_to_edges`, `pipeline.rs:1060–1071`), theo thứ tự:
1. `module_hint` (file-stem qualifier)
2. `same_file`
3. `same_dir` — chỉ Go/Java/C/C++
4. `same_namespace` — chỉ C# (từ `using` + `NamespaceMap` thật; narrow về 1 ⇒ upgrade `resolved`)
5. Ngoài ra: `looks_option_or_result_chained` (Rust — lọc theo return-type shape)
6. else → toàn bộ candidate (≤20) gắn `ambiguous`.

> **Khoảng trống then chốt: không có bước lọc theo *arity* (số tham số) ở bất kỳ đâu trong cascade.**

---

## 3. SOTA đối chiếu (paper + tool)

### 3.1 Call-graph construction cho ngôn ngữ động — literature xác nhận hướng đi của A
- **InferCG (2026)** [2] — SOTA Python CG: **"high-recall candidate set bằng permissive name- *and arity*-based matching"** rồi filter bằng LLM. +13.9% recall / +5.0% F1 so với PyCG. → **Chính xác là bước arity mà CALM thiếu**; CALM có thể lấy tầng-1 (name+arity) mà *không* cần tầng LLM.
- **PyCG (2021)** [7] — precision ~99.2%, recall ~69.9% chỉ bằng static assignment-graph. Cho thấy trần recall của pure-static là thật (~0.70–0.88) — CALM chọn precision-first (gắn `ambiguous` thay vì đoán) là đúng, nhưng có thể đẩy recall lên bằng narrowing rẻ.
- **Recall of static CG in practice (ICSE 2020)** [1] — median recall 0.884; **"thêm precision hầu như không đổi recall — chúng là 2 mối lo tách biệt."** → arity-gate cải thiện *precision của tập ambiguous* mà không hy sinh recall.
- **Approximate interpretation (OOPSLA 2024)** [3] — JS dynamic property access: +55% call edge, recall 75.9%→88.1%, precision giảm 1.5%. Hướng nâng cao cho JS/TS về sau.
- **Unimocg (ISSTA 2024)** [9] — tách "tính type" khỏi "resolve call"; kiến trúc tiered của CALM đã đồng hướng.
- **Static CG combination (2022)** [4] — hợp nhiều static analyzer ≈ dynamic CG, không phantom call. CALM đã multi-tier (tree-sitter + SCIP + LSP); việc *hợp* được literature ủng hộ.

### 3.2 Name resolution không cần build — stack graphs là SOTA
- **Stack Graphs (Creager et al., GitHub)** [b] — mở rộng scope graphs, resolve name binding trong & xuyên repo bằng path-finding, **không cần config/build của repo owner**. Đây chính là formal tier ADR-0002 của CALM. → **Đòn bẩy B**: mỗi ngôn ngữ có TSG rules là một tầng formal *không phụ thuộc toolchain user* — bù đúng chỗ SCIP đòi binary+build mà 9/19 ngôn ngữ Tier-0.5 chưa có provider nào.

### 3.3 Retrieval / reranking cho coding agent
- **CORE-Bench (2025)** [c] & **CodeRAG-Bench (2025)** [d] & **CoIR (ACL 2025)** [e]: benchmark chuẩn cho *agentic* code retrieval. Phát hiện lặp lại: **"embedding retriever tụt mạnh trên code retrieval trong agentic setting"**, và retriever thường fetch context kém hữu ích. → CALM nên (a) tự đo trên các benchmark này, (b) coi retrieval là một stage của agent-loop chứ không phải 1 lookup.
- **Hybrid best practice 2026** [f]: điểm đến production là **BM25 + dense + RRF → cross-encoder reranker → LLM**. RRF là chuẩn fusion (không cần calibrate score); reranker là tầng tinh chỉnh, đẩy recall@10 78%→91%. → CALM có BM25 + 2 dense + RRF nhưng **thiếu tầng rerank** (đòn bẩy C).

### 3.4 Code embedding SOTA
- SOTA 2026: **Qwen3-Embedding** (0.6B/4B/8B, #1 MTEB multilingual), **Voyage-code-3**, **Jina-code**, **CodeXEmbed** [g], **Granite Embedding R2** [h]. Kết luận chung: **general-purpose model tụt mạnh trên code; cần code-specialized embedder.**
- CALM dùng **potion-code-16M** (static, model2vec) — ưu tiên local-first / musl / zero-network / in-binary. Static embedding *nhanh & nhỏ* nhưng recall thấp hơn transformer code-embedder. → Không đổi mặc định (triết lý local-first là đúng), nhưng mở **backend embedding tuỳ chọn** cho user cần recall cao (đòn bẩy D).

---

## 4. Khuyến nghị chi tiết

### A. Arity-gate cho candidate selection — **ưu tiên #1**
**Vấn đề:** cascade `resolve_sites_to_edges` không lọc theo số tham số; một call `format(a, b)` (2 arg)
vẫn fan-out ra mọi `format` 0-arg/5-arg cùng tên → `ambiguous` noise.

**Giải pháp (grounded):** thêm một bước trước nhánh fallback `else if t.len() <= MAX_CALLEE_CANDIDATES`
(`pipeline.rs:1067`): lọc candidate theo **arity tương thích**, dùng:
- **Call-site arity** — đếm argument ở call node (parse-tree thuần; `RawCall` đã có sẵn hạ tầng
  capture, vd `looks_option_or_result_chained` — thêm 1 field `arg_count`).
- **Candidate arity** — parse param-count từ signature đã lưu (`sig_by_qn`, `build_resolution_context`
  đã build sẵn map này ở `pipeline.rs:758`).

**Quy tắc soft (quan trọng để không phá recall):**
- Giữ candidate nếu `call_arity ∈ [min_params, max_params]` của nó, với `max_params = ∞` khi có
  varargs/rest/`*args`; `min_params` trừ số default/optional param. (đúng tinh thần "name+arity
  *permissive*" của InferCG [2]).
- Nếu sau lọc còn **>1** → vẫn `ambiguous` nhưng **tập nhỏ hơn** (giảm noise blast-radius).
- Nếu còn **đúng 1** → cân nhắc upgrade `resolved` (như nhánh C# namespace) *hoặc* thêm tier
  `inferred` nhẹ hơn — arity đơn lẻ là bằng chứng yếu hơn namespace nên đề xuất **giữ nhãn
  bảo thủ** (ambiguous-narrowed) trừ khi kết hợp thêm 1 tín hiệu (same-dir/import).

**Vì sao rẻ & an toàn:** thuần parse-tree, không cần toolchain/type-inference; áp cho **mọi ngôn ngữ**
cùng lúc (đặc biệt Kotlin/OCaml/C++/Dart đang 80–92% ambiguous). Precision-only (literature [1] xác
nhận không đụng recall). Regression test dễ seed (nhiều same-named khác arity).

**Cạm bẫy phải xử lý:** keyword/named args (Python/Kotlin), default params, varargs, currying (OCaml/
Haskell — arity không well-defined ⇒ **tắt gate cho ngôn ngữ curry-based**, giữ ambiguous cũ). Ghi rõ
per-language capability như cách CALM đã làm với same-dir/same-namespace.

**Đo tác động:** chạy lại `benchmarks/resolution` trước/sau; kỳ vọng giảm `ambiguous%` đáng kể cho
Kotlin/C++/Dart/Go mà `formal%`/`resolved%` không giảm sai.

### B. Mở rộng stack-graphs formal tier (song song, dài hạn hơn)
Mỗi ngôn ngữ có bộ TSG rules là một tầng **formal không phụ thuộc toolchain user** — bù đúng 9/19
ngôn ngữ Tier-0.5 chưa có SCIP provider. Ưu tiên theo ambiguous% × mức dùng thực tế (Kotlin, Swift,
Scala). Chi phí cao (viết/verify TSG rules) nhưng đúng SOTA [b] và không đòi user cài gì.

### C. Reranker sau RRF / fold `coreness` vào hybrid
- **Rẻ nhất:** áp chính boost self-audit-C5 (`coreness` + exact-name) **cả cho `search_hybrid`/
  `rrf_merge_n`**, không chỉ `search_symbol` — hiện `search_hybrid` (`search.rs:1012`) gọi thẳng
  `rrf_merge_n` nên bỏ lỡ tín hiệu structural. Đây là bất nhất còn lại của C5.
- **Nâng cao (tuỳ chọn):** cross-encoder reranker nhẹ trên top-N sau RRF ([f] đẩy recall@10
  78%→91%). Cần giữ triết lý local/pure-Rust ⇒ dùng model nhỏ hoặc **structural reranker**
  (kết hợp coreness + proximity theo file/import + arity match) thay vì neural — vẫn bám "no ONNX".

### D. Optional high-accuracy embedding backend
Giữ `potion-code-16M` làm mặc định (local-first, zero-network). Thêm feature cho phép trỏ tới một
code-specialized embedder mạnh hơn (Jina-code/Voyage-code-3/CodeXEmbed [g]) qua endpoint tuỳ chọn cho
user ưu tiên recall — opt-in, off-by-default, giữ nguyên guarantee "local-only" cho mặc định.

### E. Đóng nốt self-audit đã biết (xác nhận trạng thái rồi áp)
`docs/plans/2026-07-28-calm-read-tools-self-audit.md` — **C1–C5 dường như đã fix** (dedup
derived-edge + C5 search-ranking, xác nhận qua commit `fd8cefd`/code hiện tại). Còn cần kiểm & đóng
nếu vẫn mở:
- **F3** — `edit_context.blast_radius` không lọc edge `ambiguous` như `risk_assessment` đã lọc
  (bất nhất trong cùng 1 response). Sau khi có (A), tập ambiguous nhỏ hơn nhưng vẫn nên lọc.
- **F4** — caveat `NOT_FOUND` của `path`/`callers` không phân biệt "typo" vs "stdlib/builtin không
  index"; và fallback `hybrid` trả kết quả sai tự tin cho symbol như `println`. Chỉ là đổi message +
  điều kiện (rẻ).
- **F5** — `repo_overview.weak_cross_reference_languages` báo match-rate không kèm mẫu (N files) →
  dễ hiểu nhầm trên repo ít code ngôn ngữ đó. Thêm denominator + ẩn/flag khi N nhỏ.

### F. Benchmark task-correctness (B7/B8) — bằng chứng chiến lược
Theo Harness-Bench (swing 10–20 điểm SWE-Bench chỉ từ harness) và CORE-Bench/CodeRAG-Bench [c][d]:
loại bằng chứng thị trường định giá cao nhất là **CALM tăng tỷ lệ hoàn thành task**, không phải token.
B7 (task-correctness qua refactor thật, oracle = test pass/fail) xây được gần-miễn-phí; B8 (model-tier
leveling: "model rẻ + CALM ≈ model đắt không CALM") cần agent-loop harness. Đây là ROI định vị cao
nhất và cũng là vòng feedback để *đo* chính các thay đổi A–D ở trên.

---

## 5. Thứ tự đề xuất
1. **C-rẻ** (fold coreness vào hybrid RRF) + **E** (F3/F4/F5) — vài giờ mỗi cái, rủi ro ~0.
2. **A** (arity-gate) — đòn bẩy accuracy lớn nhất; ship kèm regression + đo lại `benchmarks/resolution`.
3. **F/B7** — để đo tác động A–D bằng task-correctness thật.
4. **D** (embedding backend tuỳ chọn) + **C-nâng cao** (reranker) — khi cần đẩy recall.
5. **B** (mở rộng stack-graphs) — đầu tư dài hạn, đúng SOTA, không đòi toolchain user.

---

## Nguồn ngoài
Học thuật:
- [1] [On the Recall of Static Call Graph Construction in Practice](https://consensus.app/papers/details/93f20309e7855a5898b1f0f293a01442/) (Sui et al., ICSE 2020)
- [2] [InferCG: Enhancing Python Call Graph Generation via Static Analysis and LLMs](https://consensus.app/papers/details/f9176ef478f6583eaef6c696919949a8/) (Xiang et al., TOSEM 2026)
- [3] [Reducing Static Analysis Unsoundness with Approximate Interpretation](https://consensus.app/papers/details/ec3040c357f55940b72e2d6c05ea4dad/) (Laursen et al., OOPSLA 2024)
- [4] [Static Call Graph Combination to Simulate Dynamic Call Graph Behavior](https://consensus.app/papers/details/133b9f06e48054528133ceb60ab9e6a4/) (Ságodi et al., IEEE Access 2022)
- [7] [PyCG: Practical Call Graph Generation in Python](https://consensus.app/papers/details/b2f145e1fe035745a8b387e3f4b74740/) (Salis et al., ICSE 2021)
- [9] [Unimocg: Modular Call-Graph Algorithms](https://consensus.app/papers/details/b3c95ff9a0d4523590581f5034f8f595/) (Helm et al., ISSTA 2024)

Kỹ thuật / benchmark:
- [b] [Stack graphs: Name resolution at scale](https://arxiv.org/pdf/2211.01224) (Creager et al.) · [Introducing stack graphs — GitHub Blog](https://github.blog/open-source/introducing-stack-graphs/)
- [c] [CORE-Bench: Code Retrieval in the Era of Agentic Coding](https://arxiv.org/pdf/2606.11864)
- [d] [CodeRAG-Bench: Can Retrieval Augment Code Generation?](https://aclanthology.org/2025.findings-naacl.176/)
- [e] [CoIR: A Comprehensive Benchmark for Code Information Retrieval](https://arxiv.org/html/2407.02883v3) (ACL 2025) · [repo](https://github.com/coir-team/coir)
- [f] [Hybrid Search: BM25, Vector & Reranking Reference 2026](https://www.digitalapplied.com/blog/hybrid-search-bm25-vector-reranking-reference-2026)
- [g] [CodeXEmbed: Generalist Embedding Model Family for Code Retrieval](https://arxiv.org/pdf/2411.12644)
- [h] [Granite Embedding R2 Models](https://arxiv.org/pdf/2508.21085)

---

# §6 — Round 2: Self-audit chính bản báo cáo này (2026-07-29)

Áp `super-skills` (evidence-before-claims + verification-before-completion) và **bài học meta của chính
CALM** ("đừng tin tool output *về chính nó* — query thẳng `.calm/index.db`", `docs/plans/2026-07-28-calm-read-tools-self-audit.md` §Meta-lesson). Mỗi claim gốc bị adversarial re-check. Kết quả: **1 sai
nghiêm trọng, 2 imprecise, 3 xác nhận đúng.**

## E1 — [SAI NGHIÊM TRỌNG] Đề xuất A sai tiền đề: arity-gate KHÔNG thiếu — nó đã tồn tại, đang Elixir-only

Bản gốc (§2, §4.A) khẳng định *"không có bước lọc theo arity ở bất kỳ đâu trong cascade"* và coi
"thêm arity-gate" là đòn bẩy #1 novel. **Cả hai vế đều sai — verify trực tiếp trên code + DB:**

- **Arity đã được capture cả hai vế:** `RawCall.arg_count` (`parser.rs:1330`, qua `count_arguments_node`),
  cột `symbols.arity` (schema, `edges.rs:27`), cột `call_sites.arg_count` (`pipeline.rs:687`), map
  `ctx.arity_by_qn` (`pipeline.rs:768`).
- **Arity-gate đã được cài đặt đầy đủ** (`pipeline.rs:949-983`), chạy **trước** cả module_hint/same_file/
  same_dir/same_namespace, với *đúng semantics tôi "đề xuất"*: narrow theo declared arity → **đúng 1
  survivor thì short-circuit thẳng `resolved`** (ngang chuẩn bằng chứng với same-namespace C#), fail-open
  khi arity unknown ở một trong hai vế, giữ ambiguous-narrowed khi >1.
- **Nó là Elixir-only CÓ CHỦ ĐÍCH**, không phải bỏ sót: trong Elixir `greet/1` và `greet/2` là hai hàm
  *khác nhau về danh tính* → arity là bằng chứng **sound**, không heuristic. Ở ngôn ngữ có overload/
  default-arg/vararg/currying, arity chỉ là tín hiệu **soft** — đúng cạm bẫy tôi tự nêu. Đội ngũ đã bắt
  đầu từ nơi arity soundest.

**Việc thật (đề xuất A đã sửa):** *tổng quát hoá* gate Elixir-only sang ngôn ngữ khác, KHÔNG phải xây mới.
Hạ tầng dữ liệu (arg_count/arity/arity_by_qn) đã có → rẻ hơn tôi tưởng ở phần data; nhưng công thật nằm ở
(a) populate `symbols.arity` khi extract cho nhiều ngôn ngữ hơn, (b) wire `count_arguments_node` cho nhiều
grammar hơn, (c) **xử lý soundness per-language** (giữ soft/fail-open cho overload/default/vararg, tắt hẳn
cho ngôn ngữ curry-based) rồi mới nới guard `== Some("elixir")`. InferCG [2] vì vậy **xác nhận một hướng
CALM ĐÃ áp dụng**, không phải "tính năng còn thiếu".

**Root-cause lỗi của tôi:** chỉ đọc `pipeline.rs:1000-1075` rồi kết luận "không có arity ở đâu cả" —
**vi phạm đúng meta-lesson tôi trích dẫn** (không grep `arity`/`arg_count`, không query DB trước khi khẳng
định một sự vắng mặt). Đây là lỗi Pattern-Globalization (không kiểm phạm vi) + "absence proof" từ một lần
đọc cục bộ.

## E2 — [IMPRECISE] "ambiguous 54-92% là trần accuracy" — over-generalized; chỉ đúng khi KHÔNG có SCIP overlay

Đo tươi trên `.calm/index.db` của chính repo này (Rust Tier-0, **có** rust-analyzer overlay):

| tier | edges | % |
|---|---:|---:|
| formal | 8410 | **76.3%** |
| ambiguous | 1421 | **12.9%** |
| textual | 763 | 6.9% |
| resolved | 385 | 3.5% |
| inferred | 48 | 0.4% |

→ Con số 54-92% ở §2 là baseline **no-overlay** (benchmark corpus ngoài, đa số chưa wire SCIP). Khi overlay
có mặt, ambiguous sụp xuống ~13%. **Hệ quả cho ưu tiên:** tổng quát-hoá arity-gate chủ yếu giúp (a) 9-13
ngôn ngữ Tier-0.5 *không có* SCIP provider, và (b) repo Tier-0 mà user *chưa* cài toolchain SCIP — **không**
giúp mấy cho repo đã cấu hình tốt. Điều này **hạ arity-gate khỏi vị trí "#1 tuyệt đối"**: các thắng lợi rẻ
& phổ quát (C: coreness/reranker cho hybrid — giúp *mọi* user *mọi* query) có ROI cao ngang hoặc hơn cho
đa số người dùng.

## E3 — [IMPRECISE] "coreness boost chỉ ở search_symbol, chưa vào hybrid" — coreness CÓ chảy vào 1/3 leg

`search_hybrid` (`search.rs:1019`) gọi `search_symbol` cho leg FTS → boost C5 (coreness + exact-name) **có**
định hình input FTS đưa vào RRF. Nó *chưa* áp cho 2 leg semantic (`symbol_semantic_results`/
`chunk_semantic_results`) và *chưa* có post-fusion re-rank. Vậy phát biểu "coreness vắng mặt khỏi hybrid"
là **quá mạnh** — chính xác phải là: coreness hiện diện ở 1/3 nguồn, chưa áp đồng nhất across-legs. Đề xuất
C (áp như re-rank sau fusion, hoặc boost cả nguồn semantic) vẫn đứng vững, chỉ là mức lợi nhỏ hơn tôi ngụ ý.

## Đã xác nhận ĐÚNG (re-verify, không đổi)

- **C1-C4 (dedup) đã fix thật** — không chỉ theo commit message: `.calm/index.db` có `idx_call_edges_unique`
  UNIQUE trên `(from_symbol,to_symbol,COALESCE(call_site_line,-1),edge_kind)` **và** `idx_import_edges_unique`;
  đo trực tiếp: **0 dup tuple** ở call_edges. → mục E gốc (liệt A "C1-C5 dường như đã fix") giờ là **đã
  verify**, bỏ chữ "dường như".
- **C5 (search ranking) đã fix** — `EXACT_NAME_SEED`/`CORENESS_WEIGHT`/`EXACT_NAME_BONUS` có thật trong
  `search_symbol` (`search.rs:262-331`), đọc trực tiếp.
- **Thiếu reranker sau RRF** — đúng: `rrf_merge_n` là bước cuối của `search_hybrid`, không có cross-encoder/
  post-fusion stage. Đề xuất C-nâng-cao và [f] vẫn đúng.

## Chưa verify độc lập (ghi nhận trung thực, rủi ro thấp)

- `potion-code-16M = static/model2vec`: lấy từ `docs/architecture.md`, chưa đọc `embedding.rs`/`build.rs`
  để xác nhận trực tiếp. Không đổi đề xuất D nhưng nên tự đọc trước khi trích ra ngoài.
- Con số ambiguous no-overlay (54-92%) lấy từ `benchmarks/resolution/README.md`; một số dòng README tự
  đánh dấu superseded/noisy (C/C++ struct-ref noise, Dart trước C3). Nếu cần số chuẩn, chạy lại benchmark.

## Ưu tiên đã cập nhật (thay bảng §5 gốc)

1. **C-rẻ** (coreness/exact-name cho cả hybrid RRF, không chỉ FTS-leg) + **E** (F3/F4/F5). Phổ quát, rủi ro ~0.
2. **A′ — tổng quát-hoá arity-gate** (không phải xây mới): mở rộng `symbols.arity` extraction + nới guard
   Elixir, per-language soundness. Ưu tiên ngôn ngữ arity-sound trước (function-clause languages), soft cho
   phần còn lại. Đo lại `benchmarks/resolution` trước/sau **chỉ trên ngôn ngữ no-overlay** (nơi nó thực sự có ích).
3. **F/B7** để đo tác động bằng task-correctness.
4. **D** (embedding backend tuỳ chọn) + reranker.
5. **B** (mở rộng stack-graphs formal tier) — dài hạn.

## Meta-lesson lặp lại (cho lần sau)

Tôi trích đúng bài học "query DB, đừng tin tool output về chính nó" rồi **vẫn** vi phạm nó ở claim trung tâm —
khẳng định một sự *vắng mặt* (arity-gate) chỉ từ một lần đọc code cục bộ, không grep, không query DB. Quy tắc
cứng rút ra: **mọi claim dạng "X không tồn tại / thiếu Y" phải đi kèm một `grep` toàn repo + (nếu là data)
một truy vấn DB, trước khi viết xuống.**
