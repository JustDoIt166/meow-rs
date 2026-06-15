# ---- chef：基础系统工具 + cargo-chef ----
FROM rust:1.89-slim-bookworm AS chef
 
RUN apt-get update && apt-get install -y --no-install-recommends \
    cmake \
    g++ \
    make \
    pkg-config \
    perl \
    git \
    clang \
    libclang-dev \
    llvm-dev \
    golang-go \
    nasm \
    && rm -rf /var/lib/apt/lists/*
 
RUN cargo install cargo-chef --locked
WORKDIR /usr/src/meow-rs
 
# ---- planner：分析依赖 ----
FROM chef AS planner
COPY . .
RUN cargo chef prepare --recipe-path recipe.json
 
# ---- builder：编译依赖 + 最终 binary ----
FROM chef AS builder
COPY --from=planner /usr/src/meow-rs/recipe.json recipe.json
RUN cargo chef cook --release --recipe-path recipe.json
 
COPY . .
RUN cargo build --release --bin meow
 
# ---- runtime：最小运行环境 ----
FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y --no-install-recommends \
    ca-certificates \
    libstdc++6 \
    && rm -rf /var/lib/apt/lists/* \
    && addgroup --system meow \
    && adduser --system --ingroup meow meow \
    && mkdir -p /etc/meow
 
COPY --from=builder /usr/src/meow-rs/target/release/meow /usr/local/bin/meow
COPY --from=builder /usr/src/meow-rs/config.example.yaml /etc/meow/config.yaml
RUN chown meow:meow /etc/meow/config.yaml
 
USER meow
EXPOSE 7890 9090 1053/udp 1053/tcp
VOLUME ["/etc/meow"]
ENTRYPOINT ["meow"]
CMD ["-f", "/etc/meow/config.yaml"]
