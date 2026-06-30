# syntax=docker/dockerfile:1.7

FROM rust:1-slim-bookworm@sha256:4732ca96fd086cb9be682050c3f0176288eebaac2b80aa2bcefccfaf198e1950 AS builder

WORKDIR /app

# aws-lc-sys 与 mimalloc 都需要 C/C++ 构建工具；ca-certificates 用于拉取 crates/git 依赖。
RUN apt-get update \
    && apt-get install -y --no-install-recommends \
        ca-certificates=20230311+deb12u1 \
        cmake=3.25.1-1 \
        g++=4:12.2.0-3 \
        make=4.3-4.1 \
        perl=5.36.0-7+deb12u3 \
        pkg-config=1.8.1-1 \
    && rm -rf /var/lib/apt/lists/*

COPY Cargo.toml Cargo.lock ./
COPY src ./src
COPY config.toml.example ./config.toml.example

RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/usr/local/cargo/git \
    --mount=type=cache,target=/app/target \
    cargo build --release --locked \
    && cp /app/target/release/alma-onebot-bridge /usr/local/bin/alma-onebot-bridge

FROM debian:bookworm-slim@sha256:60eac759739651111db372c07be67863818726f754804b8707c90979bda511df AS runtime

ARG VERSION=dev
LABEL org.opencontainers.image.title="Alma OneBot Bridge" \
      org.opencontainers.image.description="Bridge service connecting Alma to QQ through OneBot v11" \
      org.opencontainers.image.licenses="AGPL-3.0-only" \
      org.opencontainers.image.version="${VERSION}"

RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates=20230311+deb12u1 \
    && rm -rf /var/lib/apt/lists/* \
    && useradd --create-home --uid 10001 --shell /usr/sbin/nologin bridge \
    && mkdir -p /config /data /home/bridge/.config/alma/groups /home/bridge/.config/alma/people \
    && chown -R bridge:bridge /config /data /home/bridge/.config/alma

COPY --from=builder /usr/local/bin/alma-onebot-bridge /usr/local/bin/alma-onebot-bridge
COPY --chown=bridge:bridge config.toml.example /config/config.toml.example

USER bridge
WORKDIR /data

ENV ALMA_ONEBOT_BRIDGE_CONFIG=/config/config.toml \
    RUST_LOG=info

EXPOSE 8090
VOLUME ["/config", "/data", "/home/bridge/.config/alma"]

ENTRYPOINT ["/usr/local/bin/alma-onebot-bridge"]
