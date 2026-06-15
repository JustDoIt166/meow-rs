# ---- Builder ----
FROM rust:1.89-alpine AS builder
 
RUN apk add --no-cache \
    cmake g++ make perl go \
    # BoringSSL 还需要这些
     clang llvm-dev \
    git linux-headers
 
WORKDIR /usr/src/meow-rs
 
# 先拷贝依赖声明，利用 Docker 缓存
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
 
# 创建空 lib.rs 以便 cargo 仅解析依赖
RUN find crates -name Cargo.toml -exec bash -c ' \
    dir=$(dirname {}); \
    [ -f "$dir/src/lib.rs" ] || mkdir -p "$dir/src" && touch "$dir/src/lib.rs"; \
  ' \;
 
# 预编译依赖（缓存层）
RUN cargo build --release --bin meow 2>/dev/null || true
 
# 拷贝全部源码，真正编译
COPY . .
RUN touch crates/*/src/lib.rs crates/meow-app/src/main.rs
RUN cargo build --release --bin meow
 
# ---- Runtime ----
FROM alpine:3.21
 
RUN apk add --no-cache ca-certificates
 
RUN addgroup -S meow && adduser -S meow -G meow
 
COPY --from=builder /usr/src/meow-rs/target/release/meow /usr/local/bin/meow
COPY --from=builder /usr/src/meow-rs/config.example.yaml /etc/meow/config.yaml
 
RUN chown meow:meow /etc/meow/config.yaml
 
USER meow
 
EXPOSE 7890 9090 1053/udp 1053/tcp
 
VOLUME ["/etc/meow"]
 
ENTRYPOINT ["meow"]
CMD ["-f", "/etc/meow/config.yaml"]
