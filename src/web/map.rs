//! `GET /map`: "Near me, right now" (issue #71). A list of events that are
//! on now or start within 3 hours (or later today), nearest first from an
//! area preset, plus a map drawn by `/static/map.mjs` from our own
//! self-hosted London vector tiles.
//!
//! - **Without JavaScript** the page is the server-rendered list for the
//!   chosen area preset ([`AREAS`], Central London by default), with plain
//!   links for the area, "Right now"/"All today" and a sort form.
//! - **With JavaScript** the map appears next to (desktop) or above (phone)
//!   the list, with numbered markers. "Use my exact coordinates" asks the
//!   browser for its position only when tapped; the position never leaves
//!   the browser: the script fetches the same London-wide `at=now` listing
//!   anyone gets (`GET /v1/events?at=now`, no location in it) and works out
//!   distances itself.
//! - **Tiles** are one PMTiles file (a Protomaps basemap extract of Greater
//!   London, see `docs/map.md`) served by [`tiles`] from disk with HTTP range
//!   requests; MapLibre GL JS, the pmtiles reader, the map styles and the
//!   label glyphs are vendored under `static/map/` and served from our origin
//!   ([`MAP_ASSETS`]). Nothing is loaded from third parties, and the page
//!   keeps the site's strict CSP; it only adds `worker-src 'self'` (MapLibre
//!   decodes tiles in a module worker) and allows geolocation for itself.

use std::path::{Path as FsPath, PathBuf};
use std::sync::Arc;

use axum::extract::Request;
use tower_http::services::ServeFile;

use super::*;

/// CSP for `/map`: the site's [`CSP`] plus `worker-src 'self'` (MapLibre's
/// module worker, same origin; no `blob:`).
pub const MAP_CSP: &str = "default-src 'self'; script-src 'self'; worker-src 'self'; \
     img-src 'self'; style-src 'self'; form-action 'self'; frame-ancestors 'none'; base-uri 'none'";

/// CSP of the vendored map files (see [`map_asset`]).
pub const ASSET_CSP: &str = "default-src 'self'; frame-ancestors 'none'; base-uri 'none'";

/// `Permissions-Policy` for `/map`: like every other response, except that
/// the page itself may ask for the visitor's location (only when they tap
/// "Use my exact coordinates").
pub const MAP_PERMISSIONS_POLICY: &str =
    "camera=(), microphone=(), geolocation=(self), payment=(), usb=()";

/// Most events listed on the page (the nearest ones).
pub const MAP_LIMIT: i64 = 100;

/// The published path of the tiles.
pub const TILES_PATH: &str = "/tiles/london.pmtiles";

/// The London tiles on disk, with a version for cache-busting URLs.
#[derive(Debug)]
pub struct TileFile {
    path: PathBuf,
    /// Hash of the file's first 16 KiB (the PMTiles header and root
    /// directory, which change whenever the tiles do).
    version: String,
}

impl TileFile {
    /// `None` (logged) when the file is missing or unreadable: `/map` then
    /// shows only the list.
    pub fn open(path: &FsPath) -> Option<Arc<TileFile>> {
        use std::io::Read;
        let mut head = Vec::with_capacity(16 * 1024);
        let read = std::fs::File::open(path)
            .and_then(|f| f.take(16 * 1024).read_to_end(&mut head).map(|_| ()));
        match read {
            Ok(()) if head.starts_with(b"PMTiles") => {
                let hash = Sha256::digest(&head);
                let version = hash[..6].iter().map(|b| format!("{b:02x}")).collect();
                Some(Arc::new(TileFile {
                    path: path.to_path_buf(),
                    version,
                }))
            }
            Ok(()) => {
                tracing::warn!(path = %path.display(), "tiles file is not PMTiles; map disabled");
                None
            }
            Err(e) => {
                tracing::warn!(path = %path.display(), error = %e, "tiles file unavailable; map disabled");
                None
            }
        }
    }

    pub fn url(&self) -> String {
        format!("{TILES_PATH}?v={}", self.version)
    }
}

/// `GET /tiles/london.pmtiles`: the whole file or byte ranges (`Range`,
/// `206`, `Content-Range`, `416`; tower-http's `ServeFile`), cached for a
/// year at the versioned URL. Only routed when the file is present.
pub(crate) fn tile_routes(tiles: Arc<TileFile>) -> Router<AppState> {
    Router::new()
        .route_service(TILES_PATH, ServeFile::new(&tiles.path))
        .layer(axum::middleware::from_fn_with_state(tiles, tile_headers))
}

async fn tile_headers(
    State(tiles): State<Arc<TileFile>>,
    req: Request,
    next: axum::middleware::Next,
) -> Response {
    let versioned = req
        .uri()
        .query()
        .is_some_and(|q| q == format!("v={}", tiles.version));
    let mut resp = next.run(req).await;
    let h = resp.headers_mut();
    h.insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static(if versioned {
            "public, max-age=31536000, immutable"
        } else {
            "public, max-age=3600"
        }),
    );
    if resp.status().is_success() {
        resp.headers_mut().insert(
            header::CONTENT_TYPE,
            HeaderValue::from_static("application/vnd.pmtiles"),
        );
    }
    resp
}

// ---------------------------------------------------------------- assets

const MAPLIBRE_DIR: &str = "maplibre-gl-6.11.2";
const PMTILES_DIR: &str = "pmtiles-4.5.0";

/// Vendored map files, served at `/static/map/<path>` (pinned versions in
/// the path, so they are cached for a year; see `docs/map.md` for their
/// origin and SHA-256 sums).
pub const MAP_ASSETS: [(&str, &str, &[u8]); 7] = [
    (
        "maplibre-gl-6.11.2/maplibre-gl.mjs",
        "text/javascript; charset=utf-8",
        include_bytes!("../../static/map/maplibre-gl-6.11.2/maplibre-gl.mjs"),
    ),
    (
        "maplibre-gl-6.11.2/maplibre-gl-shared.mjs",
        "text/javascript; charset=utf-8",
        include_bytes!("../../static/map/maplibre-gl-6.11.2/maplibre-gl-shared.mjs"),
    ),
    (
        "maplibre-gl-6.11.2/maplibre-gl-worker.mjs",
        "text/javascript; charset=utf-8",
        include_bytes!("../../static/map/maplibre-gl-6.11.2/maplibre-gl-worker.mjs"),
    ),
    (
        "pmtiles-4.5.0/pmtiles.js",
        "text/javascript; charset=utf-8",
        include_bytes!("../../static/map/pmtiles-4.5.0/pmtiles.js"),
    ),
    (
        "style-light.json",
        "application/json",
        include_bytes!("../../static/map/style-light.json"),
    ),
    (
        "style-dark.json",
        "application/json",
        include_bytes!("../../static/map/style-dark.json"),
    ),
    (
        "map.mjs",
        "text/javascript; charset=utf-8",
        include_bytes!("../map.mjs"),
    ),
];

/// Label glyphs (Noto Sans, SIL OFL; `static/map/fonts/OFL.txt`): Latin,
/// Latin Extended-A and general punctuation, the ranges London's labels use.
const GLYPHS: [(&str, &str, &[u8]); 9] = [
    (
        "Noto Sans Regular",
        "0-255",
        include_bytes!("../../static/map/fonts/noto-sans-regular/0-255.pbf"),
    ),
    (
        "Noto Sans Regular",
        "256-511",
        include_bytes!("../../static/map/fonts/noto-sans-regular/256-511.pbf"),
    ),
    (
        "Noto Sans Regular",
        "8192-8447",
        include_bytes!("../../static/map/fonts/noto-sans-regular/8192-8447.pbf"),
    ),
    (
        "Noto Sans Medium",
        "0-255",
        include_bytes!("../../static/map/fonts/noto-sans-medium/0-255.pbf"),
    ),
    (
        "Noto Sans Medium",
        "256-511",
        include_bytes!("../../static/map/fonts/noto-sans-medium/256-511.pbf"),
    ),
    (
        "Noto Sans Medium",
        "8192-8447",
        include_bytes!("../../static/map/fonts/noto-sans-medium/8192-8447.pbf"),
    ),
    (
        "Noto Sans Italic",
        "0-255",
        include_bytes!("../../static/map/fonts/noto-sans-italic/0-255.pbf"),
    ),
    (
        "Noto Sans Italic",
        "256-511",
        include_bytes!("../../static/map/fonts/noto-sans-italic/256-511.pbf"),
    ),
    (
        "Noto Sans Italic",
        "8192-8447",
        include_bytes!("../../static/map/fonts/noto-sans-italic/8192-8447.pbf"),
    ),
];

fn asset_bytes(path: &str) -> Option<(&'static str, &'static [u8])> {
    MAP_ASSETS
        .iter()
        .find(|(p, _, _)| *p == path)
        .map(|(_, t, b)| (*t, *b))
}

/// `/static/map/<path>?v=<content hash>`.
fn asset_url(path: &str) -> String {
    let bytes = asset_bytes(path).map_or(&[][..], |(_, b)| b);
    let hash = Sha256::digest(bytes);
    let v: String = hash[..6].iter().map(|b| format!("{b:02x}")).collect();
    format!("/static/map/{path}?v={v}")
}

static MAP_JS_URL: LazyLock<String> = LazyLock::new(|| asset_url("map.mjs"));
static PMTILES_JS_URL: LazyLock<String> =
    LazyLock::new(|| asset_url(&format!("{PMTILES_DIR}/pmtiles.js")));
static STYLE_LIGHT_URL: LazyLock<String> = LazyLock::new(|| asset_url("style-light.json"));
static STYLE_DARK_URL: LazyLock<String> = LazyLock::new(|| asset_url("style-dark.json"));

async fn map_asset(Path(path): Path<String>, RawQuery(q): RawQuery) -> Response {
    let Some((content_type, bytes)) = asset_bytes(&path) else {
        return (StatusCode::NOT_FOUND, "not found").into_response();
    };
    // Library files have their version in the path; the rest are linked
    // with ?v=<hash>.
    let immutable = path.starts_with(MAPLIBRE_DIR)
        || path.starts_with(PMTILES_DIR)
        || q.is_some_and(|q| q.starts_with("v="));
    (
        [
            (header::CONTENT_TYPE, content_type),
            (
                header::CACHE_CONTROL,
                if immutable {
                    "public, max-age=31536000, immutable"
                } else {
                    "public, max-age=3600"
                },
            ),
            // A dedicated worker runs under its own script's CSP (not the
            // page's): MapLibre's worker must import its shared module from
            // our origin, which the site-wide `default-src 'none'` would block.
            (header::CONTENT_SECURITY_POLICY, ASSET_CSP),
        ],
        bytes,
    )
        .into_response()
}

/// `GET /static/map-glyphs/{fontstack}/{range}.pbf`. A stack we don't have
/// (e.g. a script outside Latin) or a range outside the ones we ship gets an
/// empty glyph set, so MapLibre skips those labels instead of erroring.
async fn glyphs(Path((stack, file)): Path<(String, String)>) -> Response {
    let Some(range) = file.strip_suffix(".pbf") else {
        return (StatusCode::NOT_FOUND, "not found").into_response();
    };
    let bytes = stack
        .split(',')
        .map(str::trim)
        .find_map(|font| {
            GLYPHS
                .iter()
                .find(|(f, r, _)| *f == font && *r == range)
                .map(|(_, _, b)| *b)
        })
        .unwrap_or(&[]);
    (
        [
            (header::CONTENT_TYPE, "application/x-protobuf"),
            (header::CACHE_CONTROL, "public, max-age=604800"),
        ],
        bytes,
    )
        .into_response()
}

pub(super) fn routes() -> Router<AppState> {
    Router::new()
        .route("/map", get(map_page))
        .route("/static/map/{*path}", get(map_asset))
        .route("/static/map-glyphs/{stack}/{file}", get(glyphs))
}

// ---------------------------------------------------------------- page

/// "Right now" (on now or within 3 h) or "All today".
#[derive(Clone, Copy, PartialEq, Eq)]
enum Span {
    Now,
    Today,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Sort {
    Nearest,
    Soonest,
}

struct MapFilters {
    area: &'static Area,
    span: Span,
    sort: Sort,
}

impl MapFilters {
    /// Unknown values fall back to the defaults (Central London, right now,
    /// nearest first): it is a page, not an API.
    fn parse(raw: &str) -> Self {
        let mut f = MapFilters {
            area: &AREAS[0],
            span: Span::Now,
            sort: Sort::Nearest,
        };
        for (k, v) in url::form_urlencoded::parse(raw.as_bytes()) {
            match (k.as_ref(), v.as_ref()) {
                ("area", v) => {
                    if let Some(a) = AREAS.iter().find(|a| a.key == v) {
                        f.area = a;
                    }
                }
                ("span", "today") => f.span = Span::Today,
                ("span", _) => f.span = Span::Now,
                ("sort", "soon") => f.sort = Sort::Soonest,
                ("sort", _) => f.sort = Sort::Nearest,
                _ => {}
            }
        }
        f
    }

    fn api_query(&self) -> String {
        let mut s = url::form_urlencoded::Serializer::new(String::new());
        s.append_pair(
            "at",
            match self.span {
                Span::Now => "now",
                Span::Today => "today",
            },
        );
        s.append_pair("near", &format!("{},{}", self.area.lat, self.area.lng));
        s.append_pair("radius_km", &self.area.radius_km.to_string());
        s.append_pair("limit", &MAP_LIMIT.to_string());
        s.finish()
    }

    /// `/map?…` with `change` applied.
    fn link(&self, change: impl FnOnce(&mut MapFilters)) -> String {
        let mut g = MapFilters {
            area: self.area,
            span: self.span,
            sort: self.sort,
        };
        change(&mut g);
        let mut s = url::form_urlencoded::Serializer::new(String::new());
        s.append_pair("area", g.area.key);
        if g.span == Span::Today {
            s.append_pair("span", "today");
        }
        if g.sort == Sort::Soonest {
            s.append_pair("sort", "soon");
        }
        format!("/map?{}", s.finish())
    }
}

fn hm(t: DateTime<Utc>) -> String {
    london(t).format("%H:%M").to_string()
}

/// The status line of a card: what is happening now, in London time. An
/// ongoing run with known opening hours (`crate::hours`, #206) says
/// "Open now until 18:00", "Opens today at 11:00", "Closed now" or "Closed
/// today"; without them it says to check the hours. Mirrored by
/// `liveLabel` in `map.mjs`.
pub(crate) fn live_label(
    start: DateTime<Utc>,
    end: Option<DateTime<Utc>>,
    all_day: bool,
    hours: Option<&crate::hours::OpeningHours>,
    now: DateTime<Utc>,
) -> String {
    let today = london(now).date_naive();
    let day = |t: DateTime<Utc>| {
        if london(t).date_naive() == today {
            String::new()
        } else {
            format!("{} ", london(t).format("%a"))
        }
    };
    // "today", or the weekday when the window runs past London midnight.
    let on_day = |t: DateTime<Utc>| match day(t) {
        d if d.is_empty() => "today".to_string(),
        d => d.trim_end().to_string(),
    };
    let end = end.filter(|e| *e > start);
    let ranged = end.is_some_and(|e| london(e).date_naive() != london(start).date_naive());
    let untimed = all_day || london(start).time() == NaiveTime::MIN;
    if ranged {
        return match (start > now, untimed) {
            (true, false) => format!("Opens {}{}", day(start), hm(start)),
            (true, true) => format!("Opens {} · check opening hours", on_day(start)),
            (false, _) => match hours {
                Some(h) => h.today_status(now),
                None => "Open today · check opening hours".into(),
            },
        };
    }
    if untimed {
        return format!("{} · check opening hours", title_case(&on_day(start)));
    }
    match (start > now, end) {
        (true, Some(e)) => format!("Starts {}{}–{}", day(start), hm(start), hm(e)),
        (true, None) => format!("Starts {}{}", day(start), hm(start)),
        (false, Some(e)) => format!("On now until {}", hm(e)),
        (false, None) => format!("Started {}", hm(start)),
    }
}

/// "1.2 km from the centre of <area>" (the page's own distances are from the
/// area preset; the script labels walking times from the visitor instead).
fn area_distance(km: f64) -> String {
    if km < 0.1 {
        "At the area centre".into()
    } else {
        format!("{km:.1} km from area centre")
    }
}

/// [`live_label`] for an event: a multi-session event (#207) by its
/// current or next session (an all-day session as an untimed day).
fn event_live_label(e: &EventJson, now: DateTime<Utc>) -> String {
    match crate::model::next_session(&e.sessions, now) {
        Some((s, _)) => {
            let untimed = london(s.starts_at).time() == NaiveTime::MIN;
            let end = if untimed { None } else { s.ends_at };
            live_label(s.starts_at, end, untimed, None, now)
        }
        None => live_label(e.starts_at, e.ends_at, e.all_day, e.hours.as_ref(), now),
    }
}

fn near_card(e: &EventJson, n: usize, now: DateTime<Utc>) -> Markup {
    let detail = format!("/events/{}", e.id);
    let status = event_live_label(e, now);
    let primary = primary_source(e);
    let credit_url = e
        .image_credit
        .as_ref()
        .and_then(|c| safe_link(Some(&c.url)));
    let (tw, th) = e.thumbnail_size.unwrap_or((480, 270));
    html! {
        li {
            article class="near-card" id={ "ev-" (e.id) }
                data-event-id=(e.id) data-n=(n)
                data-lat=[e.lat] data-lng=[e.lng]
                data-title=(e.title)
                data-venue=(e.venue_name.as_deref().unwrap_or(""))
                data-status=(status)
                data-category=(title_case(&e.category))
                data-starts=(e.starts_at.to_rfc3339())
                data-thumb=[e.thumbnail_url.as_deref().filter(|_| credit_url.is_some())]
                data-thumb-w=(tw) data-thumb-h=(th)
                data-credit-name=[e.image_credit.as_ref().map(|c| c.name.as_str())]
                data-credit-url=[credit_url.as_deref()]
                data-cta-url=[primary.as_ref().map(|(_, u)| u.as_str())]
                data-cta-name=[primary.as_ref().map(|(p, _)| p.display_name.as_str())] {
                div class="near-top" {
                    span class="near-num" { span class="vh" { "Map marker " } (n) }
                    @if let Some(d) = e.distance_km {
                        span class="walk" data-slot="walk" { (area_distance(d)) }
                    }
                    (save_button(e))
                }
                h2 { a href=(detail) { (e.title) } }
                @if let Some(v) = &e.venue_name { p class="near-venue" { (v) } }
                p class="near-status" {
                    span class="dot" {}
                    span { (status) }
                    @if let Some(p) = price(e) {
                        span class="sep" aria-hidden="true" { "/" }
                        span class={ "near-price" @if e.is_free { " free" } } { (p) }
                    }
                }
                div class="near-foot" {
                    p class="tags" {
                        span class="badge" { (title_case(&e.category)) }
                        @if e.is_opening == Some(true) {
                            " " span class="badge opening" { "Opening" }
                        }
                    }
                    // Our detail page first (as on the event cards); the
                    // venue's own page is the secondary link.
                    p class="near-links" {
                        a class="arrow-link" href=(detail) { "Details →" span class="vh" { ": " (e.title) } }
                        @if let Some((p, u)) = &primary {
                            a class="near-source" href=(u) rel="noopener" {
                                "See it on " (p.display_name) " →"
                                span class="vh" { ": " (e.title) }
                            }
                        }
                    }
                }
            }
        }
    }
}

/// The card for the script's "my location" list (filled via `data-slot`s
/// with `textContent`); keep it in step with [`near_card`].
fn near_card_template() -> Markup {
    html! {
        template id="near-card-template" {
            li {
                article class="near-card" {
                    div class="near-top" {
                        span class="near-num" { span class="vh" { "Map marker " } span data-slot="n" {} }
                        span class="walk" data-slot="walk" {}
                        button type="button" class="save" aria-pressed="false" data-save-id="" data-title="" {
                            (bookmark())
                            span class="save-label" { "Save" }
                            span class="vh" data-slot="save-title" {}
                        }
                    }
                    h2 { a data-slot="title" href="/" {} }
                    p class="near-venue" data-slot="venue" {}
                    p class="near-status" {
                        span class="dot" {}
                        span data-slot="status" {}
                        span class="sep" aria-hidden="true" data-slot="sep" hidden { "/" }
                        span class="near-price" data-slot="price" hidden {}
                    }
                    div class="near-foot" {
                        p class="tags" { span class="badge" data-slot="category" {} }
                        p class="near-links" {
                            a class="arrow-link" data-slot="details" href="/" { "Details →" span class="vh" data-slot="details-title" {} }
                            a class="near-source" data-slot="cta" rel="noopener" hidden {}
                        }
                    }
                }
            }
        }
    }
}

/// Paper-plane "locate" icon (decorative).
fn locate_icon() -> Markup {
    html! {
        svg class="locate-icon" aria-hidden="true" focusable="false" viewBox="0 0 24 24" width="18" height="18" {
            path d="M21 3 3 10.5l7.5 3L13.5 21z" {}
        }
    }
}

async fn map_page(State(state): State<AppState>, RawQuery(raw): RawQuery) -> Response {
    let f = MapFilters::parse(raw.as_deref().unwrap_or(""));
    let now = Utc::now();
    let query = match listing::parse_query_at(&f.api_query(), now) {
        Ok(q) => q,
        Err(e) => return internal_error(e),
    };
    let (mut events, more) = match api::event_page(&state.pool, &query).await {
        Ok(r) => r,
        Err(e) => return internal_error(e),
    };
    if f.sort == Sort::Soonest {
        // Ongoing events first ("now"), then by start; nearer first on ties.
        events.sort_by(|a, b| {
            (a.starts_at.max(now), a.distance_km.unwrap_or(f64::MAX))
                .partial_cmp(&(b.starts_at.max(now), b.distance_km.unwrap_or(f64::MAX)))
                .unwrap_or(std::cmp::Ordering::Equal)
        });
    }
    let span_label = match f.span {
        Span::Now => "on now or starting in the next 3 hours",
        Span::Today => "on now or later today",
    };
    let tiles_url = state.tiles.as_ref().map(|t| t.url());
    let main = html! {
        section class="near-page" {
            div class="near-layout" {
                div class="near-head" {
                    div class="near-title-row" {
                        h1 class="near-title" { span class="dot" {} "Near me, right now" }
                        span class="mono near-clock" {
                            time datetime=(now.to_rfc3339()) { (london(now).format("%a %-d %b · %H:%M").to_string()) }
                        }
                    }
                    p class="near-privacy small" {
                        "Your exact location stays on this device: the page asks our server for "
                        "everything on in London and works out distances in your browser. "
                        "For public transport times it sends your position rounded to about 200 m. "
                        "Map tiles and data come only from our own server; like any map, it loads "
                        "the tiles for the area on screen."
                    }
                    button type="button" id="locate" class="locate" hidden {
                        (locate_icon()) "Use my exact coordinates"
                    }
                    p id="locate-status" class="near-note" role="status" aria-live="polite" {}
                    nav class="area-nav" aria-label="Area" {
                        p class="area-label" {
                            span { "Areas:" }
                            span class="area-active" data-slot="area-active" { (f.area.label) }
                        }
                        ul class="area-grid" {
                            @for a in &AREAS {
                                li {
                                    a class="area-btn" href=(f.link(|g| g.area = a))
                                        data-area=(a.key) data-lat=(a.lat) data-lng=(a.lng) data-radius=(a.radius_km)
                                        aria-current=[(a.key == f.area.key).then_some("true")] { (a.label) }
                                }
                            }
                        }
                    }
                    div class="near-controls" {
                        nav class="segments" aria-label="When" {
                            a class="seg" href=(f.link(|g| g.span = Span::Now)) data-span="now"
                                aria-current=[(f.span == Span::Now).then_some("true")] { "Right now (<3h)" }
                            a class="seg" href=(f.link(|g| g.span = Span::Today)) data-span="today"
                                aria-current=[(f.span == Span::Today).then_some("true")] { "All today" }
                        }
                        form class="near-sort" method="get" action="/map" {
                            input type="hidden" name="area" value=(f.area.key);
                            @if f.span == Span::Today { input type="hidden" name="span" value="today"; }
                            label for="sort" class="vh" { "Sort" }
                            select id="sort" name="sort" {
                                option value="near" selected[f.sort == Sort::Nearest] { "Nearest first" }
                                option value="soon" selected[f.sort == Sort::Soonest] { "Starting soonest" }
                            }
                            noscript { button type="submit" class="secondary" { "Sort" } }
                        }
                    }
                }
                @if let Some(tiles) = &tiles_url {
                    div class="map-pane" id="map-pane" hidden
                        data-tiles=(tiles) data-style-light=(STYLE_LIGHT_URL.as_str())
                        data-style-dark=(STYLE_DARK_URL.as_str()) data-glyphs="/static/map-glyphs/{fontstack}/{range}.pbf"
                        data-maplibre=(format!("/static/map/{MAPLIBRE_DIR}/maplibre-gl.mjs"))
                        data-center-lat=(f.area.lat) data-center-lng=(f.area.lng) {
                        div id="map" class="map-canvas" role="region" aria-label="Map of the events listed" {}
                        div class="map-zoom" {
                            button type="button" class="map-btn" data-zoom="in" aria-label="Zoom in" { "+" }
                            button type="button" class="map-btn" data-zoom="out" aria-label="Zoom out" { "−" }
                        }
                        button type="button" class="map-fit secondary" id="map-fit" hidden { "Show all on map" }
                        p class="map-attrib" {
                            "© " a href="https://www.openstreetmap.org/copyright" rel="noopener" { "OpenStreetMap contributors" }
                            " · " a href="https://protomaps.com" rel="noopener" { "Protomaps" }
                        }
                    }
                }
                div class="near-results" {
                    p class="results-status" id="near-count" role="status" {
                        span {
                            strong { (events.len()) @if events.len() == 1 { " event" } @else { " events" } }
                            " " (span_label) ", within " (format!("{}", f.area.radius_km)) " km of " (f.area.label)
                        }
                        span data-slot="order" { @if f.sort == Sort::Nearest { "Nearest first" } @else { "Starting soonest" } }
                    }
                    @if events.is_empty() {
                        p class="empty" id="near-empty" {
                            "Nothing listed " (span_label) " here. "
                            @if f.span == Span::Now {
                                a href=(f.link(|g| g.span = Span::Today)) { "See all of today" } " or pick another area."
                            } @else {
                                "Pick another area, or " a href="/" { "browse all events" } "."
                            }
                        }
                    }
                    ol class="near-list" id="near-list" aria-label="Events" {
                        @for (i, e) in events.iter().enumerate() { (near_card(e, i + 1, now)) }
                    }
                    @if more.is_some() {
                        p class="small" { "Showing the nearest " (MAP_LIMIT) "." }
                    }
                    p class="small near-foot-note" {
                        "Only events with a known location are shown. Walking times are "
                        "estimates from straight-line distance."
                    }
                    (near_card_template())
                }
            }
        }
        @if tiles_url.is_some() {
            script src=(PMTILES_JS_URL.as_str()) defer {}
        }
        script type="module" src=(MAP_JS_URL.as_str()) {}
    };
    let mut resp = page(StatusCode::OK, "Near me, right now", Nav::Map, main);
    let h = resp.headers_mut();
    h.insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static(MAP_CSP),
    );
    h.insert(
        "permissions-policy",
        HeaderValue::from_static(MAP_PERMISSIONS_POLICY),
    );
    resp
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn t(h: u32, m: u32) -> DateTime<Utc> {
        // Sat 3 Oct 2026, BST (UTC+1): London h:m.
        Utc.with_ymd_and_hms(2026, 10, 3, h - 1, m, 0).unwrap()
    }

    #[test]
    fn live_labels_say_what_is_on_now() {
        let now = t(14, 0);
        let day = |d: u32| {
            Utc.with_ymd_and_hms(2026, 10, d, 0, 0, 0).unwrap() - chrono::Duration::hours(1)
        };
        // An exhibition run (date-only) that is on.
        assert_eq!(
            live_label(day(1), Some(day(20)), true, None, now),
            "Open today · check opening hours"
        );
        // Timed run (Barbican-style) that is on.
        assert_eq!(
            live_label(
                t(10, 0) - chrono::Duration::days(300),
                Some(t(10, 0) + chrono::Duration::days(90)),
                false,
                None,
                now
            ),
            "Open today · check opening hours"
        );
        assert_eq!(
            live_label(day(3), None, true, None, now),
            "Today · check opening hours"
        );
        assert_eq!(
            live_label(t(16, 30), Some(t(18, 0)), false, None, now),
            "Starts 16:30–18:00"
        );
        assert_eq!(
            live_label(t(16, 30), None, false, None, now),
            "Starts 16:30"
        );
        assert_eq!(
            live_label(t(13, 30), Some(t(15, 0)), false, None, now),
            "On now until 15:00"
        );
        assert_eq!(
            live_label(t(13, 30), None, false, None, now),
            "Started 13:30"
        );
        assert_eq!(
            live_label(
                t(18, 0),
                Some(t(18, 0) + chrono::Duration::days(30)),
                false,
                None,
                now
            ),
            "Opens 18:00"
        );
        // After midnight: the day is named.
        let late = t(23, 0);
        assert_eq!(
            live_label(late + chrono::Duration::hours(2), None, false, None, late),
            "Starts Sun 01:00"
        );
        assert_eq!(
            live_label(day(4), None, true, None, late),
            "Sun · check opening hours"
        );
        assert_eq!(
            live_label(day(4), Some(day(30)), true, None, late),
            "Opens Sun · check opening hours"
        );
        // With opening hours (Sat 11:00–15:00, Sun 12:00–17:00; Mon–Fri closed).
        let hours = crate::hours::OpeningHours::new(vec![
            crate::hours::HoursRule {
                days: vec![6],
                opens: NaiveTime::from_hms_opt(11, 0, 0).unwrap(),
                closes: NaiveTime::from_hms_opt(15, 0, 0).unwrap(),
            },
            crate::hours::HoursRule {
                days: vec![7],
                opens: NaiveTime::from_hms_opt(12, 0, 0).unwrap(),
                closes: NaiveTime::from_hms_opt(17, 0, 0).unwrap(),
            },
        ])
        .unwrap();
        let run = |at| live_label(day(1), Some(day(20)), true, Some(&hours), at);
        assert_eq!(run(now), "Open now until 15:00");
        assert_eq!(run(t(10, 0)), "Opens today at 11:00");
        assert_eq!(run(t(16, 0)), "Closed now");
        assert_eq!(run(t(14, 0) + chrono::Duration::days(2)), "Closed today");
        // Not started yet: the hours don't say when it opens.
        assert_eq!(
            live_label(day(4), Some(day(30)), true, Some(&hours), late),
            "Opens Sun · check opening hours"
        );
    }

    #[test]
    fn filters_fall_back_to_defaults() {
        let f = MapFilters::parse("area=nowhere&span=later&sort=x");
        assert_eq!(f.area.key, "central");
        assert!(f.span == Span::Now && f.sort == Sort::Nearest);
        let f = MapFilters::parse("area=east&span=today&sort=soon");
        assert_eq!(f.area.key, "east");
        assert!(f.span == Span::Today && f.sort == Sort::Soonest);
        assert_eq!(f.link(|_| {}), "/map?area=east&span=today&sort=soon");
        assert!(
            f.api_query()
                .starts_with("at=today&near=51.52%2C-0.075&radius_km=3")
        );
    }
}
