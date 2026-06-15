# ---- Builder ----
FROM rust:1.89-alpine AS builder
 
RUN apk add --no-cache \
    # ---- BoringSSL cmake 编译 ----
    cmake \
    g++ \
    make \
    perl \
    go \
    nasm \
    # ---- bindgen (libclang) ----
    clang \
    clang-dev \
    llvm-dev \
    # ---- Rust 基础 ----
    musl-dev \
    linux-headers
 
WORKDIR /usr/src/meow-rs
 
# 依赖缓存层
COPY Cargo.toml Cargo.lock ./
COPY crates/meow-common/Cargo.toml   crates/meow-common/Cargo.toml
COPY crates/meow-trie/Cargo.toml     crates/meow-trie/Cargo.toml
COPY crates/meow-dns/Cargo.toml      crates/meow-dns/Cargo.toml
COPY crates/meow-rules/Cargo.toml    crates/meow-rules/Cargo.toml
COPY crates/meow-transport/Cargo.toml crates/meow-transport/Cargo.toml
COPY crates/meow-proxy/Cargo.toml    crates/meow-proxy/Cargo.toml
COPY crates/meow-tunnel/Cargo.toml   crates/meow-tunnel/Cargo.toml
COPY crates/meow-listener/Cargo.toml crates/meow-listener/Cargo.toml
COPY crates/meow-api/Cargo.toml      crates/meow-api/Cargo.toml
COPY crates/meow-config/Cargo.toml   crates/meow-config/Cargo.toml
COPY crates/meow-app/Cargo.toml      crates/meow-app/Cargo.toml
 
RUN find crates -name Cargo.toml -exec sh -c ' \
    dir=$(dirname {}); \
    mkdir -p "$dir/src" && touch "$dir/src/lib.rs"; \
  ' \;
 
# Alpine 上 libclang.so 可能在 /usr/lib/llvmXX/lib/ 下
# bindgen 未必能自动找到，显式设置 LIBCLANG_PATH
ENV LIBCLANG_PATH=/usr/lib
 
RUN cargo build --release --bin meow 2>/dev/null || true
 
COPY . .
RUN touch crates/*/src/lib.rs crates/meow-app/src/main.rs
RUN cargo build --release --bin meow
 
# ---- Runtime ----
FROM alpine:3.21
 
RUN apk add --no-cache ca-certificates \
    && addgroup -S meow && adduser -S meow -G meow
 
COPY --from=builder /usr/src/meow-rs/target/release/meow /usr/local/bin/meow
COPY config.example.yaml /etc/meow/config.yaml
 
RUN chown meow:meow /etc/meow/config.yaml
 
USER meow
 
EXPOSE 7890 9090 1053/udp 1053/tcp
 
VOLUME ["/etc/meow"]
 
ENTRYPOINT ["meow"]
CMD ["-f", "/etc/meow/config.yaml"]
