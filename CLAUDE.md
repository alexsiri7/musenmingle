# CLAUDE.md — guidance for coding agents

Muse & Mingle: London cultural events ingestion backend + read API. Rust,
single crate `musenmingle` (lib) with binaries `musenmingle-api` and `musenmingle-ingest`.
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
`pgvector/pgvector:pg17` service (pgvector, like production; `tests/common` installs it in schema `extensions` per test database) with `MUSENMINGLE_REQUIRE_DB=1`, plus a `docker build`.
There is no local Docker; never try to use testcontainers.

## Invariants — do not break these

1. **Never touch schemas other than `events`.** Every SQL object and query is
   schema-qualified `events.`; the migration table is `events._sqlx_migrations`
   (`sqlx.toml` + `src/db.rs`). No extensions, no objects in `public`, no
   `CREATE SCHEMA` in migrations. The one exception is *using* pgvector's
   type and operators from the owner's `extensions` schema
   (`extensions.vector`, `OPERATOR(extensions.<=>)`,
   `extensions.vector_cosine_ops`) for `events.event_embeddings`; that
   migration is idempotent and a no-op without the grant (see its header). `tests/schema_isolation.rs` must keep
   passing. Do not use `sqlx migrate run` with a different config, and never
   run anything against the production database.
2. **Migrations are append-only.** Never edit or delete a file in
   `migrations/` once merged; add a new timestamped file instead.
3. **No LLM parsing.** All extraction is deterministic code (JSON-LD first,
   then CSS selectors); scrapers never call a model. AI enrichment
   (`src/enrich/`) runs only AFTER ingest, only on data already stored in
   `events.events` (title, venue, dates, category, source tags, price, the
   stored excerpt, source names: `enrich::input`), never fetches pages and
   never sends visitor data. Its output is validated strictly
   (`enrich::output::validate`: fixed vocabularies, lengths, grounding,
   artists/evidence quoted from the input), always labelled as AI-written
   on pages and in JSON (`ai.label`), withdrawn when its input hash changes
   (`enrich::sync`), and never presented as the venue's words. Every call
   goes in the `events.enrichment_calls` ledger and must fit the daily/run
   caps; the chat model must have a zero-retention `events.model_prices`
   row (`/about` promises it). Changing the prompt/schema/validation means
   bumping `PROMPT_VERSION`; changing the embedding text means bumping
   `EMBED_VERSION`. The scraper QA check (`src/qa/`) is the one AI pass
   that sees page text: it only judges our extraction against pages fetched
   through `FetchContext` (the run's own, plus at most 4 detail pages), stores verdicts in `events.qa_checks`,
   never writes event data, and shares the ledger (`pass = 'qa'`) under its
   own `QA_DAILY_CAP_USD`; changing its prompt/input/validation means
   bumping `QA_PROMPT_VERSION`.
4. **Every scraper needs a saved fixture + an `insta` snapshot test** of its
   normalised output (see `docs/adding-a-scraper.md`), and a wiremock fetch
   test. Tests never hit the real network.
5. **robots.txt, the MuseNMingleBot User-Agent and the per-domain rate limit are
   enforced by `FetchContext` and must not be bypassed.** Sources get network
   access only through `FetchContext`; do not create a `reqwest::Client` in a
   source or expose FetchContext's client. (The other HTTP clients are
   authenticated API clients, not sources, and never fetch web pages: the
   GitHub issue filer in `src/github.rs`, the Requesty client in
   `src/enrich/requesty.rs` and the ntfy notifier in `src/notify.rs`.)
6. Runtime-checked sqlx queries only (`sqlx::query*` + `AssertSqlSafe` for
   constant-built strings); no `query!` macros — builds must not need a DB.
7. Secrets (API keys, tokens) are never logged; FetchContext redacts query
   strings in logs and errors.
8. Skips are not errors: out-of-scope items return `Ok(None)` from
   `normalise`; only real failures count toward `source_runs.errors` (which
   drive the health checker).
9. **HTML is server-rendered, escaped and self-contained.** Pages in
   `src/web.rs` are `maud` templates: never wrap data in `PreEscaped`;
   emit `href` only via `safe_link` (http(s) only). No
   third-party assets (CDNs, fonts, embeds, scripts, images), no inline `style=`,
   `<style>`, inline `<script>` or `on*=` handlers (CSS lives in
   `src/web.css`, the scripts in `src/web.js` (every page) and
   `src/map.mjs` (`/map` only), served from `/static/`; the map's vendored
   MapLibre/pmtiles/glyphs/styles are under `static/map/` and its tiles in
   `static/tiles/`, see `docs/map.md`);
   keep the `CSP` constant strict. JavaScript is progressive enhancement
   only: every page must work without it (JS-only controls render
   `hidden`), and it builds DOM with `textContent`, never `innerHTML`. Pages
   read data through the same `api.rs` helpers as the JSON API, never over
   HTTP. `node --test tests/js/*.test.js` tests `web.js` and `map.mjs`.
10. **Respect sources' content (content policy).** Never hotlink or expose a
    source's image URL: images reach pages and JSON only as our own
    thumbnails (`src/thumbs.rs`, served from `/thumbs/`, `img-src 'self'`),
    always with the visible credit "Image: <display_name>" linking to the
    event's page on that source. Thumbnails are made only by the
    thumbnailer, through `FetchContext`. Every source seeds `display_name`
    and decides `store_description` / `store_image` (+ `policy_note`) from
    the site's terms: default true only when the terms don't forbid it;
    sites/APIs whose terms restrict reproduction are facts + link only
    (both false). `repo::upsert_event` enforces the flags and the 300-char
    excerpt (`normalise::excerpt`); don't bypass it, and don't change
    `clean_description`/sources to do it. Venue links use `rel="noopener"`
    (not `noreferrer`). On the event detail page (and the map popover) the
    primary call to action is the source's page ("See it on <venue> →");
    event cards lead to our detail page and keep a "See it on" link.
11. **`/about` must stay true.** It makes public promises (robots.txt, the
    MuseNMingleBot UA linking to `/about#for-venues`, 2 s default rate limit,
    excerpts, credited thumbnails, no cookies, removal within 7 days via
    `docs/venue-requests.md`, the `/contact` form, and `#ai`: what the AI
    sees, the "✨ AI note" label, zero-retention chat model, the embedding
    model's retention, no visitor data). If you change any of that
    behaviour, update the page (`web::about`) in the same PR. Public HTML
    never links into the private GitHub repo (`github.com/alexsiri7/…`;
    tests assert it), and a contact request's reply email never goes to
    GitHub (only the `events.contact_requests` row id does).
12. **Sites we decided not to scrape go in `events.refused_sources`** (via a
    new migration, with reason and issue link) when a `new-scraper` issue is
    closed as not possible; see `docs/adding-a-scraper.md`. Never retry them.

## Layout

- `src/fetch.rs` — FetchContext (UA, robots, rate limit)
- `src/normalise.rs` — text/time/price/category helpers, dedupe key
- `src/hours.rs` — weekly opening hours (pure): parsed from a listing's full text at ingest (`repo::upsert_event`), schema.org forms, open-at/status/display; the listing SQL mirrors its JSON shape
- `src/venue_type.rs` — venue type facet (pure): overrides from `events.venues.venue_type`, source defaults, venue-name keywords; applied by `repo::sync_venue_types` after each ingest run
- `src/matching.rs` — fuzzy cross-source match rules (pure)
- `src/sources/` — `Source` trait, `jsonld` helpers, `ticketmaster`, `serpentine`
- `src/repo.rs` — all SQL (upsert/merge, runs, health issues)
- `src/runner.rs` — ingest run; `src/health.rs` rules + issue lifecycle; `src/github.rs` REST filer
- `src/thumbs.rs` — thumbnailer (fetch once, resize, store in `events.thumbnails`)
- `src/enrich/` — AI enrichment + embeddings after ingest: `input` (what the
  model sees, input hash), `prompt.txt` + `output` (vocabularies, schema,
  validation), `requesty` (client, credit-exhaustion detection), `embed`
  (embedding text), `store` (its SQL, "More like this"), `mod` (the pass,
  caps, ntfy alert). `examples/enrich_eval.rs` evaluates prompts/models
  without a database.
- `src/qa/` — scraper QA: `rules` (per-run sanity rules, no AI), `code`
  (per-source code hash; list new source files in `SOURCE_FILES`), `input`
  (page text via Readability/visible text + JSON-LD, the judge's message),
  `prompt.txt` + `output` (verdict validation), `issue` ("Scraper check"
  issues), `store` (its SQL), `mod` (`QaChecker`: when a check is due,
  caps, the check)
- `src/notify.rs` — ntfy owner alerts
- `src/contact.rs` — `/contact` venue requests (spam checks, `venue-request` issues, pending filing)
- `src/suggestions.rs` — site-suggestion validation, domain dedupe, IP rate limit, new-scraper issues
- `src/api.rs` — axum router (`/healthz`, read API, `POST /v1/suggestions`, CORS); `docs/api.md` documents it
- `src/listing.rs` — `GET /v1/events` parameter parsing and cursors (pure)
- `src/search.rs` — `q=` full-text search: accent folding (must match `events.search_fold`), prefix query, typo correction (no `pg_trgm`)
- `docs/design/stitch-2026-09/` — the visual design (DESIGN.md tokens/components + Stitch screens); follow it for UI work, but Stitch's copy is not ours (see its README)
- `src/calendar.rs` + `src/web/calendar_page.rs` + `src/ics.rs` — calendar views (`/calendar`, `/saved/calendar`: London date ranges, placement of long-running events) and the `/calendar.ics` feed
- `static/fonts/` — self-hosted, subset woff2 fonts (SIL OFL, licences alongside), embedded and served at `/static/fonts/`
- `src/share.rs` — event hand-offs (pure): `.ics`, Google Calendar link, Google/Apple Maps links
- `src/web.rs` + `src/web.css` + `src/web.js` — HTML pages (`/`, `/events/{id}`, `/sources`, `/saved`, `/about`, `POST /suggest`, `/thumbs/...`) and the Saved-events script
- `src/web/map.rs` + `src/map.mjs` — `/map` ("Near me, right now"), `/tiles/london.pmtiles` (range requests) and the vendored map assets; `docs/map.md` (tiles refresh, licences, privacy)
- `ops/sql/create-role.sql` — one-off role/grants script for the DB owner
- `tests/` — integration tests (`common/` helpers), `fixtures/`, `snapshots/`
