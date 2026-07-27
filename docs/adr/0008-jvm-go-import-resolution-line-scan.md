# ADR-0008: Resolve JVM và Go imports bằng line-scan riêng theo hệ sinh thái (`jvm_package.rs`/`go_module.rs`), không dùng `DeclaredScopeMap` chung

- **Status**: Accepted & Implemented — shipped 2026-07-27 (Tier A resolver audit). Commit: `30b8369` (`feat(indexer): resolve JVM and Go imports from declarations, not layout guesses`).
- **Date**: 2026-07-27
- **Decision makers**: TBD (draft do Claude chuẩn bị theo yêu cầu, cần chủ dự án duyệt)
- **Related**: ADR-0002 (Formal Resolver), ADR-0009 (cùng branch — ambiguous traversal semantics), `crates/calm-core/src/indexer/csharp_namespace.rs` (NamespaceMap, tiền lệ tree-sitter cho C#)

## Context

`resolve_module_to_path` (`pipeline.rs`) đã có resolver riêng theo hệ sinh thái: Rust (crate map), PHP (PSR-4), C# (`NamespaceMap`, tree-sitter), Python (`sys.path`). JVM và Go chỉ có scan chung "thử project root, rồi `src/`" — sai theo hai hướng ngược nhau:

- **JVM**: Maven/Gradle đặt source dưới `src/main/java/<package path>`, scan chung không bao giờ tìm thấy → MỌI import Java/Kotlin/Groovy giữ `to_path = NULL`. Đo được: 0/22 import first-party thật trên spring-petclinic, 0/14 trên kotlinpoet, 0/459 trên http-builder-ng.
- **Go**: resolution sai hoàn toàn ngược — `errors`/`path`/`context`/`path/filepath` (stdlib Go) lại bind vào file cùng tên của chính gin (`errors.go`/`path.go`/`context.go`), trong khi mọi import theo module-path thật của gin lại resolve ra rỗng. Tức 100% cái resolve được là sai, và 100% cái đúng first-party lại bị bỏ sót.

## Decision

Hai module mới, **tách riêng, không dùng chung một `DeclaredScopeMap`**:

- **`jvm_package.rs` (`JvmPackageMap`)**: line-scan dòng `package a.b.c;` đầu file, map package → file khai báo, có `resolve_type`/`lookup` xử lý wildcard import và static import.
- **`go_module.rs` (`GoModule`)**: line-scan dòng `module ...` trong `go.mod` (đúng 1 dòng/repo, không phải theo file), cộng quy tắc "phần tử đầu import path không có dấu chấm ⇒ stdlib" (`is_stdlib`) trước khi thử khớp trực tiếp trong module (`owns`/`package_dir`).

**Vì sao 2 module riêng, không phải 1 struct chung:**
1. Luật resolve khác nhau về BẢN CHẤT, không chỉ khác hình dạng dữ liệu — JVM cần xử lý wildcard/static import (không có ở Go); Go cần luật stdlib-vs-domain-qualified (không có ở JVM). Gộp chung sẽ vẫn cần 2 phương thức `resolve` hoàn toàn khác nhau bên trong — không tiết kiệm được gì.
2. Phạm vi "scope" khác cấp: Go là 1 `go.mod` cho cả repo; JVM là dòng `package` của TỪNG FILE riêng.

**Vì sao line-scan, không phải tree-sitter walk (như `NamespaceMap` của C# đã làm):** khai báo luôn là **một dòng đầu file cố định** ở cả hai hệ sinh thái — parse cả cây cú pháp không mua thêm gì. Quan trọng hơn: **`lang-scala`/`lang-groovy` là Cargo feature opt-in, không nằm trong bundle `tier0-5` mặc định** (xác nhận: `Cargo.toml`, `ci.yml`) — một reader dựa tree-sitter sẽ lặng lẽ trả về rỗng cho file Scala/Groovy trên build mặc định. `lang-cpp` thì nằm sẵn trong `tier0-5`, nên an toàn cho hướng tree-sitter riêng (xem ADR-0009's related work, B1 trong audit kế hoạch).

## Consequences

**Tích cực:** 22/22 Java, 1/14 Kotlin, 56/459 Groovy (2 corpus sau import chủ yếu thư viện ngoài, nên mẫu số nhỏ có chủ đích) trên spring-petclinic/kotlinpoet/http-builder-ng resolve đúng. Go: 31/31 import trong-module resolve, stdlib resolve về rỗng (trước đó ngược cả hai chiều). Benchmark's denominator cũng được sửa cùng đợt (commit `ddb3a8d`) vì cùng gốc lỗi tên-thư-mục-trùng (`java`/`org`/`com` là tên thư mục thật trong cây Maven).

**Tiêu cực / nợ ghi nhận:** giờ có 3 module "declaration-scan → map → resolve" riêng biệt không chung interface (`JvmPackageMap`, `GoModule`, và `NamespaceMap` của C# — cái này dùng tree-sitter thật). Chấp nhận được vì mỗi cái có luật khác nhau đủ nhiều (xem trên); không thử ép chung vì sẽ phải trừu tượng hoá cả kiểu tree-sitter lẫn line-scan cùng lúc.

## Alternatives Considered

- **`DeclaredScopeMap` chung, tham số hoá theo hệ sinh thái** — bác bỏ: không giảm code thật, vì phần khó (wildcard import của JVM, luật stdlib của Go) nằm ở phần riêng biệt, không phải ở khung "declaration → map".
- **Tree-sitter walk giống `NamespaceMap`** — bác bỏ vì `lang-scala`/`lang-groovy` là opt-in feature; một resolver bắt buộc parse cây sẽ hồi quy về 0 lặng lẽ trên build không bật 2 feature này.

## Evidence

Commit `30b8369` [verified 2026-07-27]: `cargo test`: 1119 passed, 0 failed (bao gồm cả integration test trong `tests/`, không chỉ `--lib`). Số liệu corpus (22/22, 1/14, 56/459, 31/31) lấy trực tiếp từ nội dung commit message, đối chiếu với `benchmarks/resolution/` — không tự mô phỏng lại trong ADR này.

## Owner

TBD — theo đúng quy ước ADR hiện có của repo (xem ADR-0007's "Decision makers: TBD").

## Known Debts (PATTERN-DEBT)

Không tạo nợ PATTERN-DEBT mới trực tiếp từ quyết định này. `call-edges-missing-ruled-out-filter` (11 site) là nợ riêng, phát sinh từ commit đồng hành `8634111` — xem ADR-0009.

## Next Cycle Trigger

Khi một hệ sinh thái thứ 5 cần resolver declaration-scan riêng của chính nó (sau JVM/Go/C#/Rust/PHP/Python — đã 6), xem lại có đáng trừu tượng hoá thành trait chung không. HOẶC: khi C++ namespace map (tree-sitter, nested/reopen) lên (B1 trong audit kế hoạch) — lúc đó tree-sitter đã có 2 tiền lệ thật (C#, C++) đối lại 2 tiền lệ line-scan (JVM, Go), đáng quyết định tách chính thức.

## Cycle Retrospective

- Giả định sai lúc đầu: coi lỗi JVM và Go là "cùng 1 loại bug" — thực ra là hai lỗi độc lập, sai theo hai hướng ngược nhau (JVM không bao giờ khớp, Go luôn khớp sai).
- Bất ngờ: lỗi Go (`errors.go` của gin nuốt luôn import `errors` chuẩn) không crash, không có test nào bắt được trước khi đo trên corpus thật — "100% resolved" trông khoẻ mạnh trong khi 100% sai.
- Nếu làm lại: đo corpus thật SỚM HƠN, trước khi viết fix — sẽ thấy "hướng ngược nhau" nhanh hơn.
- Nợ chủ đích: 3 module `JvmPackageMap`/`GoModule`/`NamespaceMap` không chung interface — chấp nhận, xem Consequences.
- Tín hiệu cần theo dõi: xem Next Cycle Trigger.
