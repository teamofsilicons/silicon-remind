# syntax=docker/dockerfile:1.7

FROM rust:1.98.0-bookworm AS builder

RUN apt-get update \
    && apt-get install --yes --no-install-recommends ca-certificates cmake clang pkg-config \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /workspace

COPY Cargo.toml Cargo.lock rust-toolchain.toml rustfmt.toml ./
COPY migrations ./migrations
COPY testing ./testing
COPY crates ./crates
COPY src ./src

RUN --mount=type=cache,target=/usr/local/cargo/registry,sharing=locked \
    --mount=type=cache,target=/usr/local/cargo/git,sharing=locked \
    --mount=type=cache,target=/workspace/target,sharing=locked \
    cargo build --locked --release --bins \
    && install -D -m 0755 target/release/remind-api /opt/remind/bin/remind-api \
    && install -D -m 0755 target/release/remind-worker /opt/remind/bin/remind-worker \
    && install -D -m 0755 target/release/remind-migrate /opt/remind/bin/remind-migrate

FROM debian:bookworm-slim AS runtime

RUN apt-get update \
    && apt-get install --yes --no-install-recommends busybox ca-certificates \
    && rm -rf /var/lib/apt/lists/* \
    && groupadd --gid 10001 remind \
    && useradd --uid 10001 --gid remind --no-create-home \
        --home-dir /nonexistent --shell /usr/sbin/nologin remind

COPY --from=builder /opt/remind/bin/ /usr/local/bin/

USER 10001:10001
WORKDIR /app

ENV RUST_BACKTRACE=0

EXPOSE 8080 9090
STOPSIGNAL SIGTERM

ENTRYPOINT ["/usr/local/bin/remind-api"]
