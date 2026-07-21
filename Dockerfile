# syntax=docker/dockerfile:1

# ============ 构建阶段 ============
# 固定到项目当前使用的 stable 版本，保证可复现。
FROM rust:1.96-bookworm AS builder

# cpal 在 Linux 上通过 alsa-sys 链接 libasound，即使 --nats 模式不碰声卡，
# 编译期仍要 ALSA 头文件；pkg-config 用于 alsa-sys 定位它。
RUN apt-get update \
    && apt-get install -y --no-install-recommends pkg-config libasound2-dev \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /app

# 先只拷清单，用一个空壳 main 把依赖编译并缓存成独立的镜像层。
# 之后只改源码时，这一层命中缓存，不必重编上百个依赖。
COPY Cargo.toml Cargo.lock ./
RUN mkdir src \
    && echo 'fn main() {}' > src/main.rs \
    && cargo build --release \
    && rm -rf src

# 再拷真正的源码，只重编本 crate。
COPY src ./src
RUN touch src/main.rs && cargo build --release

# ============ 运行阶段 ============
FROM debian:bookworm-slim AS runtime

# libasound2：cpal 的运行时动态库依赖（--nats 模式不初始化设备，但二进制仍链接它）。
# ca-certificates：仅当你把 NATS 换成 tls:// 时才用得上，留着无妨。
# tini：PID 1 进程管理，确保信号正确转发给所有 iaxmon 子进程。
RUN apt-get update \
    && apt-get install -y --no-install-recommends libasound2 ca-certificates tini \
    && rm -rf /var/lib/apt/lists/* \
    && useradd --system --no-create-home --uid 10001 iaxmon

COPY --from=builder /app/target/release/iaxmon /usr/local/bin/iaxmon
COPY docker-entrypoint.sh /usr/local/bin/docker-entrypoint.sh
RUN chmod +x /usr/local/bin/docker-entrypoint.sh

WORKDIR /app
USER iaxmon

# 配置在运行时挂载到这里（见 docker-compose.yml），不烘进镜像。
# tini 做 PID 1：转发信号、收割僵尸进程。
# entrypoint 从配置中提取所有节点 ID，为每个节点启动一个 iaxmon 子进程。
ENTRYPOINT ["tini", "--", "docker-entrypoint.sh"]
