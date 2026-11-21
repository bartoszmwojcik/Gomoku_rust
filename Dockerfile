# Built in CI and pushed to a registry: the Mikrus VPS only pulls the image (compiling Rust there OOMs).
FROM rust:1.100-bookworm AS build
WORKDIR /src
COPY Cargo.toml Cargo.lock rust-toolchain.toml rustfmt.toml ./
COPY crates crates
RUN cargo build --release --locked -p gomoku-web

FROM debian:bookworm-slim
RUN apt-get update \
 && apt-get install -y --no-install-recommends ca-certificates curl \
 && rm -rf /var/lib/apt/lists/* \
 && useradd --system --uid 10001 gomoku
WORKDIR /app
COPY --from=build /src/target/release/gomoku-web /app/gomoku-web
COPY assets /app/assets
ENV BIND_ADDR=0.0.0.0:3000 ASSETS_DIR=/app/assets
USER gomoku
EXPOSE 3000
HEALTHCHECK --interval=30s --timeout=5s --retries=3 CMD curl -fsS http://127.0.0.1:3000/api/health || exit 1
CMD ["/app/gomoku-web"]
