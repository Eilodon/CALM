# ADR-0001: Stack Graphs Scope Decision

- **Status**: Accepted
- **Date**: 2026-06-29
- **Decision makers**: ybao

## Context

Code Intelligence cần resolver chính xác hơn ConservativeResolver cho các ngôn ngữ
có duck typing, dynamic dispatch (Python), complex module resolution (TypeScript).
Stack Graphs (`tree-sitter-stack-graphs`) cung cấp formal name-binding resolution
dựa trên scope graph theory.

Tuy nhiên, không phải mọi ngôn ngữ đều có public Stack Graphs rules sẵn.

## Decision

**Tier-0 formal resolution** (Stack Graphs, implement tại Phase 2):
- Python — `tree-sitter-stack-graphs-python` (public)
- TypeScript — `tree-sitter-stack-graphs-typescript` (public)
- JavaScript — (shared rules với TypeScript)
- Java — `tree-sitter-stack-graphs-java` (public)

**ConservativeResolver** (port nguyên từ Python, giữ lâu dài):
- Rust, Go, C, C++, Ruby, PHP
- Đây không phải fallback hạng hai — là resolver chính cho nhóm ngôn ngữ chưa có public rules.

## Consequences

- `EdgeConfidence` sẽ thêm tier `formal` (đứng trên `resolved`) khi Phase 2 ship.
- CONTRACTS.md và mcp_types.ts phải cập nhật đồng bộ.
- Nếu Phase 2 trễ, hệ thống vẫn hoạt động đầy đủ với ConservativeResolver cho mọi ngôn ngữ.
- Mỗi ngôn ngữ Stack Graphs ship riêng PR, không gộp.

## Risks

- DSL learning curve cho `tree-sitter-graph` stanza files.
- Mitigation: Phase 1B ship-able độc lập trước Phase 2.
