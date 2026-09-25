# syntax=docker/dockerfile:1

# ---- build ----
FROM rust:1-slim-bookworm AS build
WORKDIR /app
# aws-lc-rs (reqwest's rustls backend) needs cmake and a C toolchain.
RUN apt-get update \
    && apt-get install -y --no-install-recommends cmake clang pkg-config \
    && rm -rf /var/lib/apt/lists/*
# Everything `cargo build` needs: `sqlx::migrate!` embeds ./migrations and
# reads ./sqlx.toml at compile time.
COPY Cargo.toml Cargo.lock sqlx.toml ./
COPY migrations ./migrations
COPY src ./src
# Tests reference fixtures via include_bytes! only under #[cfg(test)], so
# they are not needed for a release build.
RUN cargo build --release --locked --bins \
    && strip target/release/thaleia-api target/release/thaleia-ingest

# ---- runtime ----
FROM debian:bookworm-slim AS runtime
# ca-certificates: rustls uses the platform verifier (system trust store).
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/* \
    && useradd --system --uid 10001 --no-create-home thaleia
COPY --from=build /app/target/release/thaleia-api /usr/local/bin/thaleia-api
COPY --from=build /app/target/release/thaleia-ingest /usr/local/bin/thaleia-ingest
USER thaleia
ENV RUST_LOG=info PORT=8080
EXPOSE 8080
# The API is the default; the Railway cron service overrides the start
# command with `thaleia-ingest`.
CMD ["thaleia-api"]
