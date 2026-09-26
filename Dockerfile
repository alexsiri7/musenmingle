# syntax=docker/dockerfile:1

# ---- build ----
FROM rust:1-slim-bookworm AS build
WORKDIR /app
# aws-lc-rs (reqwest's rustls backend) needs cmake and a C toolchain.
RUN apt-get update \
    && apt-get install -y --no-install-recommends cmake clang pkg-config \
    && rm -rf /var/lib/apt/lists/*
# Everything `cargo build` needs: `sqlx::migrate!` embeds ./migrations and
# reads ./sqlx.toml at compile time; src/suggestions.rs embeds the new-scraper
# issue template.
COPY Cargo.toml Cargo.lock sqlx.toml ./
COPY migrations ./migrations
COPY .github/ISSUE_TEMPLATE/new-scraper.md ./.github/ISSUE_TEMPLATE/new-scraper.md
COPY src ./src
# Tests reference fixtures via include_bytes! only under #[cfg(test)], so
# they are not needed for a release build.
RUN cargo build --release --locked --bins \
    && strip target/release/musenmingle-api target/release/musenmingle-ingest

# ---- runtime ----
FROM debian:bookworm-slim AS runtime
# ca-certificates: rustls uses the platform verifier (system trust store).
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/* \
    && useradd --system --uid 10001 --no-create-home musenmingle
COPY --from=build /app/target/release/musenmingle-api /usr/local/bin/musenmingle-api
COPY --from=build /app/target/release/musenmingle-ingest /usr/local/bin/musenmingle-ingest
# Transitional aliases for the pre-rename binary names (the project was called
# Thaleia), so a deploy whose Railway start command still says
# `thaleia-api`/`thaleia-ingest` keeps working. Remove once both services'
# start commands are `musenmingle-*`.
RUN ln -s musenmingle-api /usr/local/bin/thaleia-api \
    && ln -s musenmingle-ingest /usr/local/bin/thaleia-ingest
USER musenmingle
ENV RUST_LOG=info PORT=8080
EXPOSE 8080
# The API is the default; the Railway cron service overrides the start
# command with `musenmingle-ingest`.
CMD ["musenmingle-api"]
