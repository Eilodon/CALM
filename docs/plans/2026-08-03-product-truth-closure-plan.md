---
title: "Product-Truth Closure Plan — doc-drift gate cho hand-authored docs + guarantee-level taxonomy + benchmark claim registry"
date: 2026-08-03
status: PR1 IMPLEMENTED (cùng commit với doc này) — 6 file sửa về 34 tools + hàng `txn` thêm
  vào bảng Group/Tools của README + preset `edit` đồng bộ trong AGENTS.md +
  `scripts/check-doc-truth.sh` mới + wired vào `ci.yml`/`release.yml`. PR2 (guarantee-level
  taxonomy) và PR3 (benchmark claim registry) còn ở dạng thiết kế, chưa code — xem §4.
scope: đóng khoảng trống "capability contract" còn lại sau WS-13 — status.generated.md
  đã CI-gated đúng, nhưng README/AGENTS.md/llms.txt/marketplace.json/plugin.json/workflow.rs
  vẫn hand-typed và đã trôi dạt, ngay cả sau khi v0.5.0 ship (2026-08-03). Không đụng WS-4/
  WS-5/WS-6/WS-9 — các workstream đó P1, chưa bắt đầu, cần phiên riêng theo đúng kỷ luật
  reconciliation-round2.md §5 ("không bắt đầu cơ hội chủ nghĩa").
inputs:
  - docs/status.generated.md               # nguồn đúng, CI-gated (gen-status.sh --check)
  - docs/plans/2026-08-01-calm-master-upgrade-plan.md   # WS-13 gốc
  - docs/plans/2026-08-02-reconciliation-round2.md      # N2 (tool surface), §3 bảng trạng thái
  - docs/plans/2026-08-02-toolsurface-writesafety-ledger-research.md  # §1 (tool surface 30→34)
verified_against: HEAD b4c43d7 (workspace 0.5.0, tag hôm nay) — mọi số liệu dưới đây đọc trực
  tiếp qua mcp__calm__source/search + grep trong phiên này, không suy đoán từ CHANGELOG
---

# Product-Truth Closure Plan

## 0. Vì sao viết plan này thay vì code luôn

Repo này có kỷ luật rõ: mọi thay đổi có phán đoán thiết kế (kể cả nhỏ) đi qua một doc trước
(15 plan doc trong `docs/plans/` chỉ riêng từ 2026-08-01 đến nay). Việc này tự nó thuộc **Tier A**
theo phân loại của `priority-reaudit.md`/`reconciliation-round2.md` §5 (rủi ro thấp, effort tự
chứa, giá trị rõ — không đổi hành vi write-path, không cần shadow mode) nên không cần threat-model
riêng như WS-2/WS-4, nhưng vẫn nên có một doc ngắn để bất kỳ ai (hoặc phiên Claude nào) đọc lại
sau này biết chính xác con số nào đúng, tại sao lệch, và gate nào giữ nó không lệch lại.

## 1. Bằng chứng lệch — xác minh trực tiếp trên HEAD hiện tại (không phải bản cũ)

`docs/status.generated.md` sinh từ `crates/calm-server/src/__toolsnaps__/*.snap` +
`crates/calm-core/Cargo.toml` `[features]`, CI check qua `scripts/gen-status.sh --check`
(`ci.yml` job `status-drift`, `release.yml` job `qualify-release`) — **đã đúng, đã chạy**:
`bash scripts/gen-status.sh --check` → `up to date (34 tools)`.

Nhưng 6 file hand-authored sau **vẫn nói 30 (hoặc 22) tools**, xác minh bằng grep trực tiếp trên
HEAD `b4c43d7`, tức **sau khi v0.5.0 (4 tool `txn` mới) đã merge**:

| File | Dòng | Nói gì | Đúng phải là |
|---|---|---|---|
| `README.md` | 178 | "exposing 30 tools" | 34 tools |
| `README.md` | 202 | heading `## 30 MCP tools for AI agents` | `## 34 MCP tools for AI agents` |
| `README.md` | 205-213 | bảng `Group \| Tools` — đếm thủ công 7 nhóm ra đúng **30**, thiếu hẳn 4 tool `txn` mới (`edit_transaction_status`, `maintenance_status`, `retry_maintenance`, `repair_consistency`) | thêm 1 nhóm mới, tổng 34 |
| `AGENTS.md` | 15 | banner "30 tools. 8 stages." | 34 tools |
| `AGENTS.md` | 292 | Preset Reference, hàng `edit` — liệt kê 12 tool, thiếu 4 tool `txn` | 16 tool (khớp `toolset.rs:38-59`) |
| `AGENTS.md` | 294 | "All 30 tools" | "All 34 tools" |
| `llms.txt` | 5 | "22 tools total" | 34 tools total |
| `llms.txt` | 24 | "the 22 MCP tools" | "the 34 MCP tools" |
| `.claude-plugin/marketplace.json` | 11 | "30 tools total" | 34 tools total |
| `plugins/calm/.claude-plugin/plugin.json` | 3 | "30 tools total" | 34 tools total |
| `crates/calm-core/src/workflow.rs` | 24 | "8 stages, ~30 tools" | 34 tools |

Chi tiết quan trọng: **AGENTS.md dòng 292 không chỉ là con số sai** — nó liệt kê sai *nội dung*
preset `edit` thật. Code (`toolset.rs:38-59`, đọc trực tiếp) xác nhận preset `edit` **đã có** 4 tool
`txn` từ commit `1830328`/`7b65acb` (WS-1 shadow-mode). Đây không phải lỗi đánh máy số đếm — đây là
tài liệu mô tả sai hành vi thật của server. Đối chiếu 3 preset còn lại (`orient`/`trace`/`compound`)
— khớp 100% với `toolset.rs`, không cần sửa.

Phát hiện phụ (ghi lại, không xử lý trong PR này — xem §5 Anti-goals): `plugin.json:4` có
`"version": "0.3.3"` trong khi workspace đã ở 0.5.0 — có thể là chủ đích (plugin có chu kỳ
release riêng khỏi crate) hoặc là một lệch khác; cần người biết chính sách versioning của plugin
xác nhận, không đoán trong PR này.

## 2. Vì sao lệch này *sẽ lặp lại* nếu chỉ sửa số một lần

`gen-status.sh --check` chỉ bảo vệ `status.generated.md` — không file nào trong 6 file trên nằm
trong phạm vi gate đó. Bằng chứng: 4 tool `txn` merge cách đây 1 commit trong cùng ngày hôm nay,
`status.generated.md` được regenerate đúng (CI xanh), nhưng cả 6 file kia vẫn trôi — nghĩa là gate
hiện tại **để lọt đúng loại lỗi nó được sinh ra để chặn** (comment gốc của `gen-status.sh`, dòng
9-13, trích VHEATM: *"CALM's own status must never drift the way VHEATM's own README ('pilot')
drifted from its own registry ('complete')"*) — chỉ là phạm vi chưa đủ rộng.

## 3. Thiết kế — `scripts/check-doc-truth.sh`

Không templatize 6 file này (chi phí thiết kế lớn, rủi ro làm hỏng văn phong prose đã viết tay cẩn
thận — xem `README.md`/`AGENTS.md` đầy chú thích tinh tế). Thay vào đó, đúng pattern đã có
(`gen-status.sh`: bash + jq, `--check` exit code), thêm một **check script mới, riêng biệt**, đối
chiếu số trong 6 file trên với con số thật trong `status.generated.md`:

```bash
#!/usr/bin/env bash
# Fails if any hand-authored doc hardcodes a tool count that no longer
# matches docs/status.generated.md (the CI-gated source of truth). Doesn't
# touch status.generated.md itself -- gen-status.sh --check already owns
# that. This closes the gap found 2026-08-03: 6 files still said "30
# tools"/"22 tools" a full release after a 30->34 bump had already landed
# and status.generated.md had already been regenerated correctly.
set -euo pipefail
cd "$(dirname "$0")/.."

status_file="docs/status.generated.md"
authoritative=$(grep -oE '## MCP tool inventory \([0-9]+ tools\)' "$status_file" \
    | grep -oE '[0-9]+')
if [ -z "$authoritative" ]; then
    echo "check-doc-truth: could not read tool count from $status_file" >&2
    exit 1
fi

files=(
    README.md
    AGENTS.md
    llms.txt
    .claude-plugin/marketplace.json
    plugins/calm/.claude-plugin/plugin.json
    crates/calm-core/src/workflow.rs
)

fail=0
for f in "${files[@]}"; do
    [ -f "$f" ] || continue
    while IFS= read -r match; do
        [ -n "$match" ] || continue
        n=$(echo "$match" | grep -oE '^[0-9]+')
        if [ "$n" != "$authoritative" ]; then
            echo "check-doc-truth: $f says \"$match\" but $status_file says $authoritative tools" >&2
            fail=1
        fi
    done < <(grep -ohE '[0-9]+ (MCP )?tools\b' "$f" || true)
done

if [ "$fail" -ne 0 ]; then
    echo "check-doc-truth: update the numbers above (or regenerate $status_file if it's the stale one)" >&2
    exit 1
fi
echo "check-doc-truth: all hand-authored tool-count references match ($authoritative tools)."
```

Wire vào đúng 2 chỗ `gen-status.sh --check` đã có mặt (giữ cùng cặp CI job, không tạo job mới):
- `.github/workflows/ci.yml` job `status-drift`, thêm step ngay sau dòng 309.
- `.github/workflows/release.yml` job `qualify-release`, thêm step ngay sau dòng 63.

Không kiểm tra `AGENTS.md:292`'s nội dung preset `edit` bằng script này (đối chiếu *danh sách*
tool, không phải con số, là bài toán khác — không tự động hoá trong phạm vi PR này; sửa tay lần
này, để lại như một known-manual-sync-point, ghi rõ trong comment tại `toolset.rs`/`preset_tools`
trỏ ngược về `AGENTS.md` để lần sau ai sửa preset nhớ sync tay — tương tự cách
`toolset_names_match_calm_core_valid_toolset_names` đã tự động hoá phần *có thể* tự động hoá và
để phần còn lại làm test/comment).

## 4. PR breakdown

| PR | Nội dung | Effort | Rủi ro |
|---|---|---|---|
| PR1 (doc này + code) | Sửa 6 file (11 vị trí) về đúng 34 + thêm hàng `txn` vào bảng Group/Tools của README + đồng bộ preset `edit` trong AGENTS.md + `check-doc-truth.sh` + wire 2 CI job | ~1-2h | Không — thuần văn bản + 1 script mới, không đổi runtime behavior |
| PR2 (sau, chưa code) | Guarantee-level taxonomy: `docs/guarantee-levels.toml` (enforced/advisory/best_effort/optional/provider_dependent/unsupported per hành vi) + `gen-status.sh` sinh thêm section `## Guarantee levels` từ file đó | ~2-3h | Thấp — thuần generation, cần review nội dung taxonomy cho đúng |
| PR3 (sau, chưa code) | `benchmarks/claims.registry.jsonl` + `scripts/check-claims-registry.sh`, backfill 2 chuỗi supersede đã có (B10→B11, Dart pre/post-C3) | ~half day | Thấp — file mới + script mới, không đổi benchmark cũ |

PR này (PR1) là phần được triển khai ngay bên dưới. PR2/PR3 để lại như kế hoạch đã thiết kế, chưa
code — cả hai cần một vòng review nội dung (taxonomy entries thật, schema field thật) trước khi
hợp lý để tự động chạy trong CI, đúng tinh thần "rủi ro thấp nhưng effort không nhỏ, nên tách PR"
mà `reconciliation-round2.md` §5 áp dụng cho WS-2 Phase 2 so với P0-4.

## 5. Anti-goals / việc KHÔNG nằm trong plan này

- **Không** đụng WS-4 (provider sandbox), WS-5 (evidence lattice), WS-6 (verification pipeline /
  `VERIFY_PENDING`), WS-9 (workflow MCP trust-split) — cả 4 đều P1+, chưa bắt đầu, và
  `reconciliation-round2.md` §5 đã nói rõ: không bắt đầu cơ hội chủ nghĩa, mỗi cái cần phiên
  research+plan riêng.
- **Không** sửa `plugin.json`'s `version: 0.3.3` — nêu ở §1 như một phát hiện phụ, không phải một
  quyết định của plan này (không rõ chính sách versioning độc lập của plugin).
- **Không** templatize README/AGENTS.md (biến chúng thành file sinh tự động) — chi phí thiết kế +
  rủi ro mất văn phong tay viết không tương xứng với lợi ích so với việc chỉ thêm 1 gate.
  Nếu drift tái diễn nhiều lần nữa sau khi gate này đã có, đó là tín hiệu nên revisit quyết định
  này — không phải bây giờ.
- **Không** build policy replay/simulator (đã đề xuất ở phiên phân tích trước) — phụ thuộc gate-deny
  events được ghi vào `audit_ledger`, mà `reconciliation-round2.md` §3 dòng cuối tự ghi: "Phase 2
  (sau, không bundle chung) — gate deny events" — nghĩa là đội đã cố ý hoãn phần ghi log đó. Xây
  simulator trước khi có dữ liệu để replay là làm ngược thứ tự.
