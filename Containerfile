# Multi-stage build: compile a static musl binary, ship it on `scratch`.
#
# Alpine's host target is x86_64-unknown-linux-musl, so a plain `cargo build`
# already yields a static binary — no glibc, no runtime image needed.

FROM rust:alpine AS builder
# build-base: C toolchain for tree-sitter / stack-graphs / bundled SQLite.
RUN apk add --no-cache build-base
WORKDIR /build
COPY . .
RUN cargo build --release --bin calm \
    && strip target/release/calm

FROM scratch
COPY --from=builder /build/target/release/calm /calm
# Mount the project read-only at /project; the index DB lives on a writable
# volume at /data. MCP uses stdio transport, so attach the client to stdin/stdout.
ENTRYPOINT ["/calm"]
CMD ["serve", "--project-root", "/project", "--db-path", "/data/index.db"]
