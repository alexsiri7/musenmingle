# Muse & Mingle

London cultural events for creative people: exhibitions, expos, talks,
workshops and community events (CreativeMornings, writing groups, ...).

This repository is the **ingestion backend**, the **read API** and a
minimal server-rendered **web page** (https://musenmingle.interstellarai.net),
written in Rust (axum, tokio, sqlx, reqwest, maud).

The project was called **Thaleia** until 2026-09-26. The old host
`thaleia.interstellarai.net` stays attached and redirects to the new one
(`CANONICAL_HOST`, see `src/host_redirect.rs`), and the Saved-events
script moves the old `letsart.saved.v1` key to the new one.

## Architecture

```
            +-------------------+        +---------------------------+
cron ─────▶ |  musenmingle-ingest   |        |       musenmingle-api         |
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
  - `garden-museum` — reads the what's-on listing's embedded `script#json-data`
    (no JSON-LD) → event pages for times, location, booking prices and
    description; exhibitions, talks, workshops, festivals and lates.
  - `goldsmiths-cca` — CSS-selector scraper (no JSON-LD): the homepage's
    exhibition list (title, dates, image) → exhibition pages for the
    subtitle and description.
  - `mall-galleries` — CSS-selector scraper (no JSON-LD): the homepage's
    server-rendered current + upcoming exhibition teasers (the
    `/exhibitions-events` list is a JavaScript app) → detail pages for
    dates, admission price and description.
  - `whitechapel-gallery` — CSS-selector scraper (no JSON-LD): exhibitions
    listing → detail pages for dates and free-entry status.
  - `courtauld` — the site's listing is rendered by JavaScript, so it reads
    the same GET JSON endpoints (the programme id list + WordPress REST
    `events` for links and taxonomy classes) → server-rendered detail pages
    (CSS selectors) for dates, times and price; gallery exhibitions and
    public talks at Vernon Square, online items and courses skipped.
  - `barbican` — CSS-selector scraper (no JSON-LD): the art & design and
    talks & events listings (paginated) → detail pages for times, art-form
    tags (category) and the standard ticket price.
  - `clerkenwell-design-week` — the annual design festival: one homepage
    request for its schema.org JSON-LD `Event` (category expo).
  - `chisenhale-gallery` — listing-only CSS scraper (no JSON-LD; robots.txt
    Crawl-delay 20, so no detail pages): year-less card dates resolved
    against the run date.
  - `dandad` — D&AD's creative-industry events: reads the embedded
    `script#props` page data (no JSON-LD) of the events listing → detail
    pages for time and address; London only; facts + link only (terms).
  - `soane-museum` — CSS-selector scraper (no JSON-LD): the paginated
    "What's on" listing (dates, type label, price line), plus detail pages
    of talks and events for their location (some are off-site).
- **FetchContext** (`src/fetch.rs`): the only way sources reach the network.
  Sends `MuseNMingleBot/<version> (+https://musenmingle.interstellarai.net/about#for-venues)`,
  fetches and caches robots.txt per origin (RFC 9309 semantics, Crawl-delay
  honoured) and rate-limits per domain (default 1 request / 2 s; built-in
  floors in `config::BUILTIN_MIN_INTERVALS` can't be lowered by
  configuration).
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
  content policy to stored rows, runs the thumbnailer, keeps AI fields in
  step with the stored facts, runs the [AI enrichment](#ai-enrichment)
  pass (when `REQUESTY_API_KEY` is set), and finally files issues for site
  suggestions the API left `pending`.
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
  OpenStreetMap link. `GET /about` ("About & our approach", linked from
  the header and footer) states the site's objective and how it treats
  venues, their content and visitors' data (anchors `#objective`,
  `#how-we-collect`, `#ai`, `#for-venues`, `#your-data`, `#contact`); keep it true
  when crawler or content-policy behaviour changes. Venues use the `/contact`
  form (`src/contact.rs`): requests are stored in `events.contact_requests`
  and filed as `venue-request` GitHub issues with the server's token (the
  optional reply email stays in the database), with a honeypot, a signed
  minimum fill time and a per-IP rate limit against spam; see
  [docs/venue-requests.md](docs/venue-requests.md). Public pages never link
  into the (private) GitHub repository.
  `GET /sources` is the `/v1/sources` data as a table;
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
  visitor's browser (`localStorage` key `musenmingle.saved.v1`: id, saved time
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
  already stored if a flag is turned off. Currently Ticketmaster (API terms),
  Serpentine Galleries (site terms) and Sir John Soane's Museum (site terms)
  are facts + link only.
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

### AI enrichment

Scrapers never use a language model; extraction stays deterministic. After
the sources of a tick, `src/enrich/` adds tags and a short note to each
upcoming event, **only from what we already store** (`enrich::input`:
title, venue, London dates, category, the listing's own tags, price, the
≤ 300-character excerpt when the source's terms let us keep one, and the
sources' names). It never fetches a page, and facts-only sources (Ticketmaster,
Serpentine, D&AD) send just the facts.

- **Provider:** Requesty's OpenAI-compatible
  `POST https://router.requesty.ai/v1/chat/completions`, model
  `ENRICH_MODEL` (default `anthropic/claude-opus-5-5`, 0-day retention).
  The model must have an `events.model_prices` row with
  `retention_days = 0` (the `/about#ai` promise); otherwise the pass does
  nothing. Prices in that table (catalogue, per million tokens) turn each
  response's `usage` into dollars.
- **Output** (`src/enrich/prompt.txt`, `enrich::output`): per event
  `medium_tags` (≤ 3 of 16), `format_tags` (≤ 3 of 10), `good_for` (≤ 3 of 6),
  `vibe_tags` (≤ 2 of 8), `artists` (≤ 6, verbatim from the input),
  `is_opening` + `opening_evidence` (verbatim), `grounding`
  (`listing` / `listing_plus_general_knowledge` / `insufficient`),
  `whats_cool` (≤ 220 characters, British English, no hype words; null when
  insufficient), `one_liner` (≤ 90) and `confidence`. The answer is requested
  as JSON (`ENRICH_OUTPUT_MODE`, default `json_object` with low reasoning:
  strict `json_schema` returned degenerate placeholders from Opus 5.5 in 4 of
  10 test calls) and validated strictly in Rust; invalid results are retried
  once with the validator's complaints, then given up (logged, stored in
  `events.enrichment_failures`, no partial writes, not re-sent until the
  input or `PROMPT_VERSION` changes).
- **Storage:** `events.enrichments` (event, model, prompt version, input
  hash, output JSON, tokens, cost) and materialised onto `events.events`
  (`medium_tags`, `format_tags`, `good_for`, `vibe_tags`, `is_opening`,
  `whats_cool`, `one_liner`, `ai_grounding`, `ai_model`, `ai_enriched_at`)
  for filters and pages. Events are re-enriched only when their input hash
  or the prompt version changes; when the input changes the materialised AI
  fields are cleared at once (`enrich::sync`, every tick) so a note never
  outlives its facts. `medium_tags` also carries the sources'
  `default_medium_tags` (e.g. Design Museum → `design`), which is all an
  event has while AI is off.
- **Cost control:** batches of `ENRICH_BATCH_SIZE` (10) behind one long
  static system prompt, with Requesty `auto_cache` when a pass makes several
  calls; every call (failed ones too) is recorded in
  `events.enrichment_calls`; a call is made only if its pessimistic estimate
  fits under `ENRICH_DAILY_CAP_USD` (1.00, per London day, embeddings
  included) and `ENRICH_RUN_CAP_USD` (0.40); `ENRICH_RUN_BUDGET_SECS` (300)
  bounds the pass's time inside the ingest lock. Each pass logs events
  enriched, tokens and dollars. Measured 2026-09-26: ~$0.005 per event on
  Opus 5.5 (see the evaluation notes in the PR).
- **Credits:** a 402 from Requesty (or a 403/429 whose message is about
  credits, balance, quota or spend limits) stops the pass for that run and
  is recorded in `events.alert_state`; the owner gets one high-priority
  ntfy (`NTFY_TOPIC`) per London day while it lasts, and one "OK again"
  message when a later call succeeds.
- **Embeddings** (`enrich::embed`): after enrichment, each upcoming event with
  a current enrichment (or a given-up one: then facts only, re-embedded once
  enriched) gets an `EMBED_MODEL` (default `openai/text-embedding-3-small`,
  1536 dimensions) vector of a documented text template (`EMBED_VERSION`),
  in batches of 100, re-embedded when the text hash changes. They live in
  `events.event_embeddings` (pgvector `extensions.vector(1536)`, HNSW
  cosine index), which exists only when the role may use the shared
  database's `extensions` schema (`GRANT USAGE ON SCHEMA extensions TO
  musenmingle`, in `ops/sql/create-role.sql`); the ingest re-applies that
  idempotent migration on start, so embeddings switch on after the grant.
  Used for "More like this" on event pages (`GET /v1/events/{id}/similar`)
  and by `repo::semantic_candidates` for future hybrid search.
- **Pages:** cards show the `one_liner` (muted); event pages show "What's
  cool" labelled "✨ AI note" (linking to `/about#ai`), medium/format chips
  and "More like this"; the listing filters by medium, format and "good for"
  with live counts (`GET /v1/events?medium=…&format=…&good_for=…&facets=true`).
- **Evaluating prompts/models:** `cargo run --example enrich_eval -- events.json
  <model> <in> <out> <cache-read> <cache-write>` runs the production request
  builder and validator on exported events without a database.

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
it just fills gaps. An `aggregator` (a third-party listing site; none at
present since ArtRabbit was retired, #96) never wins: it only fills gaps, and pages link to the venue's own
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

Muse & Mingle will run inside an existing, shared production Postgres (Supabase).
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
`musenmingle` login role with USAGE + CREATE on `events` only (plus default
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
they print `SKIPPING ...` and pass. CI runs them against a
`pgvector/pgvector:pg17` service with `MUSENMINGLE_REQUIRE_DB=1`, which turns a
missing URL into a failure. When the server has pgvector, every test database
gets it in schema `extensions` (as in production), so the embedding tests
run; without it they print `SKIPPING` for those parts.

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
cargo run --bin musenmingle-ingest   # one ingestion pass
cargo run --bin musenmingle-api      # http://localhost:8080/healthz
```

Both binaries apply pending migrations on start (sqlx takes a migration lock).

### Environment variables

| Variable | Used by | Default | Purpose |
|---|---|---|---|
| `DATABASE_URL` | both | — (required) | Postgres URL (the `musenmingle` role in prod) |
| `TICKETMASTER_API_KEY` | ingest | unset → source skipped | Discovery API key |
| `GITHUB_TOKEN` | both | unset → trips only logged; suggestions stay pending | Issues read/write on `GITHUB_REPO` |
| `GITHUB_REPO` | both | `alexsiri7/musenmingle` | Where health and new-scraper issues are filed |
| `SUGGESTION_IP_SALT` | api | — (required) | Secret salt for hashing submitter IPs |
| `SUGGESTION_RATE_PER_HOUR` | api | `5` | Stored suggestions per client IP per hour |
| `SUGGESTION_RATE_PER_DAY` | api | `20` | Stored suggestions per client IP per day |
| `TRUSTED_PROXY_COUNT` | api | `0` | Proxies whose `X-Forwarded-For` entries are trusted (Railway: `1`) |
| `PORT` | api | `8080` | HTTP port |
| `CANONICAL_HOST` | api | unset → no redirect | Host that requests to `LEGACY_HOSTS` are redirected to (301 for GET/HEAD, 308 otherwise; path and query kept; `/healthz` never redirects) |
| `LEGACY_HOSTS` | api | `thaleia.interstellarai.net` | Comma-separated old host names that redirect to `CANONICAL_HOST` |
| `CORS_ORIGINS` | api | unset → no cross-origin access | Comma-separated browser origins allowed to call the API |
| `RUST_LOG` | both | `info` | tracing filter |
| `RATE_LIMIT_MS` | ingest | `2000` | Min ms between requests to one host |
| `RATE_LIMIT_OVERRIDES` | ingest | — | `host=ms,host=ms` per-host overrides |
| `SOURCE_TIMEOUT_SECS` | ingest | `300` | Per-source fetch timeout |
| `REQUESTY_API_KEY` | ingest | unset → AI enrichment and embeddings off | Requesty key ([AI enrichment](#ai-enrichment)) |
| `ENRICH_MODEL` | ingest | `anthropic/claude-opus-5-5` | Chat model (needs a zero-retention `events.model_prices` row) |
| `ENRICH_DAILY_CAP_USD` | ingest | `1.00` | Max Requesty spend per London day (enrichment + embeddings) |
| `ENRICH_RUN_CAP_USD` | ingest | `0.40` | Max spend per ingest run |
| `ENRICH_BATCH_SIZE` | ingest | `10` | Events per chat call (1–25) |
| `ENRICH_MAX_EVENTS_PER_RUN` | ingest | `120` | Events queued per run |
| `ENRICH_RUN_BUDGET_SECS` | ingest | `300` | Wall-clock budget of the pass |
| `ENRICH_OUTPUT_MODE` | ingest | `json_object` | Or `json_schema` (strict) |
| `ENRICH_REASONING_EFFORT` | ingest | `low` | `reasoning_effort` sent to the model (`default` = omit) |
| `EMBED_MODEL` | ingest | `openai/text-embedding-3-small` | Embedding model (`off` = none; needs a `model_prices` row) |
| `REQUESTY_BASE_URL` | ingest | `https://router.requesty.ai` | Tests point it at a mock |
| `NTFY_TOPIC` | ingest | unset → alerts logged | ntfy topic for owner alerts (secret) |
| `NTFY_BASE_URL` | ingest | `https://ntfy.sh` | ntfy server |
| `TEST_DATABASE_URL` | tests | unset → DB tests skip | Throwaway Postgres for tests |

See `.env.example`.

## Deployment (Railway)

Railway project `musenmingle`, environment `production`, region
`europe-west4-drams3a` (EU West), runs two services. Both deploy from
`alexsiri7/musenmingle` `main`, are built from the same `Dockerfile` (multi-stage;
the runtime image contains both binaries), and are declared in
`.railway/railway.ts`:

1. **musenmingle-api** — start command `musenmingle-api`, health check `GET /healthz`
   (30 s timeout), restart on failure (max 5 retries).
2. **musenmingle-ingest** — cron job `*/15 * * * *` (every 15 minutes), start
   command `musenmingle-ingest`, no healthcheck, never restarted. The process
   exits when done, as Railway cron requires. Per-source `interval_minutes` in
   `events.sources` decides what actually runs on each tick (Ticketmaster
   every 6 h, Serpentine, Somerset House, the Design Museum, Whitechapel
   Gallery, the Barbican, Chisenhale Gallery, D&AD and Sir John Soane's Museum daily), and an advisory lock prevents overlapping
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
railway link            # project musenmingle, environment production
railway config plan     # review the diff
railway config apply
```

Variables are declared with `preserve()`, so their values are managed in the
dashboard and never committed; when you add a variable in the dashboard, add
its `preserve()` line too. `CORS_ORIGINS` is not set on Railway yet — add it to `railway.ts` when it is.
The ingest service also has `GITHUB_TOKEN`, `TICKETMASTER_API_KEY`,
`REQUESTY_API_KEY`, `ENRICH_MODEL`, `ENRICH_DAILY_CAP_USD` and `NTFY_TOPIC`
(the API needs none of the enrichment ones). Before the first `apply`, check that `plan` shows no unintended changes;
if it reports a service as still managed by a config file, clear that
service's config-as-code path in the dashboard first.

Database: run `ops/sql/create-role.sql` once as the Supabase owner, set the
role's password, and use a **session**-mode (or direct) connection string for
`DATABASE_URL` — the transaction pooler does not support the startup
`search_path` option or prepared statements reliably.

## Roadmap

- **More sources** — CreativeMornings London, galleries/museums, writing groups;
  each via a `new-scraper` issue (`docs/adding-a-scraper.md`).
