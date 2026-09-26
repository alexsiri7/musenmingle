//! Human-facing HTML pages, server-rendered by the API process itself:
//! `GET /` (filterable upcoming events), `GET /events/{id}`, `GET /sources`
//! and `POST /suggest` (the "Suggest a venue site" form).
//!
//! Templating is `maud`: templates are Rust code checked at compile time, and
//! every interpolated value is HTML-escaped unless explicitly wrapped in
//! `PreEscaped` (which this module never does with data). Pages use no
//! JavaScript and no third-party assets; the stylesheet is served from
//! `/static/style.css`, so the Content-Security-Policy needs no
//! `'unsafe-inline'`. Everything shown comes from the same helpers as the
//! JSON API (`api::event_page`, `api::event_by_id`, `api::source_values`,
//! `api::submit_suggestion`), and filters go through `listing::parse_query`,
//! so validation is identical.

use std::net::SocketAddr;

use axum::Router;
use axum::extract::rejection::FormRejection;
use axum::extract::{ConnectInfo, Form, Path, RawQuery, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use chrono::{DateTime, NaiveTime, Utc};
use chrono_tz::Europe::London;
use maud::{DOCTYPE, Markup, html};
use rust_decimal::Decimal;
use serde::Deserialize;
use serde_json::Value;
use uuid::Uuid;

use crate::api::{self, AppState, EventJson};
use crate::listing;
use crate::model::Category;
use crate::suggestions::{MAX_NOTE_CHARS, Outcome};

/// Content-Security-Policy for every HTML response.
pub const CSP: &str = "default-src 'self'; img-src https: data:; style-src 'self'; \
     form-action 'self'; frame-ancestors 'none'; base-uri 'none'";

/// Site name shown in page titles and the header (the product name; the
/// project/crate stays `thaleia`).
pub const BRAND: &str = "LetsArt";

const TAGLINE: &str = "What's on in London for creative people";

/// Events per page on `/`.
pub const PAGE_SIZE: i64 = 24;

const STYLESHEET: &str = include_str!("web.css");

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
        .route("/suggest", post(suggest))
        .route("/static/style.css", get(stylesheet))
}

async fn stylesheet() -> Response {
    (
        [
            (header::CONTENT_TYPE, "text/css; charset=utf-8"),
            (header::CACHE_CONTROL, "public, max-age=3600"),
        ],
        STYLESHEET,
    )
        .into_response()
}

// ---------------------------------------------------------------- layout

/// `title` is the page's own name ("Sources" → "Sources · LetsArt"); empty
/// for the home page ("LetsArt — What's on in London for creative people").
fn page(status: StatusCode, title: &str, main: Markup) -> Response {
    let doc = html! {
        (DOCTYPE)
        html lang="en-GB" {
            head {
                meta charset="utf-8";
                meta name="viewport" content="width=device-width, initial-scale=1";
                title {
                    @if title.is_empty() { (BRAND) " — " (TAGLINE) } @else { (title) " · " (BRAND) }
                }
                link rel="stylesheet" href="/static/style.css";
            }
            body {
                a class="skip" href="#main" { "Skip to content" }
                header class="site" {
                    a class="brand" href="/" { (BRAND) }
                    nav aria-label="Site" {
                        a href="/" { "Events" }
                        a href="/sources" { "Sources" }
                    }
                }
                main id="main" { (main) }
                footer class="site" {
                    (suggest_form())
                    p class="small" {
                        "Data from venue sites and ticketing APIs. Also available as "
                        a href="/v1/events" { "JSON" } "."
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

fn suggest_form() -> Markup {
    html! {
        form class="suggest" method="post" action="/suggest" {
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
    page(
        status,
        title,
        html! {
            h1 { (title) }
            p { (message) }
            p { a href="/" { "Back to events" } }
        },
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

/// `s` if it is an https URL (the CSP only allows https images).
fn safe_image(s: Option<&str>) -> Option<String> {
    safe_link(s).filter(|u| u.starts_with("https://"))
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

/// "Sat 3 Oct 2026, 18:30–20:00", "Until Sun 3 Jan 2027" (a multi-day event
/// already running) or "Sat 3 Oct 2026 – Sun 3 Jan 2027".
fn when(start: DateTime<Utc>, end: Option<DateTime<Utc>>, now: DateTime<Utc>) -> Markup {
    match end {
        Some(end) if london(end).date_naive() != london(start).date_naive() && end > start => {
            if start <= now {
                html! { "Until " (time_tag(end, fmt_date(end))) }
            } else {
                html! { (time_tag(start, fmt_date(start))) " – " (time_tag(end, fmt_date(end))) }
            }
        }
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
#[derive(Debug, Default)]
struct Filters {
    from: String,
    to: String,
    category: String,
    free: bool,
    near: String,
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
                "near" => f.near = v,
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
            ("near", &self.near),
        ] {
            if !v.is_empty() {
                s.append_pair(k, v);
            }
        }
        if self.free {
            s.append_pair("free", "true");
        }
        s.finish()
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

fn filter_form(f: &Filters) -> Markup {
    html! {
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
                label for="category" { "Category" }
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
                label for="near" { "Near" }
                select id="near" name="near" {
                    option value="" selected[f.near.is_empty()] { "Anywhere in London" }
                    @for a in &AREAS {
                        option value=(a.key) selected[f.near == a.key] { (a.label) }
                    }
                }
            }
            div class="field check" {
                input id="free" name="free" type="checkbox" value="true" checked[f.free];
                label for="free" { "Free only" }
            }
            div class="field" {
                button type="submit" { "Show events" }
                " "
                a href="/" { "Reset" }
            }
        }
    }
}

fn card(e: &EventJson, now: DateTime<Utc>) -> Markup {
    let detail = format!("/events/{}", e.id);
    html! {
        article class="card" {
            @if let Some(img) = safe_image(e.image_url.as_deref()) {
                img src=(img) alt="" loading="lazy" decoding="async" width="320" height="180";
            }
            div class="card-body" {
                h2 { a href=(detail) { (e.title) } }
                p class="when" { (when(e.starts_at, e.ends_at, now)) }
                @if let Some(v) = &e.venue_name { p class="venue" { (v) } }
                p class="tags" {
                    span class="badge" { (title_case(&e.category)) }
                    @if let Some(p) = price(e) {
                        " " span class={ "badge" @if e.is_free { " free" } } { (p) }
                    }
                    @if let Some(d) = e.distance_km {
                        " " span class="badge" { (format!("{d:.1} km")) }
                    }
                }
                p class="links" {
                    a href=(detail) { "Details" span class="vh" { ": " (e.title) } }
                    @for s in &e.sources {
                        @if let Some(u) = safe_link(s.url.as_deref()) {
                            " · " a href=(u) rel="noopener noreferrer" { "on " (s.source) }
                        }
                    }
                }
            }
        }
    }
}

async fn home(State(state): State<AppState>, RawQuery(raw): RawQuery) -> Response {
    let filters = Filters::parse(raw.as_deref().unwrap_or(""));
    let heading = html! {
        h1 { (TAGLINE) }
        p class="lede" {
            "Exhibitions, talks, workshops, expos and community events, gathered from venue sites."
        }
        (filter_form(&filters))
    };
    let query = match filters.api_query() {
        Ok(q) => q,
        Err(msg) => {
            return page(
                StatusCode::BAD_REQUEST,
                "",
                html! { (heading) p class="error" role="alert" { "Check the filters: " (msg) } },
            );
        }
    };
    let (events, next_cursor) = match api::event_page(&state.pool, &query).await {
        Ok(r) => r,
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
    page(
        StatusCode::OK,
        "",
        html! {
            (heading)
            @if events.is_empty() {
                p class="empty" { "No events match these filters." }
                @if !filters.near.is_empty() {
                    p class="small" { "Area filters only include events with a known location." }
                }
            } @else {
                section class="cards" aria-label="Events" {
                    @for e in &events { (card(e, now)) }
                }
            }
            @if let Some(href) = more {
                p class="more" { a href=(href) rel="next" { "More events" } }
            }
        },
    )
}

// ---------------------------------------------------------------- detail

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
    let map = match (e.lat, e.lng) {
        (Some(lat), Some(lng)) => Some(format!(
            "https://www.openstreetmap.org/?mlat={lat}&mlon={lng}#map=17/{lat}/{lng}"
        )),
        _ => None,
    };
    page(
        StatusCode::OK,
        &e.title,
        html! {
            p class="small" { a href="/" { "← All events" } }
            article class="detail" {
                h1 { (e.title) }
                @if let Some(img) = safe_image(e.image_url.as_deref()) {
                    img class="hero" src=(img) alt="" loading="lazy" decoding="async";
                }
                dl {
                    dt { "When" } dd { (when(e.starts_at, e.ends_at, now)) }
                    @if e.ends_at.is_some() {
                        dt { "Starts" } dd { (time_tag(e.starts_at, fmt_date_time(e.starts_at))) }
                    }
                    @if let Some(end) = e.ends_at {
                        dt { "Ends" } dd { (time_tag(end, fmt_date_time(end))) }
                    }
                    @if let Some(v) = &e.venue_name { dt { "Venue" } dd { (v) } }
                    @if let Some(a) = &e.address { dt { "Address" } dd { (a) } }
                    @if let Some(m) = &map {
                        dt { "Map" } dd { a href=(m) rel="noopener noreferrer" { "Open in OpenStreetMap" } }
                    }
                    dt { "Category" } dd { (title_case(&e.category)) }
                    dt { "Price" } dd { (price(&e).unwrap_or_else(|| "Not listed".into())) }
                    @if !e.tags.is_empty() { dt { "Tags" } dd { (e.tags.join(", ")) } }
                    @if let Some(u) = safe_link(e.url.as_deref()) {
                        dt { "Event page" } dd { a href=(u) rel="noopener noreferrer" { (u) } }
                    }
                }
                @if let Some(d) = e.description.as_deref().filter(|d| !d.trim().is_empty()) {
                    section class="description" aria-label="Description" { (paragraphs(d)) }
                }
                h2 { "Found on" }
                ul class="sources" {
                    @for s in &e.sources {
                        li {
                            @if let Some(u) = safe_link(s.url.as_deref()) {
                                a href=(u) rel="noopener noreferrer" { (s.source) }
                            } @else {
                                (s.source)
                            }
                            span class="small" {
                                " — first seen " (time_tag(s.first_seen_at, fmt_date(s.first_seen_at)))
                                ", last seen " (time_tag(s.last_seen_at, fmt_date(s.last_seen_at)))
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
    let text = |v: &Value| match v {
        Value::String(s) => s.clone(),
        Value::Null => String::new(),
        other => other.to_string(),
    };
    page(
        StatusCode::OK,
        "Sources",
        html! {
            h1 { "Sources" }
            p class="lede" { "Where events come from, and how the last run of each went." }
            div class="table-wrap" {
                table {
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
                        @for s in &sources {
                            @let run = &s["last_run"];
                            @let status = text(&s["status"]);
                            tr {
                                th scope="row" {
                                    (text(&s["key"]))
                                    @if s["enabled"] == Value::Bool(false) { " (disabled)" }
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
                                    @if let Some(u) = safe_link(s["issue_url"].as_str()) {
                                        " " a href=(u) rel="noopener noreferrer" { "issue" }
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
        },
    )
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
    let respond = |status: StatusCode, message: Markup| {
        page(
            status,
            title,
            html! {
                h1 { (title) }
                div class={ "outcome" @if !status.is_success() { " error" } } role="status" { (message) }
                p { a href="/" { "Back to events" } }
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
            let issue_url =
                issue.map(|n| format!("https://github.com/{}/issues/{n}", state.github_repo));
            respond(
                StatusCode::CREATED,
                html! {
                    p { "Thanks! We'll look at adding " strong { (domain) } "." }
                    @if let (Some(n), Some(u)) = (issue, issue_url) {
                        p { "Tracked as " a href=(u) rel="noopener noreferrer" { "issue #" (n) } "." }
                    } @else {
                        p { "It is queued for review." }
                    }
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
    fn only_http_links_and_https_images() {
        assert_eq!(safe_link(Some("javascript:alert(1)")), None);
        assert_eq!(safe_link(Some("data:text/html,x")), None);
        assert_eq!(safe_link(Some("/relative")), None);
        assert!(safe_link(Some("http://example.org/a")).is_some());
        assert_eq!(safe_image(Some("http://example.org/a.jpg")), None);
        assert!(safe_image(Some("https://example.org/a.jpg")).is_some());
    }

    #[test]
    fn when_formats_in_london_time() {
        let now = t("2026-10-01T12:00:00Z");
        // BST: 17:00Z is 18:00 London.
        let one_off = when(
            t("2026-10-03T17:00:00Z"),
            Some(t("2026-10-03T19:00:00Z")),
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
            now,
        );
        assert!(
            future.0.contains("Sun 1 Nov 2026</time> – <time"),
            "{}",
            future.0
        );
        let midnight = Utc.with_ymd_and_hms(2026, 10, 2, 23, 0, 0).unwrap(); // 00:00 BST
        assert!(
            when(midnight, None, now)
                .0
                .contains(">Sat 3 Oct 2026</time>")
        );
    }

    #[test]
    fn description_paragraphs_are_escaped() {
        let m = paragraphs("One <b>\r\nline two\n\n\nPara & two");
        assert_eq!(m.0, "<p>One &lt;b&gt;<br>line two</p><p>Para &amp; two</p>");
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
}
