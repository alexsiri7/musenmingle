# Thaleia

London cultural events for creative people: exhibitions, expos, talks,
workshops and community events (CreativeMornings, writing groups, ...).

This repository is the **ingestion backend** and (later) the **read API**,
written in Rust (axum, tokio, sqlx, reqwest). There is no frontend yet.

## Architecture

```
            +-------------------+        +---------------------------+
cron ─────▶ |  thaleia-ingest   |        |       thaleia-api         |
(15 min)    |  (one-shot run)   |        |  GET /healthz (read API   |
            +---------+---------+        |  is a later phase)        |
                      │                  +-------------+-------------+
       ┌──────────────┼──────────────┐                 │
       ▼              ▼              ▼                 ▼
  Source trait   normalise     health checker ──▶ GitHub issues
  (API/scraper)  + dedupe key  (3 rules)          (scraper-broken)
       │              │              │
       ▼              ▼              ▼
  FetchContext   repo::upsert_event / source_runs / health_issues
  (UA, robots,           │
   rate limit)           ▼
                 Postgres, schema `events` only
```

- **Sources** (`src/sources/`): each implements the async, object-safe
  `Source` trait: `key()`, `fetch(&FetchContext) -> Vec<RawEvent>`,
  `normalise(&RawEvent) -> Option<NewEvent>`. Implemented:
  - `ticketmaster` — Discovery API v2, London (`city=London&countryCode=GB`),
    segments Arts & Theatre + Miscellaneous, filtered to our categories,
    paginated with a page cap.
  - `serpentine-galleries` — example hand-written scraper: listing page →
    detail pages → schema.org JSON-LD `Event` (CSS only for the price line).
- **FetchContext** (`src/fetch.rs`): the only way sources reach the network.
  Sends `ThaleiaBot/<version> (+https://github.com/alexsiri7/thaleia; contact via repo issues)`,
  fetches and caches robots.txt per origin (RFC 9309 semantics, Crawl-delay
  honoured) and rate-limits per domain (default 1 request / 2 s).
- **Normalisation** (`src/normalise.rs`): HTML/whitespace cleanup,
  Europe/London → UTC, price parsing (free detection), category mapping and
  the cross-source **dedupe key** (`title|London date|venue`, algorithm
  documented in the module).
- **Repository** (`src/repo.rs`): `upsert_event` inserts or merges events by
  dedupe key and links each source in `events.event_sources`, so one event can
  carry several source links (e.g. Ticketmaster + the venue's own site).
- **Ingest runner** (`src/runner.rs`): takes an advisory lock, runs enabled
  sources whose `interval_minutes` has elapsed, each with a timeout, upserts,
  records `events.source_runs`, then runs the health checker.
- **Health checks** (`src/health.rs`, `src/github.rs`): after each run a source
  trips if (1) a successful run found 0 events while its trailing average is > 0, (2) it had
  errors on 2 consecutive runs, or (3) its count dropped > 60 % vs the trailing
  average. A trip opens **one** GitHub issue `Scraper broken: <key>` labelled
  `scraper-broken` (deduped via `events.health_issues` *and* a lookup of open
  issues by label + title, so it survives DB resets). Recovery comments on and
  closes the issue.

### Schema isolation

Thaleia will run inside an existing, shared production Postgres (Supabase).
Everything it owns lives in the **`events`** schema:

- every migration object is schema-qualified `events.`;
- sqlx's bookkeeping table is `events._sqlx_migrations`, configured in
  `sqlx.toml` (sqlx 0.9 `[migrate] table-name`, read by `sqlx::migrate!` and by
  sqlx-cli) and re-asserted at runtime in `src/db.rs`;
- connections set `search_path=events`;
- the `events` schema is created by `ops/sql/create-role.sql` (or by the app if
  it is missing and the role may create it);
- `tests/schema_isolation.rs` runs the migrations on an empty database and
  diffs `pg_class`/`pg_type`/`pg_proc`/`pg_namespace`/`pg_extension` to prove
  nothing is created outside `events`, and runs the migrations **as** the
  restricted role to prove it cannot touch other schemas.

`ops/sql/create-role.sql` is run once by the database owner; it creates the
`thaleia` login role with USAGE + CREATE on `events` only (plus default
privileges) and nothing elsewhere.

## Local development

```bash
cargo fmt --all
cargo clippy --all-targets --all-features -- -D warnings
cargo test
```

There is no Docker requirement. Database integration tests read
`TEST_DATABASE_URL`, pointing at **any** Postgres (15+) where the user may
`CREATE DATABASE`; each test creates and drops its own database. Without it
they print `SKIPPING ...` and pass. CI runs them against a `postgres:17`
service with `THALEIA_REQUIRE_DB=1`, which turns a missing URL into a failure.

```bash
TEST_DATABASE_URL=postgres://postgres@localhost:5432/postgres cargo test
```

Never point `TEST_DATABASE_URL` at a shared or production database.

Snapshot tests use [`insta`](https://insta.rs): after an intentional change,
`INSTA_UPDATE=always cargo test` rewrites the `.snap` files — review the diff
before committing.

Running the binaries locally:

```bash
set -a; . ./.env; set +a
cargo run --bin thaleia-ingest   # one ingestion pass
cargo run --bin thaleia-api      # http://localhost:8080/healthz
```

Both binaries apply pending migrations on start (sqlx takes a migration lock).

### Environment variables

| Variable | Used by | Default | Purpose |
|---|---|---|---|
| `DATABASE_URL` | both | — (required) | Postgres URL (the `thaleia` role in prod) |
| `TICKETMASTER_API_KEY` | ingest | unset → source skipped | Discovery API key |
| `GITHUB_TOKEN` | ingest | unset → trips only logged | Issues read/write on `GITHUB_REPO` |
| `GITHUB_REPO` | ingest | `alexsiri7/thaleia` | Where health issues are filed |
| `PORT` | api | `8080` | HTTP port |
| `RUST_LOG` | both | `info` | tracing filter |
| `RATE_LIMIT_MS` | ingest | `2000` | Min ms between requests to one host |
| `RATE_LIMIT_OVERRIDES` | ingest | — | `host=ms,host=ms` per-host overrides |
| `SOURCE_TIMEOUT_SECS` | ingest | `300` | Per-source fetch timeout |
| `TEST_DATABASE_URL` | tests | unset → DB tests skip | Throwaway Postgres for tests |

See `.env.example`.

## Deployment (Railway)

Not deployed yet; nothing has been created on Railway. The intended setup is
two services built from the same `Dockerfile` (multi-stage; the runtime image
contains both binaries):

1. **API service** — reads `railway.toml` (the default config file):
   start command `thaleia-api`, health check `GET /healthz`, restart on
   failure. Env: `DATABASE_URL`, `RUST_LOG`.
2. **Ingest cron service** — same repo and Dockerfile; in its service
   settings set the *config-as-code file path* to `/railway.ingest.toml`
   (config in code overrides the dashboard, so it must not read
   `railway.toml`). That file sets start command `thaleia-ingest`,
   `cronSchedule = "*/15 * * * *"` (every 15 minutes), no healthcheck and
   `restartPolicyType = "NEVER"`. The process exits when done, as Railway cron
   requires. Per-source `interval_minutes` in `events.sources` decides what
   actually runs on each tick (Ticketmaster every 6 h, Serpentine daily), and
   an advisory lock prevents overlapping runs. Env: `DATABASE_URL`,
   `TICKETMASTER_API_KEY`, `GITHUB_TOKEN`, `GITHUB_REPO`, `RUST_LOG`,
   optionally `RATE_LIMIT_*`, `SOURCE_TIMEOUT_SECS`.

Database: run `ops/sql/create-role.sql` once as the Supabase owner, set the
role's password, and use a **session**-mode (or direct) connection string for
`DATABASE_URL` — the transaction pooler does not support the startup
`search_path` option or prepared statements reliably.

## Roadmap

- **Read API** — list/filter events by date range, category, free, bounding
  box (lat/lng index is in place).
- **Site submissions endpoint** — public "suggest a site" form writing to
  `events.site_suggestions` (table exists), deduped per domain, filed as
  `new-scraper` issues.
- **More sources** — CreativeMornings London, galleries/museums, writing groups;
  each via a `new-scraper` issue (`docs/adding-a-scraper.md`).
- **Railway deployment** — API + ingest cron services as described above.
