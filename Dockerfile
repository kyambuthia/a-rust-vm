# syntax=docker/dockerfile:1

FROM rust:1.93-bookworm AS builder
RUN rustup target add wasm32-unknown-unknown
WORKDIR /app
COPY Cargo.toml Cargo.lock ./
COPY src ./src
COPY web ./web
RUN cargo build --release && \
    cargo build --target wasm32-unknown-unknown --lib --release

FROM debian:bookworm-slim AS runtime
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates && \
    rm -rf /var/lib/apt/lists/* && \
    groupadd --gid 10001 arvm && \
    useradd --uid 10001 --gid 10001 --create-home --shell /usr/sbin/nologin arvm && \
    mkdir -p /app/web /app/target/wasm32-unknown-unknown/release /app/target/wasm32-unknown-unknown/debug && \
    chown -R arvm:arvm /app
WORKDIR /app
COPY --from=builder --chown=arvm:arvm /app/target/release/arvm ./target/release/arvm
COPY --from=builder --chown=arvm:arvm /app/target/wasm32-unknown-unknown/release/a_rust_vm.wasm ./target/wasm32-unknown-unknown/release/a_rust_vm.wasm
COPY --chown=arvm:arvm web ./web
USER 10001:10001
EXPOSE 8080
ENV A_RVM_WEB_PORT=8080
ENTRYPOINT ["./target/release/arvm", "serve"]
