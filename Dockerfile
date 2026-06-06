FROM rust:1.97.1-alpine AS builder

WORKDIR /build
RUN apk add --no-cache musl-dev
COPY Cargo.toml Cargo.lock ./
COPY crates ./crates
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/build/target \
    cargo build --locked --release -p ctx-cli -p ctx-mcp \
    && mkdir /artifacts \
    && cp target/release/ctx target/release/ctx-mcp /artifacts/

FROM alpine:3.23 AS runtime

RUN apk add --no-cache ca-certificates git \
    && addgroup -g 10001 -S ctx \
    && adduser -u 10001 -S -D -H -G ctx ctx

COPY --from=builder /artifacts/ctx /usr/local/bin/ctx
COPY --from=builder /artifacts/ctx-mcp /usr/local/bin/ctx-mcp

USER ctx:ctx
WORKDIR /workspace
ENTRYPOINT ["ctx"]
CMD ["--help"]
