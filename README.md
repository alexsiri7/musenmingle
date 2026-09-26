# Thaleia

London cultural events for creative people: exhibitions, expos, talks,
workshops and community events (CreativeMornings, writing groups, ...).

This repository is the **ingestion backend**, the **read API** and a
minimal server-rendered **web page** (https://thaleia.interstellarai.net),
written in Rust (axum, tokio, sqlx, reqwest, maud).

## Architecture

```
            +-------------------+        +---------------------------+
cron ─────▶ |  thaleia-ingest   |        |       thaleia-api         |
(15 min)    |  (one-shot run)   |        |  GET /healthz,            |
            +---------+---------+        |  GET /v1/events[/{id}],   |
                      │                  |  GET /v1/sources,         |
                      │                  |  POST /v1/suggestions     |
                      │                  |  HTML: /, /events/{id},   |
                      │                  |  /sources, POST /suggest  |
                      │                  +-------------+-------------+
       ┌──────────────┼──────────────┐                 │
       ▼              ▼              ▼                 ▼
  Source trait   normalise     health checker ──▶ GitHub issues
  (API/scraper)  + dedupe key  (3 rules)          (scraper-broken,
                                                   new-scraper)
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
  - `somerset-house` — listing-only scraper that reads the page's embedded
    `script#props` JSON (the site has no JSON-LD), paginated, no detail pages.
  - `design-museum` — CSS-selector scraper (the site has no JSON-LD): the
    current and future exhibition listings → detail pages for the date range.
  - `whitechapel-gallery` — CSS-selector scraper (no JSON-LD): exhibitions
    listing → detail pages for dates and free-entry status.
  - `barbican` — CSS-selector scraper (no JSON-LD): the art & design and
    talks & events listings (paginated) → detail pages for times, art-form
    tags (category) and the standard ticket price.
  - `artrabbit` — aggregator (kind `aggregator`) of current London art shows,
    listing pages only (paginated, capped, one request per 5 s): facts + link
    only, per its terms (no descriptions, images or raw payloads).
- **FetchContext** (`src/fetch.rs`): the only way sources reach the network.
  Sends `ThaleiaBot/<version> (+https://github.com/alexsiri7/thaleia; contact via repo issues)`,
  fetches and caches robots.txt per origin (RFC 9309 semantics, Crawl-delay
  honoured) and rate-limits per domain (default 1 request / 2 s; built-in
  floors in `config::BUILTIN_MIN_INTERVALS`, e.g. 5 s for ArtRabbit, can't be
  lowered by configuration).
- **Normalisation** (`src/normalise.rs`): HTML/whitespace cleanup,
  Europe/London → UTC, price parsing (free detection), category mapping and
  the cross-source **dedupe key** (`title|London date|venue`, algorithm
  documented in the module).
- **Repository** (`src/repo.rs`): `upsert_event` inserts or merges events by
  exact dedupe key, then by fuzzy title and venue matching (`src/matching.rs`),
  and links each source in `events.event_sources`, so one event can carry
  several source links (e.g. Ticketmaster + the venue's own site). See
  [Merging and overrides](#merging-and-overrides).
- **Ingest runner** (`src/runner.rs`): takes an advisory lock, runs enabled
  sources whose `interval_minutes` has elapsed, each with a timeout, upserts,
  records `events.source_runs`, then runs the health checker (a source that
  cannot be built, e.g. missing credentials, is instead recorded as skipped
  on `events.sources` and retried next tick). Every tick it then applies the
  content policy to stored rows, runs the thumbnailer, and finally files
  issues for site suggestions the API left `pending`.
- **Content policy** (see [Content policy](#content-policy)): per-source
  `display_name`, `store_description`, `store_image` and `policy_note` on
  `events.sources`; description excerpts; self-hosted, credited thumbnails
  (`src/thumbs.rs`, `events.thumbnails`, served at `/thumbs/...`).
- **Health checks** (`src/health.rs`, `src/github.rs`): after each run a source
  trips if (1) a successful run found 0 events while its trailing average is > 0, (2) it had
  errors on 2 consecutive runs, or (3) its count dropped > 60 % vs the trailing
  average. A trip opens **one** GitHub issue `Scraper broken: <key>` labelled
  `scraper-broken` (deduped via `events.health_issues` *and* a lookup of open
  issues by label + title, so it survives DB resets). Recovery comments on and
  closes the issue.
- **Site suggestions** (`src/suggestions.rs`, `POST /v1/suggestions` with
  `{"url": "...", "note": "optional, ≤ 500 chars"}`): http(s) URLs on a
  public domain (no IPs, `localhost`, `.local`, …) are reduced to their
  registrable domain (public suffix list; `www.example.org/x` →
  `example.org`) and deduped against `events.sources.domain` (409
  `already_covered`) and pending/accepted suggestions (200
  `already_suggested`). New domains get 201 `accepted` and one GitHub issue
  labelled `new-scraper` from `.github/ISSUE_TEMPLATE/new-scraper.md`; if
  GitHub fails the row stays `pending` and the next ingest run files it
  (adopting an open issue with the same title). Invalid input gets 400
  `invalid`. Each client IP (stored only as `sha256(ip + salt)`) may make 5
  stored submissions per hour and 20 per day, duplicates included; beyond
  that, 429 with `Retry-After`. The URL is never fetched.
- **Read API** (`src/api.rs`, `src/listing.rs`; documented in
  [`docs/api.md`](docs/api.md)): `GET /v1/events` filters by London date
  window (events with an end date match on range overlap), category, free,
  and `near`+`radius_km` (bounding-box prefilter on the lat/lng index, then
  haversine, nearest first), with keyset cursor pagination and every event's
  source links; `GET /v1/events/{id}`; `GET /v1/sources` with the last run
  and `pending`/`unconfigured`/`healthy`/`degraded`/`broken` status (skips
  recorded by ingest). Read-only; browser access is
  limited to `CORS_ORIGINS`.
- **Web pages** (`src/web.rs`, `src/web.css`): server-rendered HTML from the
  same process, for people rather than programs. `GET /` lists upcoming
  events (from today, London) with a plain GET filter form — dates,
  category, free only, and a preset "near" area (Central & South Bank,
  East, King's Cross, South Kensington) mapped to `near`+`radius_km` — so
  every filtered view is a shareable URL; results are cards with a "More"
  link on the same cursor as the API. `GET /events/{id}` shows every field,
  the description as escaped paragraphs, each source link and an
  OpenStreetMap link. `GET /sources` is the `/v1/sources` data as a table;
  each source links to `/?source=<key>` (only its events, with a clearable
  "From: <source>" chip), and "Sites we couldn't use" lists
  `events.refused_sources` (why a venue is missing; suggestions for those
  domains get the reason instead of a new issue).
  The footer's "Suggest a venue site" form posts to `POST /suggest`, which
  runs the same validation, dedupe, rate limit and issue filing as
  `POST /v1/suggestions`. Templates are [maud](https://maud.lambda.xyz)
  (compile-time checked, HTML-escaped by default); no third-party scripts or
  assets (images are our own credited thumbnails, never the sources' URLs),
  and a strict Content-Security-Policy (`default-src 'self'`, `script-src
  'self'`, `img-src 'self'`, no `unsafe-inline`: the CSS is served from
  `/static/style.css`). Sources are shown by `display_name`. The main button
  on every card and detail page is the source's own page ("See it on
  Barbican →"); our detail page is the secondary link.
  **Saved events** (`/saved`, `src/web.js` served as `/static/app.js?v=<hash>`):
  the only script, and progressive enhancement — without it every page
  works and the save buttons stay `hidden`. Saves live only in the
  visitor's browser (`localStorage` key `letsart.saved.v1`: id, saved time
  and a title/venue/dates snapshot; no accounts, cookies or server
  storage). `/saved` is a server-rendered shell whose script fetches the
  saved events with `GET /v1/events?ids=…`, renders them from a `<template>`,
  marks vanished ones "No longer listed", and can export them as `.ics`.

### Content policy

The site is a free, for-fun aggregator: we don't want to use venues'
resources or take their traffic, so we keep facts, a short excerpt and a
small credited thumbnail, and send people to the venue.

- **Per-source policy** (`events.sources`, set by migration): `display_name`
  (shown on the pages and used in image credits), `store_description`,
  `store_image` (both default true; false when the site's terms restrict
  reuse — then we keep facts + link only) and `policy_note` (why, with the
  terms URL and date). `repo::upsert_event` enforces the flags for every
  source, and the ingest runner's `repo::enforce_content_policy` clears data
  already stored if a flag is turned off. Currently Ticketmaster (API terms)
  and Serpentine Galleries (site terms) are facts + link only.
- **Excerpts**: every stored description is cut to ≤ 300 characters at a
  sentence (else word) boundary with an ellipsis (`normalise::excerpt`);
  pages say "An excerpt. Read more on <venue>".
- **Thumbnails, never hotlinks** (`src/thumbs.rs`): after the sources of a
  tick, the runner fetches each new source image ONCE through
  `FetchContext` (robots.txt, User-Agent, rate limit), skips images over
  8 MB, shrinks it to fit 480×480 as a JPEG (quality 70, lowered until
  < 40 KB), and stores it in `events.thumbnails`. It re-fetches only when
  the event's `image_url` changes (failures are retried after 7 days), at
  most 60 per run and 20 per image host. `events.events.image_source_id`
  records which source the image came from, for the credit "Image:
  <display_name>" that links to the event's page on that source (not to the
  image file). Pages and JSON never expose the source's image URL.
- **Links** to venues use `rel="noopener"` without `noreferrer`, so venues
  can see (as our origin only, under `Referrer-Policy:
  strict-origin-when-cross-origin`) that visitors came from us.

### Merging and overrides

A listing whose dedupe key matches no event is compared with existing events
of nearby dates (`src/matching.rs`). It joins one when the dates agree (same
London day, or overlapping ranges when both run over several days), the venue
agrees (same or contained normalised name, or coordinates within 150 m) and
the titles agree (after dropping stop words, "exhibition"/"tickets"/"london",
years and venue words: token Jaccard ≥ 0.8 or bigram Sørensen–Dice ≥ 0.9).
Fuzzy matching never joins two distinct listings of the same source. Every
fuzzy merge is logged at `info` ("fuzzy merge") with both titles and scores.

When a second source joins an event, fields are merged by source kind:

| Field | Winner |
| --- | --- |
| title | first source (merges never change it) |
| dates, description, image | venue site (`scraper`) |
| price, URL | `api` (price only when it reports one) |
| everything else | existing value; newcomer fills gaps; tags unioned |

A kind only wins while it is the sole linked source of that kind; otherwise
it just fills gaps. An `aggregator` (a third-party listing site such as
ArtRabbit) never wins: it only fills gaps, and pages link to the venue's own
site (then an API) before it.

Bad (or missed) merges are corrected in `events.merge_overrides`, keyed on
the source listings `(source_id, source_event_id)`:

```sql
INSERT INTO events.merge_overrides
    (action, source_id_a, source_event_id_a, source_id_b, source_event_id_b, note)
VALUES ('never_merge', 1, 'Z698xZG2Z17aTalks', 2, 'some-slug', 'different talks');
```

`action` is `never_merge` (keep apart, even on an identical dedupe key) or
`force_merge` (join even though they do not match). The pair must be in
canonical order, `(source_id_a, source_event_id_a) < (source_id_b,
source_event_id_b)` (enforced by a CHECK). An override takes effect the next
time either listing is ingested.

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
| `GITHUB_TOKEN` | both | unset → trips only logged; suggestions stay pending | Issues read/write on `GITHUB_REPO` |
| `GITHUB_REPO` | both | `alexsiri7/thaleia` | Where health and new-scraper issues are filed |
| `SUGGESTION_IP_SALT` | api | — (required) | Secret salt for hashing submitter IPs |
| `SUGGESTION_RATE_PER_HOUR` | api | `5` | Stored suggestions per client IP per hour |
| `SUGGESTION_RATE_PER_DAY` | api | `20` | Stored suggestions per client IP per day |
| `TRUSTED_PROXY_COUNT` | api | `0` | Proxies whose `X-Forwarded-For` entries are trusted (Railway: `1`) |
| `PORT` | api | `8080` | HTTP port |
| `CORS_ORIGINS` | api | unset → no cross-origin access | Comma-separated browser origins allowed to call the API |
| `RUST_LOG` | both | `info` | tracing filter |
| `RATE_LIMIT_MS` | ingest | `2000` | Min ms between requests to one host |
| `RATE_LIMIT_OVERRIDES` | ingest | — | `host=ms,host=ms` per-host overrides |
| `SOURCE_TIMEOUT_SECS` | ingest | `300` | Per-source fetch timeout |
| `TEST_DATABASE_URL` | tests | unset → DB tests skip | Throwaway Postgres for tests |

See `.env.example`.

## Deployment (Railway)

Railway project `thaleia`, environment `production`, region
`europe-west4-drams3a` (EU West), runs two services. Both deploy from
`alexsiri7/thaleia` `main`, are built from the same `Dockerfile` (multi-stage;
the runtime image contains both binaries), and are declared in
`.railway/railway.ts`:

1. **thaleia-api** — start command `thaleia-api`, health check `GET /healthz`
   (30 s timeout), restart on failure (max 5 retries).
2. **thaleia-ingest** — cron job `*/15 * * * *` (every 15 minutes), start
   command `thaleia-ingest`, no healthcheck, never restarted. The process
   exits when done, as Railway cron requires. Per-source `interval_minutes` in
   `events.sources` decides what actually runs on each tick (Ticketmaster
   every 6 h, Serpentine, Somerset House, the Design Museum, Whitechapel
   Gallery and the Barbican daily), and an advisory lock prevents overlapping
   runs.

See the environment variable table above (`Used by` column) for the full
per-service list and defaults.

**Infrastructure as Code.** Railway retired `railway.toml` config-as-code
(cutoff 2026-12-01); service settings now live in `.railway/railway.ts`, which
Railway does **not** read on deploy. To change a setting, edit the file, then
run the steps below with the global `railway` CLI at 5.42.1 or newer (check
`railway --version`; upgrade it if needed). `npm ci` installs only the
TypeScript SDK the file imports, not the CLI:

```bash
cd .railway && npm ci
railway link            # project thaleia, environment production
railway config plan     # review the diff
railway config apply
```

Variables are declared with `preserve()`, so their values are managed in the
dashboard and never committed; when you add a variable in the dashboard, add
its `preserve()` line too. `GITHUB_TOKEN`, `TICKETMASTER_API_KEY` and
`CORS_ORIGINS` are not set on Railway yet — add them to `railway.ts` when they
are. Before the first `apply`, check that `plan` shows no unintended changes;
if it reports a service as still managed by a config file, clear that
service's config-as-code path in the dashboard first.

Database: run `ops/sql/create-role.sql` once as the Supabase owner, set the
role's password, and use a **session**-mode (or direct) connection string for
`DATABASE_URL` — the transaction pooler does not support the startup
`search_path` option or prepared statements reliably.

## Roadmap

- **More sources** — CreativeMornings London, galleries/museums, writing groups;
  each via a `new-scraper` issue (`docs/adding-a-scraper.md`).
