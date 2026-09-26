# CLAUDE.md — guidance for coding agents

Thaleia: London cultural events ingestion backend + read API. Rust,
single crate `thaleia` (lib) with binaries `thaleia-api` and `thaleia-ingest`.
Read `README.md` for the architecture.

## Commands

```bash
cargo fmt --all                                              # CI runs --check
cargo clippy --all-targets --all-features -- -D warnings     # must be clean
cargo test                                                   # DB tests skip without TEST_DATABASE_URL
TEST_DATABASE_URL=postgres://postgres@localhost:5432/postgres cargo test   # full suite
INSTA_UPDATE=always cargo test --test <name>                 # rewrite snapshots, then REVIEW them
```

CI (`.github/workflows/ci.yml`) runs fmt, clippy and tests against a
`postgres:17` service with `THALEIA_REQUIRE_DB=1`, plus a `docker build`.
There is no local Docker; never try to use testcontainers.

## Invariants — do not break these

1. **Never touch schemas other than `events`.** Every SQL object and query is
   schema-qualified `events.`; the migration table is `events._sqlx_migrations`
   (`sqlx.toml` + `src/db.rs`). No extensions, no objects in `public`, no
   `CREATE SCHEMA` in migrations. `tests/schema_isolation.rs` must keep
   passing. Do not use `sqlx migrate run` with a different config, and never
   run anything against the production database.
2. **Migrations are append-only.** Never edit or delete a file in
   `migrations/` once merged; add a new timestamped file instead.
3. **No LLM parsing.** All extraction is deterministic code (JSON-LD first,
   then CSS selectors).
4. **Every scraper needs a saved fixture + an `insta` snapshot test** of its
   normalised output (see `docs/adding-a-scraper.md`), and a wiremock fetch
   test. Tests never hit the real network.
5. **robots.txt, the ThaleiaBot User-Agent and the per-domain rate limit are
   enforced by `FetchContext` and must not be bypassed.** Sources get network
   access only through `FetchContext`; do not create a `reqwest::Client` in a
   source or expose FetchContext's client. (The GitHub issue filer in
   `src/github.rs` is an authenticated API client, not a source, and is the
   only other HTTP client.)
6. Runtime-checked sqlx queries only (`sqlx::query*` + `AssertSqlSafe` for
   constant-built strings); no `query!` macros — builds must not need a DB.
7. Secrets (API keys, tokens) are never logged; FetchContext redacts query
   strings in logs and errors.
8. Skips are not errors: out-of-scope items return `Ok(None)` from
   `normalise`; only real failures count toward `source_runs.errors` (which
   drive the health checker).
9. **HTML is server-rendered, escaped and self-contained.** Pages in
   `src/web.rs` are `maud` templates: never wrap data in `PreEscaped`;
   emit `href`/`src` only via `safe_link`/`safe_image` (http(s) only). No
   JavaScript, no third-party assets (CDNs, fonts, embeds), no inline
   `style=` or `<style>` (CSS lives in `src/web.css`, served from
   `/static/style.css`); keep the `CSP` constant strict. Pages read data
   through the same `api.rs` helpers as the JSON API, never over HTTP.

## Layout

- `src/fetch.rs` — FetchContext (UA, robots, rate limit)
- `src/normalise.rs` — text/time/price/category helpers, dedupe key
- `src/matching.rs` — fuzzy cross-source match rules (pure)
- `src/sources/` — `Source` trait, `jsonld` helpers, `ticketmaster`, `serpentine`
- `src/repo.rs` — all SQL (upsert/merge, runs, health issues)
- `src/runner.rs` — ingest run; `src/health.rs` rules + issue lifecycle; `src/github.rs` REST filer
- `src/suggestions.rs` — site-suggestion validation, domain dedupe, IP rate limit, new-scraper issues
- `src/api.rs` — axum router (`/healthz`, read API, `POST /v1/suggestions`, CORS); `docs/api.md` documents it
- `src/listing.rs` — `GET /v1/events` parameter parsing and cursors (pure)
- `src/web.rs` + `src/web.css` — HTML pages (`/`, `/events/{id}`, `/sources`, `POST /suggest`)
- `ops/sql/create-role.sql` — one-off role/grants script for the DB owner
- `tests/` — integration tests (`common/` helpers), `fixtures/`, `snapshots/`
