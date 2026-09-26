//! Human-facing HTML pages, server-rendered by the API process itself:
//! `GET /` (filterable upcoming events), `GET /events/{id}`, `GET /sources`
//! and `POST /suggest` (the "Suggest a venue site" form).
//!
//! Templating is `maud`: templates are Rust code checked at compile time, and
//! every interpolated value is HTML-escaped unless explicitly wrapped in
//! `PreEscaped` (which this module never does with data). Pages use no
//! JavaScript and no third-party assets; the stylesheet is served from
//! `/static/style.css`, so the Content-Security-Policy needs no
//! `'unsafe-inline'`. Images are only our own thumbnails (`/thumbs/...`,
//! see `crate::thumbs`), each credited to its source, so `img-src` is
//! `'self'`.
//!
//! Links to venues and sources use `rel="noopener"` but deliberately NOT
//! `noreferrer`: we want venues to see (via the Referer, which our
//! `Referrer-Policy: strict-origin-when-cross-origin` limits to our origin)
//! that visitors came from Muse & Mingle. The primary call to action on cards and
//! detail pages is the source's own page ("See it on Barbican →"). Everything shown comes from the same helpers as the
//! JSON API (`api::event_page`, `api::event_by_id`, `api::source_values`,
//! `api::submit_suggestion`), and filters go through `listing::parse_query`,
//! so validation is identical.

use std::net::SocketAddr;
use std::sync::LazyLock;

use axum::Router;
use axum::extract::rejection::FormRejection;
use axum::extract::{ConnectInfo, Form, Path, RawQuery, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use chrono::{DateTime, Datelike, NaiveDate, NaiveTime, Utc, Weekday};
use chrono_tz::Europe::London;
use maud::{DOCTYPE, Markup, html};
use rust_decimal::Decimal;
use serde::Deserialize;
use serde_json::Value;
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::api::{self, AppState, CountsJson, EventJson, SourceLinkJson};
use crate::listing::{self, When};
use crate::model::{Category, SourceKind};
use crate::repo;
use crate::suggestions::{MAX_NOTE_CHARS, Outcome};

/// Content-Security-Policy for every HTML response.
/// Images are our own thumbnails only (`/thumbs/...`), never hotlinked.
pub const CSP: &str = "default-src 'self'; script-src 'self'; img-src 'self'; \
     style-src 'self'; form-action 'self'; frame-ancestors 'none'; base-uri 'none'";

/// Site name shown in page titles and the header (the crate is `musenmingle`).
pub const BRAND: &str = "Muse & Mingle";

const TAGLINE: &str = "What's on in London for creative people";

/// Events per page on `/`.
pub const PAGE_SIZE: i64 = 24;

const STYLESHEET_SRC: &str = include_str!("web.css");

/// Self-hosted fonts (SIL OFL 1.1, licences in `static/fonts/`), subset to
/// Latin + Latin-1 + common punctuation: Hanken Grotesk as one variable
/// file (weights 400–700) and JetBrains Mono 400. Served from our origin
/// (`font-src` falls back to `default-src 'self'`).
pub const FONTS: [(&str, &[u8]); 2] = [
    (
        "hanken-grotesk-latin-var.woff2",
        include_bytes!("../static/fonts/hanken-grotesk-latin-var.woff2"),
    ),
    (
        "jetbrains-mono-latin-400.woff2",
        include_bytes!("../static/fonts/jetbrains-mono-latin-400.woff2"),
    ),
];

/// `/static/fonts/<name>?v=<content hash>` (cached for a year, like the script).
fn font_url(name: &str, bytes: &[u8]) -> String {
    let hash = Sha256::digest(bytes);
    let v: String = hash[..6].iter().map(|b| format!("{b:02x}")).collect();
    format!("/static/fonts/{name}?v={v}")
}

static FONT_URLS: LazyLock<Vec<String>> =
    LazyLock::new(|| FONTS.iter().map(|(n, b)| font_url(n, b)).collect());

/// `/static/style.css?v=<content hash>`, so a deploy never pairs new markup
/// with a cached old stylesheet.
static STYLESHEET_URL: LazyLock<String> = LazyLock::new(|| {
    let hash = Sha256::digest(STYLESHEET.as_bytes());
    let v: String = hash[..6].iter().map(|b| format!("{b:02x}")).collect();
    format!("/static/style.css?v={v}")
});

/// The stylesheet with each font's URL versioned by its content.
static STYLESHEET: LazyLock<String> = LazyLock::new(|| {
    FONTS
        .iter()
        .zip(FONT_URLS.iter())
        .fold(STYLESHEET_SRC.to_string(), |css, ((name, _), url)| {
            css.replace(&format!("/static/fonts/{name}\""), &format!("{url}\""))
        })
});

/// The one first-party script ("Saved" events; progressive enhancement).
const APP_JS: &str = include_str!("web.js");

/// `/static/app.js?v=<content hash>`: the URL changes with the content, so
/// the script can be cached for a year.
static APP_JS_URL: LazyLock<String> = LazyLock::new(|| {
    let hash = Sha256::digest(APP_JS.as_bytes());
    let v: String = hash[..6].iter().map(|b| format!("{b:02x}")).collect();
    format!("/static/app.js?v={v}")
});

/// A preset "near" area for the home-page filter.
pub struct Area {
    pub key: &'static str,
    pub label: &'static str,
    pub lat: f64,
    pub lng: f64,
    pub radius_km: f64,
}

pub const AREAS: [Area; 4] = [
    Area {
        key: "central",
        label: "Central & South Bank",
        lat: 51.5074,
        lng: -0.1225,
        radius_km: 2.5,
    },
    Area {
        key: "east",
        label: "East (City, Shoreditch, Whitechapel)",
        lat: 51.5200,
        lng: -0.0750,
        radius_km: 3.0,
    },
    Area {
        key: "kings-cross",
        label: "King's Cross",
        lat: 51.5320,
        lng: -0.1240,
        radius_km: 1.5,
    },
    Area {
        key: "south-kensington",
        label: "South Kensington & Hyde Park",
        lat: 51.4990,
        lng: -0.1750,
        radius_km: 2.0,
    },
];

pub(crate) fn routes() -> Router<AppState> {
    Router::new()
        .route("/", get(home))
        .route("/events/{id}", get(event_detail))
        .route("/sources", get(sources))
        .route("/about", get(about))
        .route(
            "/contact",
            get(contact_page)
                .post(contact_submit)
                .layer(axum::extract::DefaultBodyLimit::max(CONTACT_BODY_LIMIT)),
        )
        .route("/suggest", post(suggest))
        .route("/static/style.css", get(stylesheet))
        .route("/favicon.ico", get(favicon_ico))
        .route("/favicon.svg", get(favicon_svg))
        .route("/apple-touch-icon.png", get(apple_touch_icon))
        .route("/static/app.js", get(app_js))
        .route("/static/fonts/{name}", get(font))
        .route("/saved", get(saved))
        .route("/thumbs/{name}", get(thumbnail))
}

/// `GET /thumbs/{event_id}-{hash}.jpg` (immutable: the hash changes with the
/// bytes) or `GET /thumbs/{event_id}` (current thumbnail, revalidated).
/// A stale hash or unknown id is 404.
async fn thumbnail(
    State(state): State<AppState>,
    Path(name): Path<String>,
    headers: HeaderMap,
) -> Response {
    let not_found = || (StatusCode::NOT_FOUND, "not found").into_response();
    let Some((id, want_hash)) = crate::thumbs::parse_thumb_name(&name) else {
        return not_found();
    };
    let t = match repo::get_thumbnail(&state.pool, id).await {
        Ok(Some(t)) => t,
        Ok(None) => return not_found(),
        Err(e) => {
            tracing::error!(error = %e, "thumbnail query failed");
            return (StatusCode::INTERNAL_SERVER_ERROR, "internal error").into_response();
        }
    };
    if want_hash.is_some_and(|h| h != t.content_hash) {
        return not_found();
    }
    let etag = format!("\"{}\"", t.content_hash);
    let cache = if want_hash.is_some() {
        "public, max-age=604800, immutable"
    } else {
        "public, max-age=604800"
    };
    let matches = headers
        .get(header::IF_NONE_MATCH)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.split(',').any(|e| e.trim() == etag || e.trim() == "*"));
    let mut resp = if matches {
        StatusCode::NOT_MODIFIED.into_response()
    } else {
        (StatusCode::OK, t.bytes).into_response()
    };
    let h = resp.headers_mut();
    if !matches {
        if let Ok(v) = HeaderValue::from_str(&t.content_type) {
            h.insert(header::CONTENT_TYPE, v);
        }
    }
    h.insert(header::CACHE_CONTROL, HeaderValue::from_static(cache));
    if let Ok(v) = HeaderValue::from_str(&etag) {
        h.insert(header::ETAG, v);
    }
    h.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    resp
}

/// Site icons, served from our own origin (`img-src 'self'`).
const FAVICON_SVG: &str = include_str!("icons/favicon.svg");
const FAVICON_ICO: &[u8] = include_bytes!("icons/favicon.ico");
const APPLE_TOUCH_ICON: &[u8] = include_bytes!("icons/apple-touch-icon.png");

fn icon(content_type: &'static str, body: impl IntoResponse) -> Response {
    (
        [
            (header::CONTENT_TYPE, content_type),
            (header::CACHE_CONTROL, "public, max-age=86400"),
        ],
        body,
    )
        .into_response()
}

async fn favicon_svg() -> Response {
    icon("image/svg+xml", FAVICON_SVG)
}

async fn favicon_ico() -> Response {
    icon("image/x-icon", FAVICON_ICO)
}

async fn apple_touch_icon() -> Response {
    icon("image/png", APPLE_TOUCH_ICON)
}

async fn stylesheet(RawQuery(q): RawQuery) -> Response {
    // Versioned URLs (what pages link to) are immutable; a bare URL revalidates hourly.
    let cache = if q.is_some_and(|q| q.starts_with("v=")) {
        "public, max-age=31536000, immutable"
    } else {
        "public, max-age=3600"
    };
    (
        [
            (header::CONTENT_TYPE, "text/css; charset=utf-8"),
            (header::CACHE_CONTROL, cache),
        ],
        STYLESHEET.as_str(),
    )
        .into_response()
}

async fn font(Path(name): Path<String>) -> Response {
    let Some((_, bytes)) = FONTS.iter().find(|(n, _)| *n == name) else {
        return (StatusCode::NOT_FOUND, "not found").into_response();
    };
    (
        [
            (header::CONTENT_TYPE, "font/woff2"),
            (header::CACHE_CONTROL, "public, max-age=31536000, immutable"),
            (header::X_CONTENT_TYPE_OPTIONS, "nosniff"),
        ],
        *bytes,
    )
        .into_response()
}

async fn app_js() -> Response {
    (
        [
            (header::CONTENT_TYPE, "text/javascript; charset=utf-8"),
            (header::CACHE_CONTROL, "public, max-age=31536000, immutable"),
        ],
        APP_JS,
    )
        .into_response()
}

// ---------------------------------------------------------------- layout

/// Which main-navigation item a page belongs to (`aria-current="page"`).
#[derive(Clone, Copy, PartialEq, Eq)]
enum Nav {
    Events,
    Saved,
    Sources,
    About,
    Other,
}

/// The wordmark: a square frame with a cobalt dot (inline SVG, decorative;
/// the link's text is the site name). Colours come from the stylesheet.
fn brand_mark() -> Markup {
    html! {
        svg class="brand-mark" aria-hidden="true" focusable="false" viewBox="0 0 28 28" width="28" height="28" {
            rect class="frame" x="1" y="1" width="26" height="26" {}
            circle class="spot" cx="14" cy="14" r="4.5" {}
        }
    }
}

/// `title` is the page's own name ("Sources" → "Sources · Muse & Mingle"); empty
/// for the home page ("Muse & Mingle — What's on in London for creative people").
/// `main` brings its own full-width sections (see [`head_band`]).
fn page(status: StatusCode, title: &str, nav: Nav, main: Markup) -> Response {
    let item = |href: &str, label: &str, me: Nav| {
        html! {
            a href=(href) aria-current=[(nav == me).then_some("page")] { (label) }
        }
    };
    let doc = html! {
        (DOCTYPE)
        html lang="en-GB" {
            head {
                meta charset="utf-8";
                meta name="viewport" content="width=device-width, initial-scale=1";
                title {
                    @if title.is_empty() { (BRAND) " — " (TAGLINE) } @else { (title) " · " (BRAND) }
                }
                link rel="icon" href="/favicon.ico" sizes="32x32";
                link rel="icon" href="/favicon.svg" type="image/svg+xml";
                link rel="apple-touch-icon" href="/apple-touch-icon.png";
                @for url in FONT_URLS.iter() {
                    link rel="preload" href=(url) as="font" type="font/woff2" crossorigin;
                }
                link rel="stylesheet" href=(STYLESHEET_URL.as_str());
                script src=(APP_JS_URL.as_str()) defer {}
            }
            body {
                a class="skip" href="#main" { "Skip to content" }
                header class="site" {
                    div class="wrap-x" {
                        div class="brand-block" {
                            a class="brand" href="/" { (brand_mark()) span { (BRAND) } }
                            span class="tagline" { (TAGLINE) }
                        }
                        nav class="primary" aria-label="Site" {
                            (item("/", "Events", Nav::Events))
                            a href="/saved" aria-current=[(nav == Nav::Saved).then_some("page")] {
                                "Saved"
                                span class="count" data-saved-count hidden { "0" }
                            }
                            (item("/sources", "Sources", Nav::Sources))
                            (item("/about", "About", Nav::About))
                        }
                        div class="utility" {
                            a href="#suggest" { "+ Suggest a venue" }
                        }
                    }
                }
                main id="main" {
                    (main)
                    p id="save-status" class="vh" role="status" aria-live="polite" {}
                }
                footer class="site" {
                    div class="wrap-x" {
                        (suggest_form())
                        div class="footer-meta" {
                            p class="eyebrow" { span class="dot" {} "London" }
                            p class="small" {
                                (BRAND) " is a free, non-commercial guide. Data from venue sites and "
                                "ticketing APIs, credited and linked."
                            }
                            ul class="footer-links" {
                                li { a href="/about" { "About & our approach" } }
                                li { a href="/sources" { "Sources" } }
                                li { a href="/contact" { "Contact" } }
                                li { a href="/v1/events" { "JSON" } }
                            }
                        }
                    }
                }
            }
        }
    };
    let mut resp = (status, doc.into_string()).into_response();
    let h = resp.headers_mut();
    h.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/html; charset=utf-8"),
    );
    h.insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static(CSP),
    );
    h.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    h.insert(
        header::REFERRER_POLICY,
        HeaderValue::from_static("strict-origin-when-cross-origin"),
    );
    resp
}

/// The white heading band at the top of a page: a mono eyebrow, the `h1`
/// and an optional lede.
fn head_band(eyebrow: &str, title: Markup, lede: Option<Markup>) -> Markup {
    html! {
        section class="page-head" {
            div class="wrap-x" {
                p class="eyebrow" { span class="dot" {} (eyebrow) }
                h1 { (title) }
                @if let Some(l) = lede { p class="lede" { (l) } }
            }
        }
    }
}

/// A plain page: heading band, then `body` on the wall wash.
fn simple_page(
    status: StatusCode,
    title: &str,
    nav: Nav,
    eyebrow: &str,
    lede: Option<Markup>,
    body: Markup,
) -> Response {
    page(
        status,
        title,
        nav,
        html! {
            (head_band(eyebrow, html! { (title) }, lede))
            div class="page-body" { div class="wrap-x" { (body) } }
        },
    )
}

fn suggest_form() -> Markup {
    html! {
        form class="suggest" id="suggest" method="post" action="/suggest" {
            h2 { "Suggest a venue site" }
            p class="small" { "Know a gallery, studio or venue we should list? Send us its website." }
            label for="suggest-url" { "Website URL" }
            input id="suggest-url" name="url" type="url" required
                placeholder="https://" maxlength="2048" autocomplete="url";
            label for="suggest-note" { "Note (optional)" }
            textarea id="suggest-note" name="note" rows="2" maxlength=(MAX_NOTE_CHARS) {}
            button type="submit" { "Suggest" }
        }
    }
}

fn error_page(status: StatusCode, title: &str, message: &str) -> Response {
    simple_page(
        status,
        title,
        Nav::Other,
        &format!("Error {}", status.as_u16()),
        Some(html! { (message) }),
        html! { p { a class="arrow-link" href="/" { "← Back to events" } } },
    )
}

fn internal_error(e: impl std::fmt::Display) -> Response {
    tracing::error!(error = %e, "HTML page query failed");
    error_page(
        StatusCode::INTERNAL_SERVER_ERROR,
        "Something went wrong",
        "Please try again in a moment.",
    )
}

// ---------------------------------------------------------------- helpers

/// `s` if it is an absolute http(s) URL, so data can never become a
/// `javascript:` (or other scheme) link.
fn safe_link(s: Option<&str>) -> Option<String> {
    let u = url::Url::parse(s?).ok()?;
    matches!(u.scheme(), "http" | "https").then(|| u.to_string())
}

/// The source page to send visitors to: the first venue site (scraper)
/// listing with a link, else the first listing with a link.
fn primary_source(e: &EventJson) -> Option<(&SourceLinkJson, String)> {
    let linked = |s: &'_ SourceLinkJson| safe_link(s.url.as_deref());
    // The venue's own site first, then APIs, then aggregators.
    [SourceKind::Scraper, SourceKind::Api, SourceKind::Aggregator]
        .iter()
        .flat_map(|k| e.sources.iter().filter(move |s| s.kind == *k))
        .find_map(|s| linked(s).map(|u| (s, u)))
}

/// Our thumbnail with its visible credit ("Image: Barbican", linking to the
/// event's page on that source, never to the image file).
fn thumbnail_figure(e: &EventJson, class: &str) -> Markup {
    let (Some(src), Some(credit)) = (&e.thumbnail_url, &e.image_credit) else {
        return blank(e, class);
    };
    let (w, h) = e.thumbnail_size.unwrap_or((480, 270));
    html! {
        figure class=(class) {
            img src=(src) alt="" loading="lazy" decoding="async" width=(w) height=(h);
            figcaption class="credit" {
                "Image: "
                @if let Some(u) = safe_link(Some(&credit.url)) {
                    a href=(u) rel="noopener" { (credit.name) }
                } @else {
                    (credit.name)
                }
            }
        }
    }
}

/// The "Monograph Blank" shown where an event has no thumbnail: the same box,
/// decorative only (`aria-hidden`, no credit), and always the same for the
/// same event. It shows only real facts (category, venue, start day), never
/// anything that looks like catalogue or provenance data.
fn blank(e: &EventJson, class: &str) -> Markup {
    html! {
        div class={ "blank " (class) } aria-hidden="true" {
            span class="blank-top" {
                span { "No image" }
                span class="blank-kind" { (title_case(&e.category)) }
            }
            span class="blank-venue" { (e.venue_name.as_deref().unwrap_or("London")) }
            span class="blank-numeral" { (london(e.starts_at).format("%d").to_string()) }
        }
    }
}

fn london(t: DateTime<Utc>) -> DateTime<chrono_tz::Tz> {
    t.with_timezone(&London)
}

fn fmt_date(t: DateTime<Utc>) -> String {
    london(t).format("%a %-d %b %Y").to_string()
}

fn fmt_date_time(t: DateTime<Utc>) -> String {
    let l = london(t);
    if l.time() == NaiveTime::MIN {
        fmt_date(t)
    } else {
        l.format("%a %-d %b %Y, %H:%M").to_string()
    }
}

fn time_tag(t: DateTime<Utc>, text: String) -> Markup {
    html! { time datetime=(t.to_rfc3339()) { (text) } }
}

/// "Sat 3 Oct 2026, 18:30–20:00", "Sat 3 Oct 2026, all day", "Until Sun 3 Jan
/// 2027" (a multi-day event already running) or "Sat 3 Oct 2026 – Sun 3 Jan
/// 2027".
fn when(
    start: DateTime<Utc>,
    end: Option<DateTime<Utc>>,
    all_day: bool,
    now: DateTime<Utc>,
) -> Markup {
    match end {
        Some(end) if london(end).date_naive() != london(start).date_naive() && end > start => {
            if start <= now {
                html! { "Until " (time_tag(end, fmt_date(end))) }
            } else {
                html! { (time_tag(start, fmt_date(start))) " – " (time_tag(end, fmt_date(end))) }
            }
        }
        _ if all_day => html! { (time_tag(start, fmt_date(start))) ", all day" },
        Some(end) if end > start && london(start).time() != NaiveTime::MIN => html! {
            (time_tag(start, fmt_date_time(start))) "–" (london(end).format("%H:%M").to_string())
        },
        _ => time_tag(start, fmt_date_time(start)),
    }
}

/// "£5", "£12.50" (whole amounts without pence).
fn money(amount: Decimal, currency: Option<&str>) -> String {
    let amount = if amount.fract().is_zero() {
        amount.trunc().to_string()
    } else {
        format!("{amount:.2}")
    };
    match currency {
        None | Some("GBP") => format!("£{amount}"),
        Some("EUR") => format!("€{amount}"),
        Some("USD") => format!("${amount}"),
        Some(other) => format!("{amount} {other}"),
    }
}

fn price(e: &EventJson) -> Option<String> {
    if e.is_free {
        return Some("Free".into());
    }
    let c = e.currency.as_deref();
    match (e.price_min, e.price_max) {
        (Some(a), Some(b)) if a != b => Some(format!("{}–{}", money(a, c), money(b, c))),
        (Some(a), _) => Some(money(a, c)),
        (None, Some(b)) => Some(format!("Up to {}", money(b, c))),
        (None, None) => None,
    }
}

fn title_case(s: &str) -> String {
    let mut c = s.chars();
    c.next()
        .map(|f| f.to_uppercase().chain(c).collect())
        .unwrap_or_default()
}

/// Plain-text description as paragraphs: blank lines split paragraphs,
/// single newlines become line breaks. Everything is escaped by maud.
fn paragraphs(text: &str) -> Markup {
    let text = text.replace("\r\n", "\n");
    html! {
        @for para in text.split("\n\n").map(str::trim).filter(|p| !p.is_empty()) {
            p {
                @for (i, line) in para.lines().enumerate() {
                    @if i > 0 { br; }
                    (line)
                }
            }
        }
    }
}

// ---------------------------------------------------------------- home

/// The home-page filter form, as submitted.
#[derive(Debug, Default, Clone)]
struct Filters {
    from: String,
    to: String,
    category: String,
    free: bool,
    /// `when=` bucket (`evening`, …); empty = any time.
    when: String,
    /// `price_max=` amount; empty = any price.
    price_max: String,
    near: String,
    /// Source keys (repeatable), e.g. from a link on `/sources`.
    sources: Vec<String>,
    /// AI/default tag filters (one value each on the page; the API repeats).
    medium: String,
    format: String,
    good_for: String,
    cursor: String,
}

impl Filters {
    /// Last value wins; unknown parameters are ignored (it is a page).
    fn parse(raw: &str) -> Self {
        let mut f = Filters::default();
        for (k, v) in url::form_urlencoded::parse(raw.as_bytes()) {
            let v = v.trim().to_string();
            match k.as_ref() {
                "from" => f.from = v,
                "to" => f.to = v,
                "category" => f.category = v,
                "free" => f.free = matches!(v.as_str(), "true" | "on" | "1"),
                "when" => f.when = v,
                "price_max" => f.price_max = v,
                "near" => f.near = v,
                "medium" => f.medium = v,
                "format" => f.format = v,
                "good_for" => f.good_for = v,
                "source" if !v.is_empty() && !f.sources.contains(&v) => f.sources.push(v),
                "cursor" => f.cursor = v,
                _ => {}
            }
        }
        if f.from.is_empty() {
            f.from = Utc::now()
                .with_timezone(&London)
                .date_naive()
                .format("%Y-%m-%d")
                .to_string();
        }
        f
    }

    /// The page's own query string (for the "More" link), without cursor.
    fn page_query(&self) -> String {
        let mut s = url::form_urlencoded::Serializer::new(String::new());
        s.append_pair("from", &self.from);
        for (k, v) in [
            ("to", &self.to),
            ("category", &self.category),
            ("when", &self.when),
            ("price_max", &self.price_max),
            ("near", &self.near),
            ("medium", &self.medium),
            ("format", &self.format),
            ("good_for", &self.good_for),
        ] {
            if !v.is_empty() {
                s.append_pair(k, v);
            }
        }
        if self.free {
            s.append_pair("free", "true");
        }
        for src in &self.sources {
            s.append_pair("source", src);
        }
        s.finish()
    }

    /// `page_query` without one source (the chip's "clear" link).
    fn without_source(&self, key: &str) -> String {
        let rest = Filters {
            sources: self.sources.iter().filter(|s| *s != key).cloned().collect(),
            from: self.from.clone(),
            to: self.to.clone(),
            category: self.category.clone(),
            free: self.free,
            when: self.when.clone(),
            price_max: self.price_max.clone(),
            near: self.near.clone(),
            medium: self.medium.clone(),
            format: self.format.clone(),
            good_for: self.good_for.clone(),
            cursor: String::new(),
        };
        format!("/?{}", rest.page_query())
    }

    /// The equivalent `GET /v1/events` query.
    fn api_query(&self) -> Result<listing::EventQuery, String> {
        let mut s = url::form_urlencoded::Serializer::new(String::new());
        s.append_pair("from", &self.from);
        if !self.to.is_empty() {
            s.append_pair("to", &self.to);
        }
        if !self.category.is_empty() {
            s.append_pair("category", &self.category);
        }
        if self.free {
            s.append_pair("free", "true");
        }
        for (k, v) in [("when", &self.when), ("price_max", &self.price_max)] {
            if !v.is_empty() {
                s.append_pair(k, v);
            }
        }
        for src in &self.sources {
            s.append_pair("source", src);
        }
        for (k, v) in [
            ("medium", &self.medium),
            ("format", &self.format),
            ("good_for", &self.good_for),
        ] {
            if !v.is_empty() {
                s.append_pair(k, v);
            }
        }
        if !self.near.is_empty() {
            let area = AREAS
                .iter()
                .find(|a| a.key == self.near)
                .ok_or_else(|| format!("unknown area {:?}", self.near))?;
            s.append_pair("near", &format!("{},{}", area.lat, area.lng));
            s.append_pair("radius_km", &area.radius_km.to_string());
        }
        if !self.cursor.is_empty() {
            s.append_pair("cursor", &self.cursor);
        }
        s.append_pair("limit", &PAGE_SIZE.to_string());
        listing::parse_query(&s.finish())
    }
}

/// A tag filter select whose options show how many events each would give
/// (`counts` from `api::facets`; tags with none are left out unless chosen).
fn tag_select(
    id: &str,
    label: &str,
    any: &str,
    vocab: &[&str],
    chosen: &str,
    counts: Option<&serde_json::Value>,
) -> Markup {
    let count = |t: &str| {
        counts
            .and_then(|c| c.get(t))
            .and_then(serde_json::Value::as_i64)
    };
    html! {
        div class="field" {
            label for=(id) { (label) }
            select id=(id) name=(id) {
                option value="" selected[chosen.is_empty()] { (any) }
                @for t in vocab {
                    @let n = count(t);
                    @if counts.is_none() || n.is_some() || chosen == *t {
                        option value=(t) selected[chosen == *t] {
                            (title_case(label_of(t)))
                            @if let Some(n) = n { " (" (n) ")" }
                        }
                    }
                }
            }
        }
    }
}

fn label_of(tag: &str) -> &str {
    crate::enrich::output::label(tag)
}

/// `label`, followed by ` (n)` when the count is known.
fn with_count(label: &str, n: Option<i64>) -> Markup {
    html! {
        (label)
        @if let Some(n) = n { " (" (n) ")" }
    }
}

/// The `when=` options of the filter form, with their counts.
fn when_options(counts: Option<&CountsJson>) -> [(When, &'static str, Option<i64>); 4] {
    let n = |pick: fn(&CountsJson) -> i64| counts.map(pick);
    [
        (When::Evening, "Evenings", n(|c| c.when.evening)),
        (When::AfterWork, "After work", n(|c| c.when.after_work)),
        (When::Weekend, "Weekends", n(|c| c.when.weekend)),
        (When::Daytime, "Daytime", n(|c| c.when.daytime)),
    ]
}

/// `/?…` for these filters with `change` applied (and no cursor): the
/// date and type quick links keep every other filter.
fn link_with(f: &Filters, change: impl FnOnce(&mut Filters)) -> String {
    let mut g = f.clone();
    g.cursor.clear();
    change(&mut g);
    format!("/?{}", g.page_query())
}

/// Quick date ranges (London dates): `(label, from, to)`; `to` empty = open.
fn date_presets(today: NaiveDate) -> [(&'static str, String, String); 4] {
    let d = |n: i64| {
        (today + chrono::Duration::days(n))
            .format("%Y-%m-%d")
            .to_string()
    };
    let (sat, sun) = match today.weekday() {
        Weekday::Sat => (0, 1),
        Weekday::Sun => (0, 0),
        w => {
            let to_sat = 5 - i64::from(w.num_days_from_monday());
            (to_sat, to_sat + 1)
        }
    };
    [
        ("Any date", d(0), String::new()),
        ("Today", d(0), d(0)),
        ("This weekend", d(sat), d(sun)),
        ("Next 7 days", d(0), d(6)),
    ]
}

/// The filter bar: date and type quick links (plain links, so they work
/// without JavaScript), then the full form for everything else.
fn filter_form(
    f: &Filters,
    facets: Option<&serde_json::Value>,
    counts: Option<&CountsJson>,
) -> Markup {
    use crate::enrich::output::{FORMAT_TAGS, GOOD_FOR, MEDIUM_TAGS};
    let price = |pick: fn(&CountsJson) -> i64| counts.map(pick);
    let today = Utc::now().with_timezone(&London).date_naive();
    html! {
        section class="filter-bar" aria-label="Filters" {
            div class="wrap-x" {
                p class="chip-row" {
                    span class="label" { "Dates:" }
                    @for (label, from, to) in date_presets(today) {
                        @let on = f.from == from && f.to == to;
                        a class="pill" aria-current=[on.then_some("true")]
                            href=(link_with(f, |g| { g.from = from.clone(); g.to = to.clone(); })) { (label) }
                    }
                }
                p class="chip-row" {
                    span class="label" { "Type:" }
                    a class="pill" aria-current=[f.category.is_empty().then_some("true")]
                        href=(link_with(f, |g| g.category.clear())) { "All types" }
                    @for c in Category::ALL {
                        a class="pill" aria-current=[(f.category == c.as_str()).then_some("true")]
                            href=(link_with(f, |g| g.category = c.as_str().to_string())) {
                            (title_case(c.as_str()))
                        }
                    }
                }
                form class="filters" method="get" action="/" {
                    div class="field" {
                        label for="from" { "From" }
                        input id="from" name="from" type="date" value=(f.from);
                    }
                    div class="field" {
                        label for="to" { "To" }
                        input id="to" name="to" type="date" value=(f.to);
                    }
                    div class="field" {
                        label for="category" { "Type" }
                        select id="category" name="category" {
                            option value="" selected[f.category.is_empty()] { "Any" }
                            @for c in Category::ALL {
                                option value=(c.as_str()) selected[f.category == c.as_str()] {
                                    (title_case(c.as_str()))
                                }
                            }
                        }
                    }
                    div class="field" {
                        label for="when" { "When" }
                        select id="when" name="when" {
                            option value="" selected[f.when.is_empty()] { "Any time" }
                            @for (w, label, n) in when_options(counts) {
                                option value=(w.as_str()) selected[f.when == w.as_str()] {
                                    (with_count(label, n))
                                }
                            }
                        }
                    }
                    div class="field" {
                        label for="price_max" { "Max price" }
                        select id="price_max" name="price_max" {
                            option value="" selected[f.price_max.is_empty()] { "Any" }
                            option value="10" selected[f.price_max == "10"] {
                                (with_count("Under £10", price(|c| c.price.max_10)))
                            }
                            option value="20" selected[f.price_max == "20"] {
                                (with_count("Under £20", price(|c| c.price.max_20)))
                            }
                        }
                    }
                    div class="field" {
                        label for="near" { "Area" }
                        select id="near" name="near" {
                            option value="" selected[f.near.is_empty()] { "Anywhere in London" }
                            @for a in &AREAS {
                                option value=(a.key) selected[f.near == a.key] { (a.label) }
                            }
                        }
                    }
                    (tag_select("medium", "Medium", "Any medium", MEDIUM_TAGS, &f.medium, facets.and_then(|v| v.get("medium"))))
                    (tag_select("format", "Format", "Any format", FORMAT_TAGS, &f.format, facets.and_then(|v| v.get("format"))))
                    (tag_select("good_for", "Good for", "Anyone", GOOD_FOR, &f.good_for, facets.and_then(|v| v.get("good_for"))))
                    div class="field check" {
                        input id="free" name="free" type="checkbox" value="true" checked[f.free];
                        label for="free" { (with_count("Free only", price(|c| c.price.free))) }
                    }
                    @for src in &f.sources {
                        input type="hidden" name="source" value=(src);
                    }
                    div class="field actions" {
                        button type="submit" { "Show events" }
                        a href="/" { "Reset" }
                    }
                }
            }
        }
    }
}

/// Bookmark icon for the save toggle (decorative; the button has a text label).
fn bookmark() -> Markup {
    html! {
        svg class="save-icon" aria-hidden="true" focusable="false" viewBox="0 0 24 24" width="16" height="16" {
            path d="M6 3.5h12v17l-6-4.5-6 4.5z" {}
        }
    }
}

/// Save/unsave toggle. Rendered `hidden`; `/static/app.js` shows it and keeps
/// `aria-pressed` in sync with localStorage. The data attributes are the
/// snapshot kept with a save.
fn save_button(e: &EventJson) -> Markup {
    html! {
        button type="button" class="save" hidden aria-pressed="false"
            data-save-id=(e.id) data-title=(e.title)
            data-venue=(e.venue_name.as_deref().unwrap_or(""))
            data-starts=(e.starts_at.to_rfc3339())
            data-ends=(e.ends_at.map(|t| t.to_rfc3339()).unwrap_or_default())
            data-all-day=(if e.all_day { "true" } else { "false" }) {
            (bookmark())
            span class="save-label" { "Save" }
            span class="vh" { ": " (e.title) }
        }
    }
}

/// The "✨ AI" mark in front of AI-written text on cards.
fn ai_mark() -> Markup {
    html! { span class="ai-mark" { "\u{2728} AI" span class="vh" { "-written summary:" } } }
}

/// The card markup for the Saved page, filled in by `/static/app.js` (via
/// the `data-slot`s). Keep it in step with [`card`] and [`blank`].
fn card_template() -> Markup {
    html! {
        template id="card-template" {
            article class="card" {
                figure class="thumb" data-slot="figure" hidden {
                    img data-slot="image" alt="" loading="lazy" decoding="async" width="480" height="270";
                    figcaption class="credit" { "Image: " a data-slot="credit" rel="noopener" {} }
                }
                div class="blank thumb" data-slot="blank" aria-hidden="true" hidden {
                    span class="blank-top" {
                        span { "No image" }
                        span class="blank-kind" data-slot="blank-kind" {}
                    }
                    span class="blank-venue" data-slot="blank-venue" {}
                    span class="blank-numeral" data-slot="blank-numeral" {}
                }
                div class="card-body" {
                    p class="card-meta" {
                        span class="when" data-slot="when" {}
                        span class="badge price" data-slot="price" {}
                    }
                    h2 { a data-slot="title" href="/" {} }
                    p class="venue" data-slot="venue" {}
                    p class="tags" {
                        span class="badge" data-slot="category" {}
                        span class="badge gone" data-slot="gone" hidden { "No longer listed" }
                    }
                    div class="card-actions" {
                        p class="cta" data-slot="cta-wrap" hidden {
                            a class="button" data-slot="cta" rel="noopener" {}
                        }
                        button type="button" class="save" aria-pressed="true" data-save-id="" data-title="" {
                            (bookmark())
                            span class="save-label" { "Save" }
                            span class="vh" data-slot="save-title" {}
                        }
                    }
                    p class="links" {
                        a data-slot="details" href="/" { "Details" span class="vh" data-slot="details-title" {} }
                        span data-slot="sources" {}
                    }
                }
            }
        }
    }
}

fn card(e: &EventJson, now: DateTime<Utc>) -> Markup {
    let detail = format!("/events/{}", e.id);
    let primary = primary_source(e);
    html! {
        article class="card" {
            (thumbnail_figure(e, "thumb"))
            div class="card-body" {
                p class="card-meta" {
                    span class="when" { (when(e.starts_at, e.ends_at, e.all_day, now)) }
                    @if let Some(p) = price(e) {
                        span class={ "badge price" @if e.is_free { " free" } } { (p) }
                    }
                }
                h2 { a href=(detail) { (e.title) } }
                @if let Some(v) = &e.venue_name { p class="venue" { (v) } }
                @if let Some(o) = e.ai.as_ref().and_then(|a| a.one_liner.as_deref()) {
                    p class="one-liner" title="AI-written summary" { (ai_mark()) (o) }
                }
                p class="tags" {
                    span class="badge" { (title_case(&e.category)) }
                    @if e.is_opening == Some(true) {
                        " " span class="badge opening" { "Opening" }
                    }
                    @if let Some(d) = e.distance_km {
                        " " span class="badge distance" { (format!("{d:.1} km")) }
                    }
                }
                div class="card-actions" {
                    @if let Some((p, u)) = &primary {
                        p class="cta" {
                            a class="button" href=(u) rel="noopener" {
                                "See it on " (p.display_name) " →"
                                span class="vh" { ": " (e.title) }
                            }
                        }
                    }
                    (save_button(e))
                }
                p class="links" {
                    a href=(detail) { "Details" span class="vh" { ": " (e.title) } }
                    @for s in &e.sources {
                        @if let Some(u) = safe_link(s.url.as_deref()).filter(|u| primary.as_ref().is_none_or(|(_, pu)| pu != u)) {
                            " · " a href=(u) rel="noopener" { "also on " (s.display_name) }
                        }
                    }
                }
            }
        }
    }
}

/// Our three principles, in the band above the footer on the home page.
fn principles() -> Markup {
    html! {
        section class="principles" aria-label="How this guide works" {
            div class="wrap-x" {
                div {
                    p class="eyebrow" { "Principle 01" }
                    h2 { "Free and ad-free" }
                    p { "No ads, no sponsored listings, no ticket sales or affiliate links. Nobody pays to be listed." }
                }
                div {
                    p class="eyebrow" { "Principle 02" }
                    h2 { "We send you to the venue" }
                    p {
                        "Every event's main button goes to the page where we found it: the venue's own "
                        "site whenever we have it. We keep only the facts, a short excerpt and a small, "
                        "credited image."
                    }
                }
                div {
                    p class="eyebrow" { "Principle 03" }
                    h2 { "We respect venues' rules" }
                    p {
                        "Listings are gathered automatically from venues' websites and ticketing APIs, "
                        "following robots.txt and each site's terms. Some carry a short, labelled AI note. "
                        a href="/about" { "How we work" } "."
                    }
                }
            }
        }
    }
}

async fn home(State(state): State<AppState>, RawQuery(raw): RawQuery) -> Response {
    let filters = Filters::parse(raw.as_deref().unwrap_or(""));
    let source_names = if filters.sources.is_empty() {
        Vec::new()
    } else {
        match repo::source_names(&state.pool).await {
            Ok(n) => n,
            Err(e) => return internal_error(e),
        }
    };
    let query = filters.api_query();
    let facets = match &query {
        Ok(q) => match api::facets(&state.pool, q).await {
            Ok(f) => Some(f),
            Err(e) => return internal_error(e),
        },
        Err(_) => None,
    };
    let week = Utc::now().with_timezone(&London).iso_week().week();
    let heading = |counts: Option<&CountsJson>| {
        html! {
            section class="page-head" {
                div class="wrap-x hero-row" {
                    div {
                        p class="eyebrow" { span class="dot" {} "London // Week " (week) }
                        h1 { (TAGLINE) }
                        p class="lede" {
                            "Exhibitions, talks, workshops, expos and community events, gathered "
                            "automatically from venues' websites and ticketing APIs. No ads, no "
                            "sponsored listings."
                        }
                    }
                }
            }
            (filter_form(&filters, facets.as_ref(), counts))
        }
    };
    // With a price ceiling, say how many events were left out for having no known price.
    let hidden_unknown = |counts: &CountsJson| {
        let n = counts.price.unknown;
        html! {
            @if n > 0 && !filters.price_max.is_empty() {
                p class="results-status" {
                    @if n == 1 {
                        "1 event with an unknown price is not shown."
                    } @else {
                        (n) " events with an unknown price are not shown."
                    }
                }
            }
        }
    };
    let chips = html! {
        @if !filters.sources.is_empty() {
            p class="chips" aria-label="Active filters" {
                @for src in &filters.sources {
                    @let name = source_names.iter().find(|(k, _)| k == src).map_or_else(|| repo::display_name(src, None), |(_, n)| n.clone());
                    span class="chip" {
                        "From: " strong { (name) } " "
                        a href=(filters.without_source(src)) aria-label={ "Show events from all sources, not only " (name) } { "×" }
                    }
                }
            }
        }
    };
    let query = match query {
        Ok(q) => q,
        Err(msg) => {
            return page(
                StatusCode::BAD_REQUEST,
                "",
                Nav::Events,
                html! {
                    (heading(None))
                    section class="band results" {
                        div class="wrap-x" {
                            (chips)
                            p class="error" role="alert" { "Check the filters: " (msg) }
                        }
                    }
                },
            );
        }
    };
    let (events, next_cursor) = match api::event_page(&state.pool, &query).await {
        Ok(r) => r,
        Err(e) => return internal_error(e),
    };
    let counts = match api::event_counts(&state.pool, &query).await {
        Ok(c) => c,
        Err(e) => return internal_error(e),
    };
    let now = Utc::now();
    let more = next_cursor.map(|c| {
        let mut q = filters.page_query();
        q.push('&');
        q.push_str(
            &url::form_urlencoded::Serializer::new(String::new())
                .append_pair("cursor", &c)
                .finish(),
        );
        format!("/?{q}")
    });
    let order = if filters.near.is_empty() {
        "By start date"
    } else {
        "Nearest first"
    };
    page(
        StatusCode::OK,
        "",
        Nav::Events,
        html! {
            (heading(Some(&counts)))
            section class="band results" {
                div class="wrap-x" {
                    @if !filters.sources.is_empty() { div class="results-status" { (chips) } }
                    (hidden_unknown(&counts))
                    @if events.is_empty() {
                        p class="empty" { "No events match these filters." }
                        @if !filters.near.is_empty() {
                            p class="small" { "Area filters only include events with a known location." }
                        }
                    } @else {
                        p class="results-status" {
                            span {
                                strong { "Showing " (events.len()) @if events.len() == 1 { " event" } @else { " events" } }
                                @if more.is_some() { " // more below" }
                            }
                            span { (order) }
                        }
                        section class="cards" aria-label="Events" {
                            @for e in &events { (card(e, now)) }
                        }
                    }
                    @if let Some(href) = more {
                        p class="more" { a href=(href) rel="next" { "More events" } }
                    }
                }
            }
            @if filters.cursor.is_empty() { (principles()) }
        },
    )
}

// ---------------------------------------------------------------- saved

async fn saved() -> Response {
    page(
        StatusCode::OK,
        "Saved events",
        Nav::Saved,
        html! {
            (head_band(
                "Saved // this browser only",
                html! { "Saved events" },
                Some(html! {
                    "Events you save are kept only in this browser on this device — no account, "
                    "nothing stored on our server. Clearing your browser data removes them."
                }),
            ))
            section class="band results" {
                div class="wrap-x" {
                    noscript {
                        p class="error" { "Saving events needs JavaScript. Everything else on this site works without it." }
                    }
                    p id="saved-status" class="results-status" role="status" aria-live="polite" {}
                    p id="saved-empty" class="empty" hidden {
                        "Nothing saved yet. Use the Save button on any event, then come back here."
                    }
                    section id="saved-list" class="cards" aria-label="Saved events" {}
                    p class="saved-tools" { button id="export-ics" type="button" hidden { "Export saved as .ics" } }
                    (card_template())
                }
            }
        },
    )
}

// ---------------------------------------------------------------- detail

/// Where an event stands today: ("On now", "live"), ("Starts in 3 days",
/// "soon") or ("Ended", "past"), in London dates.
fn status_line(
    start: DateTime<Utc>,
    end: Option<DateTime<Utc>>,
    now: DateTime<Utc>,
) -> (String, &'static str) {
    let today = london(now).date_naive();
    let start_day = london(start).date_naive();
    let over = match end {
        Some(end) => end < now,
        None => start_day < today,
    };
    if over {
        return ("Ended".into(), "past");
    }
    if start <= now {
        // Without an end time we only know it is today, not that it's still on.
        return match end {
            Some(_) => ("On now".into(), "live"),
            None => ("Today".into(), "live"),
        };
    }
    let text = match (start_day - today).num_days() {
        0 => "Starts today".to_string(),
        1 => "Starts tomorrow".to_string(),
        n => format!("Starts in {n} days"),
    };
    (text, "soon")
}

async fn event_detail(State(state): State<AppState>, Path(id): Path<String>) -> Response {
    let not_found = || {
        error_page(
            StatusCode::NOT_FOUND,
            "Event not found",
            "This event does not exist or is no longer listed.",
        )
    };
    let Ok(id) = Uuid::parse_str(&id) else {
        return not_found();
    };
    let e = match api::event_by_id(&state.pool, id).await {
        Ok(Some(e)) => e,
        Ok(None) => return not_found(),
        Err(err) => return internal_error(err),
    };
    let now = Utc::now();
    let similar = api::similar_events(&state.pool, &e, now)
        .await
        .unwrap_or_else(|err| {
            tracing::error!(error = %err, "similar events query failed");
            Vec::new()
        });
    let map = match (e.lat, e.lng) {
        (Some(lat), Some(lng)) => Some(format!(
            "https://www.openstreetmap.org/?mlat={lat}&mlon={lng}#map=17/{lat}/{lng}"
        )),
        _ => None,
    };
    let primary = primary_source(&e);
    let (status, status_class) = status_line(e.starts_at, e.ends_at, now);
    let kinds: Vec<String> = std::iter::once(title_case(&e.category))
        .chain(e.medium_tags.iter().map(|t| title_case(label_of(t))))
        .collect();
    page(
        StatusCode::OK,
        &e.title,
        Nav::Events,
        html! {
            nav class="crumbs" aria-label="Breadcrumb" {
                div class="wrap-x" {
                    div class="trail" {
                        a href="/" { "← All events" }
                        span class="kind" { "[" (kinds.join(" // ")) "]" }
                    }
                    span class={ "status-live " (status_class) } {
                        @if status_class != "soon" { span class="dot" {} }
                        (status)
                    }
                }
            }
            article class="detail" {
                section class="page-head" {
                    div class="wrap-x" {
                        p class="eyebrow" {
                            span class="dot" {}
                            (title_case(&e.category))
                            @if let Some(v) = &e.venue_name { " // " (v) }
                        }
                        div class="detail-head" {
                            div {
                                h1 { (e.title) }
                                @if let Some(o) = e.ai.as_ref().and_then(|a| a.one_liner.as_deref()) {
                                    p class="lede one-liner" title="AI-written summary" { (ai_mark()) (o) }
                                }
                            }
                            div class="detail-actions" {
                                @if let Some((p, u)) = &primary {
                                    p class="cta" {
                                        a class="button" href=(u) rel="noopener" { "See it on " (p.display_name) " →" }
                                    }
                                }
                                (save_button(&e))
                            }
                        }
                        dl class="facts" {
                            div {
                                dt { "When" }
                                dd {
                                    (when(e.starts_at, e.ends_at, e.all_day, now))
                                    @if let Some(end) = e.ends_at {
                                        span class="sub" {
                                            "Starts " (time_tag(e.starts_at, fmt_date_time(e.starts_at)))
                                            br;
                                            "Ends " (time_tag(end, fmt_date_time(end)))
                                        }
                                    }
                                }
                            }
                            div {
                                dt { "Price" }
                                dd class=[e.is_free.then_some("free")] {
                                    (price(&e).unwrap_or_else(|| "Not listed".into()))
                                    span class="sub" { "Check the venue's page before you go" }
                                }
                            }
                            div {
                                dt { "Venue" }
                                dd {
                                    (e.venue_name.as_deref().unwrap_or("Not listed"))
                                    @if let Some(a) = &e.address { span class="sub" { (a) } }
                                }
                            }
                            @if let (Some(m), Some(lat), Some(lng)) = (&map, e.lat, e.lng) {
                                div {
                                    dt { "Map" }
                                    dd {
                                        a href=(m) rel="noopener noreferrer" { "Open in OpenStreetMap ↗" }
                                        span class="sub" { (format!("{lat:.4}, {lng:.4}")) }
                                    }
                                }
                            }
                        }
                        div class="hero-wrap" { (thumbnail_figure(&e, "hero")) }
                    }
                }
                                            br;
                                            "Ends " (time_tag(end, fmt_date_time(end)))
                                        }
                                    }
                                }
                            }
                            div {
                                dt { "Price" }
                                dd class=[e.is_free.then_some("free")] {
                                    (price(&e).unwrap_or_else(|| "Not listed".into()))
                                    span class="sub" { "Check the venue's page before you go" }
                                }
                            }
                            div {
                                dt { "Venue" }
                                dd {
                                    (e.venue_name.as_deref().unwrap_or("Not listed"))
                                    @if let Some(a) = &e.address { span class="sub" { (a) } }
                                }
                            }
                            @if let (Some(m), Some(lat), Some(lng)) = (&map, e.lat, e.lng) {
                                div {
                                    dt { "Map" }
                                    dd {
                                        a href=(m) rel="noopener noreferrer" { "Open in OpenStreetMap ↗" }
                                        span class="sub" { (format!("{lat:.4}, {lng:.4}")) }
                                    }
                                }
                            }
                        }
                        div class="hero-wrap" { (thumbnail_figure(&e, "hero")) }
                    }
                }
                div class="wrap-x" {
                    div class="detail-grid" {
                        div {
                            @if let Some(ai) = &e.ai {
                                @if let Some(w) = &ai.whats_cool {
                                    section class="ai-note" aria-labelledby="whats-cool-h" {
                                        h2 id="whats-cool-h" {
                                            "What's cool "
                                            a class="ai-label" href="/about#ai" { "\u{2728} AI note" }
                                        }
                                        p { (w) }
                                        p class="small" {
                                            "Written by AI from the listing"
                                            @if ai.grounding == "listing_plus_general_knowledge" {
                                                " and general knowledge about the people or venue named"
                                            }
                                            ". It is not the venue's description; check details on the venue's own page."
                                        }
                                    }
                                }
                            }
                            @if let Some(d) = e.description.as_deref().filter(|d| !d.trim().is_empty()) {
                                section class="description" aria-labelledby="description-h" {
                                    div class="section-title" {
                                        span class="sq" {}
                                        h2 id="description-h" { "From the listing" }
                                    }
                                    (paragraphs(d))
                                    @if let Some((p, u)) = &primary {
                                        p class="small" {
                                            "An excerpt. Read more "
                                            a href=(u) rel="noopener" { "on " (p.display_name) }
                                            "."
                                        }
                                    }
                                }
                            }
                            @if !e.medium_tags.is_empty() || !e.format_tags.is_empty() {
                                p class="tag-chips" aria-label="Medium and format" {
                                    @for t in &e.medium_tags {
                                        a class="tag-chip" href={ "/?medium=" (t) } { (title_case(label_of(t))) } " "
                                    }
                                    @for t in &e.format_tags {
                                        a class="tag-chip format" href={ "/?format=" (t) } { (title_case(label_of(t))) } " "
                                    }
                                }
                            }
                            section aria-labelledby="details-h" {
                                div class="section-title" {
                                    span class="sq" {}
                                    h2 id="details-h" { "Details" }
                                }
                                dl class="more-facts" {
                                    dt { "Category" } dd { (title_case(&e.category)) }
                                    @if !e.tags.is_empty() { dt { "Tags" } dd { (e.tags.join(", ")) } }
                                    @if e.is_opening == Some(true) { dt { "Opening" } dd { "Private view / opening event" } }
                                    @if !e.good_for.is_empty() {
                                        dt { "Good for" }
                                        dd { (e.good_for.iter().map(|t| label_of(t)).collect::<Vec<_>>().join(", ")) }
                                    }
                                    @if let Some(u) = safe_link(e.url.as_deref()) {
                                        dt { "Event page" } dd { a href=(u) rel="noopener" { (u) } }
                                    }
                                }
                            }
                        }
                        aside {
                            section class="panel" aria-labelledby="found-on-h" {
                                div class="panel-head" {
                                    h2 id="found-on-h" { span class="dot" {} "Found on (" (e.sources.len()) ")" }
                                }
                                p class="small" { "Where we found this listing. The source's page has the full, current details." }
                                ul class="sources" {
                                    @for s in &e.sources {
                                        li {
                                            span class="name" {
                                                @if let Some(u) = safe_link(s.url.as_deref()) {
                                                    a href=(u) rel="noopener" { (s.display_name) }
                                                } @else {
                                                    (s.display_name)
                                                }
                                            }
                                            span class="mono" {
                                                "First seen " (time_tag(s.first_seen_at, fmt_date(s.first_seen_at)))
                                                " · last seen " (time_tag(s.last_seen_at, fmt_date(s.last_seen_at)))
                                            }
                                        }
                                    }
                                }
                            }
                            @if !similar.is_empty() {
                                section class="panel similar" aria-labelledby="similar-h" {
                                    div class="panel-head" {
                                        h2 id="similar-h" { "More like this" }
                                    }
                                    ul {
                                        @for s in &similar {
                                            li {
                                                a href={ "/events/" (s.id) } { (s.title) }
                                                span class="mono" {
                                                    @if let Some(v) = &s.venue_name { (v) " · " }
                                                    (when(s.starts_at, s.ends_at, s.all_day, now))
                                                }
                                                @if !s.shared_tags.is_empty() {
                                                    span class="why" {
                                                        "Similar because: shared "
                                                        (s.shared_tags.iter().map(|t| label_of(t)).collect::<Vec<_>>().join(", "))
                                                    }
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        },
    )
}

// ---------------------------------------------------------------- sources

async fn sources(State(state): State<AppState>) -> Response {
    let sources = match api::source_values(&state).await {
        Ok(s) => s,
        Err(e) => return internal_error(e),
    };
    let refused = match repo::refused_sources(&state.pool).await {
        Ok(r) => r,
        Err(e) => return internal_error(e),
    };
    let text = |v: &Value| match v {
        Value::String(s) => s.clone(),
        Value::Null => String::new(),
        other => other.to_string(),
    };
    let body = html! {
            div class="table-wrap" {
                table class="sources-table" {
                    thead {
                        tr {
                            th scope="col" { "Source" }
                            th scope="col" { "Kind" }
                            th scope="col" { "Last run" }
                            th scope="col" { "Events found" }
                            th scope="col" { "Errors" }
                            th scope="col" { "Health" }
                        }
                    }
                    tbody {
                        // Retired (disabled) sources are listed only under
                        // "Sites we couldn't use" below.
                        @for s in sources.iter().filter(|s| s["enabled"] != Value::Bool(false)) {
                            @let run = &s["last_run"];
                            @let status = text(&s["status"]);
                            tr {
                                th scope="row" {
                                    @let key = text(&s["key"]);
                                    @let name = text(&s["display_name"]);
                                    a href={ "/?" (url::form_urlencoded::Serializer::new(String::new()).append_pair("source", &key).finish()) }
                                        aria-label={ "Events from " (name) } { (name) }
                                }
                                td { (text(&s["kind"])) }
                                td {
                                    @match run["started_at"].as_str().and_then(|t| t.parse::<DateTime<Utc>>().ok()) {
                                        Some(t) => (time_tag(t, fmt_date_time(t))),
                                        None => "Never",
                                    }
                                }
                                td { (text(&run["events_found"])) }
                                td { (text(&run["errors"])) }
                                td {
                                    span class={ "status status-" (status) } { (status) }
                                    // The issue link points into the private repo (#57).
                                    @if !s["issue_url"].is_null() {
                                        " " span class="small" { "(we're on it)" }
                                    }
                                    // Present once sources record skips (#25).
                                    @if let Some(reason) = s["skip"]["reason"].as_str() {
                                        br; span class="small" { "Skipped: " (reason) }
                                    }
                                }
                            }
                        }
                    }
                }
            }
            h2 id="refused" { "Sites we couldn't use" }
            p class="lede small" {
                "We checked these sites and decided not to collect their events, so they "
                "won't appear here. We don't retry them."
            }
            p class="small" {
                "Run a venue listed here, or want yours removed or corrected? "
                a href="/contact" { "Contact us" } "."
            }
            @if refused.is_empty() {
                p { "None so far." }
            } @else {
                div class="table-wrap" {
                    table {
                        thead {
                            tr {
                                th scope="col" { "Site" }
                                th scope="col" { "Reason" }
                                th scope="col" { "Checked" }
                            }
                        }
                        tbody {
                            @for r in &refused {
                                tr {
                                    th scope="row" {
                                        @if let Some(u) = safe_link(Some(&r.url)) {
                                            a href=(u) rel="noopener noreferrer" { (r.name) }
                                        } @else {
                                            (r.name)
                                        }
                                        br; span class="small" { (r.domain) }
                                    }
                                    td class="wrap" { (r.reason_text) }
                                    td {
                                        time datetime=(r.checked_on.format("%Y-%m-%d").to_string()) {
                                            (r.checked_on.format("%-d %b %Y").to_string())
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
    };
    simple_page(
        StatusCode::OK,
        "Sources",
        Nav::Sources,
        "Sources // venues and APIs",
        Some(html! {
            "Where events come from, and how the last run of each went. Listings are "
            "gathered automatically from these venues' websites and ticketing APIs."
        }),
        body,
    )
}

// ---------------------------------------------------------------- about

/// `GET /about`: what the site is for and how it treats venues and their
/// content. Every statement must stay true for the code (crawler rules in
/// `fetch.rs`, content policy in `repo.rs`/`thumbs.rs`, suggestions in
/// `suggestions.rs`, contact form in `contact.rs`, saved events in
/// `web.js`); update it when they change.
async fn about() -> Response {
    let rate_secs = crate::config::DEFAULT_RATE_LIMIT_MS as f64 / 1000.0;
    page(
        StatusCode::OK,
        "About & our approach",
        Nav::About,
        html! {
            article class="about" {
                (head_band(
                    "About // our approach",
                    html! { "About " (BRAND) " & our approach" },
                    Some(html! {
                        "A free, non-commercial guide that sends you to the venues. Here's how we "
                        "collect listings, use AI, treat venues' content and handle your data."
                    }),
                ))
                div class="page-body" { div class="wrap-x prose" {
                section id="objective" aria-labelledby="objective-h" {
                    h2 id="objective-h" { "What we're for" }
                    p {
                        (BRAND) " is a free, non-commercial, just-for-fun guide to exhibitions, "
                        "talks, workshops and community events in London for creative people."
                    }
                    p { "No ads, no ticket sales, no affiliate links. We want to send you to the venues, not keep you here." }
                }
                section id="how-we-collect" aria-labelledby="how-we-collect-h" {
                    h2 id="how-we-collect-h" { "How we collect listings" }
                    ul {
                        li { "We read public event listings on venue websites and ticketing APIs." }
                        li { "We check each site's robots.txt and obey it." }
                        li {
                            "Our crawler says who it is: it identifies itself as "
                            strong { (crate::fetch::ROBOTS_AGENT) } " with a link to this page."
                        }
                        li {
                            "We go slowly: by default one request every " (rate_secs)
                            " seconds per website, and slower where a site asks us to."
                        }
                        li { "We check each site on a schedule (usually once a day), not constantly." }
                        li {
                            "We never log in or get around blocks, CAPTCHAs or bot protection. "
                            "If a site blocks us, we stop and list it under "
                            a href="/sources#refused" { "Sites we couldn't use" } " with the reason."
                        }
                        li {
                            "We follow each site's terms. Where they restrict reuse, we keep only "
                            "the basic facts (title, dates, venue) and a link."
                        }
                        li {
                            "Collecting never uses AI: our crawler reads listings with ordinary, "
                            "predictable code. AI only adds tags and a short note afterwards, from "
                            "what we already keep ("
                            a href="#ai" { "how we use AI" } ")."
                        }
                    }
                }
                section id="ai" aria-labelledby="ai-h" {
                    h2 id="ai-h" { "How we use AI" }
                    ul {
                        li {
                            "After collecting, an AI language model tags each event (medium, format, "
                            "who it suits, whether it's an opening) and may write a short "
                            "\u{201c}What's cool\u{201d} note and a one-line summary."
                        }
                        li {
                            "It only sees facts we already store: title, venue, dates, category, the "
                            "listing's own labels, price, the short excerpt we keep (if any) and which "
                            "sites list the event. We never fetch a page for it, and for sites whose "
                            "terms limit reuse it gets just the basic facts."
                        }
                        li {
                            "Notes are labelled \u{201c}\u{2728} AI note\u{201d} and link here. They are our "
                            "AI's summary, not the venue's words, and can be wrong: check details on the "
                            "venue's own page. When a listing says too little, the AI writes nothing "
                            "rather than guess, and a note is withdrawn at our next update when the facts it was "
                            "written from change."
                        }
                        li {
                            "We reach the model through Requesty, an AI gateway, and only use models "
                            "whose provider keeps no copy of what we send (zero data retention). "
                            "Requesty itself may keep a log of our requests (event facts and the AI's "
                            "answers) in our account. To "
                            "find similar events we also turn each event's facts and tags into an "
                            "embedding (a list of numbers) with OpenAI's embedding model through the "
                            "same gateway; OpenAI may keep API inputs for up to 30 days for abuse "
                            "monitoring."
                        }
                        li {
                            "Only event information is ever sent: nothing about you, your searches, "
                            "saved events, suggestions or messages."
                        }
                        li {
                            "A note or tag about your venue is wrong? "
                            a href="/contact" { "Tell us" } " and we'll fix or remove it."
                        }
                    }
                }
                section id="for-venues" aria-labelledby="for-venues-h" {
                    h2 id="for-venues-h" { "For venues" }
                    p { "For each event we show:" }
                    ul {
                        li { "the facts: title, dates, venue, price and category;" }
                        li { "a short excerpt of the description, not the full text;" }
                        li {
                            "a small thumbnail, made once from your image and served from our own "
                            "server so we don't use your bandwidth. It's always credited "
                            "\u{201c}Image: <your venue>\u{201d} and linked to your own event page."
                        }
                    }
                    p {
                        "The main button on every event is \u{201c}See it on <your venue>\u{201d}. "
                        "Our links are ordinary links, so your analytics can see visits came from us."
                    }
                    p {
                        "If you'd rather we didn't list your events, or want something changed "
                        "(wrong details, or you'd prefer we didn't use your images), tell us and "
                        "we'll act on it. We remove a venue's listings within 7 days of a request. "
                        "Just " a href="/contact" { "use our contact form" } "."
                    }
                }
                section id="your-data" aria-labelledby="your-data-h" {
                    h2 id="your-data-h" { "Your data" }
                    ul {
                        li { "There are no accounts, and no tracking or analytics cookies." }
                        li {
                            "Saved events live only in your browser. We don't store them; the Saved "
                            "page just asks us for those events' current details, like any other page."
                        }
                        li {
                            "If you suggest a venue, we store the website address and your note, "
                            "plus a salted hash of your IP address (not the address itself) to stop spam. "
                            "The address and note go into our project's issue tracker so we can review them."
                        }
                        li {
                            "If you use the contact form, we keep what you send and the same salted IP hash; "
                            "your details go into our issue tracker so we can act on them. An email "
                            "address, if you give one, is kept only in our database, only used to reply, "
                            "and never put in the issue tracker."
                        }
                        li {
                            "Nothing about you is sent to AI services: the AI features only ever see "
                            "event listings (see " a href="#ai" { "How we use AI" } ")."
                        }
                        li { "Nothing is sold or shared for advertising." }
                    }
                }
                section id="contact" aria-labelledby="contact-h" {
                    h2 id="contact-h" { "Contact" }
                    p {
                        "Venue requests, corrections or anything else: "
                        a href="/contact" { "use our contact form" }
                        ". No account needed; leave an email address only if you'd like a reply."
                    }
                }
                p class="small" {
                    "Sources and credits: every venue and service we use, and the sites we "
                    "couldn't use, are listed on " a href="/sources" { "Sources" } "."
                }
                } }
            }
        },
    )
}

// ---------------------------------------------------------------- contact

/// Largest accepted `POST /contact` body (the fields' own limits are far
/// smaller; this just stops huge bodies early).
pub const CONTACT_BODY_LIMIT: usize = 16 * 1024;

fn contact_form(state: &AppState, f: &crate::contact::ContactForm, error: Option<&str>) -> Markup {
    use crate::contact::{MAX_DETAILS_CHARS, MAX_EMAIL_CHARS, RequestType, form_token};
    let token = form_token(state.suggestions.ip_salt(), Utc::now());
    html! {
        @if let Some(e) = error {
            p class="error" role="alert" { "Please check the form: " (e) "." }
        }
        form class="contact" method="post" action="/contact" {
            fieldset {
                legend { "What would you like us to do?" }
                @for t in RequestType::ALL {
                    div class="field check" {
                        input type="radio" id={ "type-" (t.as_str()) } name="request_type"
                            value=(t.as_str()) required checked[f.request_type == t.as_str()];
                        label for={ "type-" (t.as_str()) } { (t.label()) }
                    }
                }
            }
            label for="contact-url" { "Your venue or website address" }
            input id="contact-url" name="url" type="text" required maxlength="2048"
                inputmode="url" autocomplete="url" placeholder="https://" value=(f.url);
            label for="contact-details" { "Details" }
            p class="small" id="details-hint" {
                "Which events or pages, and what should change. Up to " (MAX_DETAILS_CHARS) " characters."
            }
            textarea id="contact-details" name="details" rows="6" required
                maxlength=(MAX_DETAILS_CHARS) aria-describedby="details-hint" { (f.details) }
            label for="contact-email" { "Your email (optional)" }
            p class="small" id="email-hint" {
                "Only if you'd like a reply. We only use it to reply to you, and it is never published."
            }
            input id="contact-email" name="reply_email" type="email" maxlength=(MAX_EMAIL_CHARS)
                autocomplete="email" aria-describedby="email-hint" value=(f.reply_email);
            // Honeypot: hidden from people (CSS and aria), tempting to bots.
            div class="hp" aria-hidden="true" {
                label for="contact-website" { "Leave this empty" }
                input id="contact-website" name="website" type="text" tabindex="-1" autocomplete="off";
            }
            input type="hidden" name="token" value=(token);
            button type="submit" { "Send" }
        }
    }
}

/// The Contact page around `body` (the form, or the outcome of sending it).
fn contact_page_with(status: StatusCode, heading: &str, body: Markup) -> Response {
    page(
        status,
        "Contact",
        Nav::Other,
        html! {
            (head_band(
                "Contact // venues and site owners",
                html! { (heading) },
                Some(html! {
                    "For venues and site owners: ask us to remove your listings, correct an event, "
                    "or anything else. We remove a venue's listings within 7 days of a request. "
                    "No account needed."
                }),
            ))
            div class="page-body" { div class="wrap-x" { (body) } }
        },
    )
}

async fn contact_page(State(state): State<AppState>) -> Response {
    contact_page_with(
        StatusCode::OK,
        "Contact us",
        contact_form(&state, &crate::contact::ContactForm::default(), None),
    )
}

/// Whether a browser says this form POST came from another site's page
/// (`Sec-Fetch-Site: cross-site`). Our forms are only ever submitted from
/// our own pages; non-browser clients send no such header.
fn cross_site(headers: &HeaderMap) -> bool {
    headers
        .get("sec-fetch-site")
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.eq_ignore_ascii_case("cross-site"))
}

fn cross_site_refused(title: &str) -> Response {
    error_page(
        StatusCode::FORBIDDEN,
        title,
        "Please use the form on this site.",
    )
}

async fn contact_submit(
    State(state): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    form: Result<Form<crate::contact::ContactForm>, FormRejection>,
) -> Response {
    if cross_site(&headers) {
        return cross_site_refused("Contact");
    }
    use crate::contact::Outcome as C;
    let thanks = || {
        contact_page_with(
            StatusCode::OK,
            "Thanks, we've got it",
            html! {
                div class="outcome" role="status" {
                    p { "We read every request. We remove a venue's listings within 7 days, and we'll reply if you left an email address." }
                }
                p { a class="arrow-link" href="/" { "← Back to events" } }
            },
        )
    };
    let Ok(Form(form)) = form else {
        return contact_page_with(
            StatusCode::BAD_REQUEST,
            "Contact us",
            contact_form(
                &state,
                &Default::default(),
                Some("some fields were missing"),
            ),
        );
    };
    let forwarded_for = headers
        .get_all("x-forwarded-for")
        .iter()
        .filter_map(|v| v.to_str().ok());
    let client = state.suggestions.client_ip(peer.ip(), forwarded_for);
    match crate::contact::submit(&state.pool, &state.suggestions, client, &form, Utc::now()).await {
        Ok(C::Received { .. } | C::Dropped) => thanks(),
        Ok(C::Invalid(e)) => contact_page_with(
            StatusCode::BAD_REQUEST,
            "Contact us",
            contact_form(&state, &form, Some(&e.to_string())),
        ),
        Ok(C::RateLimited) => contact_page_with(
            StatusCode::TOO_MANY_REQUESTS,
            "Contact us",
            html! {
                div class="outcome error" role="status" {
                    p { "You've sent several requests recently. Please try again later; we have the earlier ones." }
                }
            },
        ),
        Err(e) => internal_error(e),
    }
}

// ---------------------------------------------------------------- suggest

#[derive(Deserialize)]
struct SuggestForm {
    url: String,
    #[serde(default)]
    note: Option<String>,
}

async fn suggest(
    State(state): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    form: Result<Form<SuggestForm>, FormRejection>,
) -> Response {
    let title = "Suggest a venue site";
    if cross_site(&headers) {
        return cross_site_refused(title);
    }
    let respond = |status: StatusCode, message: Markup| {
        simple_page(
            status,
            title,
            Nav::Other,
            "Suggestions // venue sites",
            None,
            html! {
                div class={ "outcome" @if !status.is_success() { " error" } } role="status" { (message) }
                p { a class="arrow-link" href="/" { "← Back to events" } }
            },
        )
    };
    let Ok(Form(form)) = form else {
        return respond(
            StatusCode::BAD_REQUEST,
            html! { p { "Please enter the website's URL." } },
        );
    };
    let note = form
        .note
        .as_deref()
        .map(str::trim)
        .filter(|n| !n.is_empty());
    let outcome = api::submit_suggestion(&state, peer, &headers, form.url.trim(), note).await;
    match outcome {
        Ok(Outcome::Accepted { domain, issue }) => {
            if let Some(n) = issue {
                tracing::info!(%domain, issue = n, "suggestion filed");
            }
            respond(
                StatusCode::CREATED,
                html! {
                    p { "Thanks — we'll take a look at " strong { (domain) } "." }
                },
            )
        }
        Ok(Outcome::AlreadySuggested { domain }) => respond(
            StatusCode::OK,
            html! { p { strong { (domain) } " has already been suggested — thanks!" } },
        ),
        Ok(Outcome::AlreadyCovered { source }) => respond(
            StatusCode::CONFLICT,
            html! { p { "We already list that site (source " strong { (source) } ")." } },
        ),
        Ok(Outcome::Refused(r)) => respond(
            StatusCode::CONFLICT,
            html! {
                p { (crate::suggestions::refused_message(&r)) "." }
                p { a href="/sources#refused" { "Other sites we couldn't use" } }
            },
        ),
        Ok(Outcome::RateLimited { retry_after_secs }) => {
            let mut resp = respond(
                StatusCode::TOO_MANY_REQUESTS,
                html! {
                    p { "Too many suggestions from your network. Please try again in "
                        (retry_after_secs.div_ceil(60)) " minute(s)." }
                },
            );
            if let Ok(v) = HeaderValue::from_str(&retry_after_secs.to_string()) {
                resp.headers_mut().insert(header::RETRY_AFTER, v);
            }
            resp
        }
        Ok(Outcome::Invalid(e)) => respond(
            StatusCode::BAD_REQUEST,
            html! { p { "That doesn't look right: " (e.to_string()) "." } },
        ),
        Err(e) => {
            tracing::error!(error = %e, "suggestion failed");
            respond(
                StatusCode::INTERNAL_SERVER_ERROR,
                html! { p { "Something went wrong. Please try again later." } },
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn t(s: &str) -> DateTime<Utc> {
        s.parse().unwrap()
    }

    #[test]
    fn only_http_links() {
        assert_eq!(safe_link(Some("javascript:alert(1)")), None);
        assert_eq!(safe_link(Some("data:text/html,x")), None);
        assert_eq!(safe_link(Some("/relative")), None);
        assert!(safe_link(Some("http://example.org/a")).is_some());
    }

    #[test]
    fn when_formats_in_london_time() {
        let now = t("2026-10-01T12:00:00Z");
        // BST: 17:00Z is 18:00 London.
        let one_off = when(
            t("2026-10-03T17:00:00Z"),
            Some(t("2026-10-03T19:00:00Z")),
            false,
            now,
        );
        assert!(
            one_off.0.contains(">Sat 3 Oct 2026, 18:00</time>–20:00"),
            "{}",
            one_off.0
        );
        let running = when(
            t("2026-09-01T00:00:00Z"),
            Some(t("2027-01-03T00:00:00Z")),
            false,
            now,
        );
        assert!(
            running.0.starts_with("Until <time") && running.0.contains("Sun 3 Jan 2027"),
            "{}",
            running.0
        );
        let future = when(
            t("2026-11-01T00:00:00Z"),
            Some(t("2027-01-03T00:00:00Z")),
            false,
            now,
        );
        assert!(
            future.0.contains("Sun 1 Nov 2026</time> – <time"),
            "{}",
            future.0
        );
        let midnight = Utc.with_ymd_and_hms(2026, 10, 2, 23, 0, 0).unwrap(); // 00:00 BST
        assert!(
            when(midnight, None, false, now)
                .0
                .ends_with(">Sat 3 Oct 2026</time>")
        );
    }

    #[test]
    fn all_day_events_show_dates_only() {
        let now = t("2026-10-01T12:00:00Z");
        let midnight = t("2026-10-02T23:00:00Z"); // 00:00 BST
        let one_day = when(midnight, None, true, now);
        assert!(
            one_day.0.ends_with(">Sat 3 Oct 2026</time>, all day"),
            "{}",
            one_day.0
        );
        let range = when(midnight, Some(t("2026-10-10T23:00:00Z")), true, now);
        assert!(
            range.0.contains("Sat 3 Oct 2026</time> – <time") && !range.0.contains("all day"),
            "{}",
            range.0
        );
    }

    #[test]
    fn description_paragraphs_are_escaped() {
        let m = paragraphs("One <b>\r\nline two\n\n\nPara & two");
        assert_eq!(m.0, "<p>One &lt;b&gt;<br>line two</p><p>Para &amp; two</p>");
    }

    #[test]
    fn date_presets_are_london_ranges() {
        let d = |s: &str| NaiveDate::parse_from_str(s, "%Y-%m-%d").unwrap();
        let weekend = |day: &str| {
            let p = date_presets(d(day));
            assert_eq!(p[2].0, "This weekend");
            (p[2].1.clone(), p[2].2.clone())
        };
        // Wednesday → the coming Saturday and Sunday; Saturday → today and
        // tomorrow; Sunday → today only.
        assert_eq!(
            weekend("2026-09-23"),
            ("2026-09-26".into(), "2026-09-27".into())
        );
        assert_eq!(
            weekend("2026-09-26"),
            ("2026-09-26".into(), "2026-09-27".into())
        );
        assert_eq!(
            weekend("2026-09-27"),
            ("2026-09-27".into(), "2026-09-27".into())
        );
        let p = date_presets(d("2026-09-23"));
        assert_eq!((p[0].1.as_str(), p[0].2.as_str()), ("2026-09-23", ""));
        assert_eq!(
            (p[3].1.as_str(), p[3].2.as_str()),
            ("2026-09-23", "2026-09-29")
        );
    }

    #[test]
    fn status_line_says_where_an_event_stands() {
        let now = t("2026-10-01T12:00:00Z");
        let s = |a: &str, b: Option<&str>| status_line(t(a), b.map(t), now);
        assert_eq!(
            s("2026-09-01T00:00:00Z", Some("2027-01-03T00:00:00Z")).1,
            "live"
        );
        assert_eq!(
            s("2026-09-01T00:00:00Z", Some("2026-09-30T00:00:00Z")).0,
            "Ended"
        );
        assert_eq!(s("2026-10-01T18:00:00Z", None).0, "Starts today");
        assert_eq!(s("2026-10-01T10:00:00Z", None).0, "Today");
        assert_eq!(s("2026-10-02T18:00:00Z", None).0, "Starts tomorrow");
        assert_eq!(s("2026-10-05T18:00:00Z", None).0, "Starts in 4 days");
    }

    #[test]
    fn filters_default_from_and_map_areas() {
        let f = Filters::parse("category=&near=kings-cross&free=on&bogus=1");
        assert!(!f.from.is_empty());
        let q = f.api_query().unwrap();
        assert!(q.filter.free_only);
        assert!(matches!(q.order, listing::EventOrder::ByDistance { .. }));
        assert!(f.page_query().contains("near=kings-cross"));
        assert!(Filters::parse("near=mars").api_query().is_err());
        assert!(Filters::parse("category=concert").api_query().is_err());
    }

    #[test]
    fn filters_pass_when_and_price_max_through() {
        let f = Filters::parse("when=evening&price_max=10");
        let q = f.api_query().unwrap();
        assert_eq!(q.filter.when, Some(When::Evening));
        assert_eq!(q.filter.price_max, Some(Decimal::from(10)));
        let page = f.page_query();
        assert!(
            page.contains("when=evening") && page.contains("price_max=10"),
            "{page}"
        );
        assert!(f.without_source("x").contains("when=evening"));
        assert!(Filters::parse("when=night").api_query().is_err());
    }
}
