# Containerized mock-translate-api (GAP-05): the template repo CI and any
# consumer can start the full mock in ~1s without a Rust toolchain.
#
# Build (from THIS directory as the context — keeps the client target/ tree
# out of the context):
#   docker build -t ghcr.io/wpmmcc/mock-translate-api:latest .
# Image is a static musl binary on scratch (~8MB, well under the 20MB budget).
FROM rust:1-alpine AS builder
WORKDIR /build
RUN apk add --no-cache musl-dev
COPY Cargo.toml Cargo.lock ./
COPY src ./src
COPY fixtures ./fixtures
COPY media ./media
RUN cargo build --release --target x86_64-unknown-linux-musl

FROM scratch
COPY --from=builder /build/target/x86_64-unknown-linux-musl/release/mock-translate-api /mock-translate-api
EXPOSE 9090
ENTRYPOINT ["/mock-translate-api"]
