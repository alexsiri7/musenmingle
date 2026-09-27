//! `GET /calendar` (month / week / agenda views of every listed event),
//! `GET /calendar.ics` (the same filters as a subscribable iCalendar feed)
//! and `GET /saved/calendar` (the visitor's saved events in the same
//! layout, filled in by `/static/app.js` from this browser's storage).
//!
//! Placement rules (long-running events in the "ongoing" strip, markers on
//! their first and last day) are in `crate::calendar`; the iCalendar writer
//! is `crate::ics`. The grid is one server-rendered list of days: CSS lays
//! it out as a 7-column grid on wide screens and as an agenda (days with
//! events only) on narrow ones or at large text sizes.

use axum::extract::{RawQuery, State};
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use chrono::{DateTime, Duration, NaiveDate, Utc};
use maud::{Markup, html};

use super::{
    AREAS, AppState, BRAND, EventJson, Filters, Nav, internal_error, page, primary_source,
    save_button, title_case,
};
use crate::api;
use crate::calendar::{self, Mark, Range, View};
use crate::ics;
use crate::model::Category;

/// The public origin, for the feed's `webcal://` link and event links in
/// the feed. A constant rather than the request's `Host` header, so a
/// forged header can never change a published link.
pub const SITE_ORIGIN: &str = "https://musenmingle.interstellarai.net";

/// Most events one calendar view loads (a month has ~100 today); a notice
/// says so if a view has more.
pub const VIEW_CAP: i64 = 600;

/// Entries a month-grid cell shows before "+N more" (keep in step with
/// `.cal-ev:nth-child(n + 5)` in `web.css`).
pub const MONTH_CELL_MAX: usize = 4;

/// Most events in the `.ics` feed.
pub const FEED_CAP: i64 = 1000;

/// Days of upcoming events in the feed (from today, London).
pub const FEED_DAYS: i64 = 90;

/// The filters the calendar and feed take (a subset of the listing's).
#[derive(Debug, Default, Clone, PartialEq)]
pub(super) struct CalFilters {
    category: String,
    free: bool,
    near: String,
    sources: Vec<String>,
}

impl CalFilters {
    /// Filters plus `view=` and `date=` (last value wins; unknown keys ignored).
    fn parse(raw: &str) -> (Self, View, Option<NaiveDate>) {
        let f = Filters::parse(raw);
        let (mut view, mut date) = (View::Month, None);
        for (k, v) in url::form_urlencoded::parse(raw.as_bytes()) {
            match k.as_ref() {
                "view" => view = View::parse(v.trim()),
                "date" => date = calendar::parse_date(&v),
                _ => {}
            }
        }
        let cal = CalFilters {
            category: f.category,
            free: f.free,
            near: f.near,
            sources: f.sources,
        };
        (cal, view, date)
    }

    /// Query-string pairs of the filters (no view or date).
    fn pairs(&self) -> Vec<(&'static str, String)> {
        let mut out = Vec::new();
        if !self.category.is_empty() {
            out.push(("category", self.category.clone()));
        }
        if self.free {
            out.push(("free", "true".to_string()));
        }
        if !self.near.is_empty() {
            out.push(("near", self.near.clone()));
        }
        for s in &self.sources {
            out.push(("source", s.clone()));
        }
        out
    }

    fn query(&self, lead: &[(&str, String)]) -> String {
        let mut s = url::form_urlencoded::Serializer::new(String::new());
        for (k, v) in lead.iter().chain(self.pairs().iter()) {
            s.append_pair(k, v);
        }
        s.finish()
    }

    /// `/calendar?…` for a view and date with these filters.
    fn view_link(&self, view: View, date: NaiveDate) -> String {
        let q = self.query(&[
            ("view", view.as_str().to_string()),
            ("date", date.format("%Y-%m-%d").to_string()),
        ]);
        format!("/calendar?{q}")
    }

    /// The listing (`/`) for one day with these filters.
    fn day_link(&self, day: NaiveDate) -> String {
        let d = day.format("%Y-%m-%d").to_string();
        format!("/?{}", self.query(&[("from", d.clone()), ("to", d)]))
    }

    /// `/calendar.ics?…` (relative; the page also shows the absolute URL).
    fn feed_path(&self) -> String {
        let q = self.query(&[]);
        if q.is_empty() {
            "/calendar.ics".to_string()
        } else {
            format!("/calendar.ics?{q}")
        }
    }

    /// The listing query for London dates `from..=to`, fetching `cap` rows.
    fn listing_query(
        &self,
        from: NaiveDate,
        to: NaiveDate,
        cap: i64,
    ) -> Result<crate::listing::EventQuery, String> {
        let f = Filters {
            from: from.format("%Y-%m-%d").to_string(),
            to: to.format("%Y-%m-%d").to_string(),
            category: self.category.clone(),
            free: self.free,
            near: self.near.clone(),
            sources: self.sources.clone(),
            ..Filters::default()
        };
        let mut q = f.api_query()?;
        // parse_query caps page sizes for the API; a calendar needs the
        // whole range (one extra row tells us it was cut short).
        q.limit = cap;
        Ok(q)
    }
}

/// Events overlapping London dates `from..=to`, ordered by start, and
/// whether there were more than `cap`.
async fn load(
    state: &AppState,
    f: &CalFilters,
    from: NaiveDate,
    to: NaiveDate,
    cap: i64,
) -> Result<Result<(Vec<EventJson>, bool), String>, sqlx::Error> {
    let query = match f.listing_query(from, to, cap) {
        Ok(q) => q,
        Err(e) => return Ok(Err(e)),
    };
    let (mut events, next) = api::event_page(&state.pool, &query).await?;
    // With an area the page is ordered by distance.
    events.sort_by_key(|e| (e.starts_at, e.id));
    Ok(Ok((events, next.is_some())))
}

fn today() -> NaiveDate {
    calendar::london_date(Utc::now())
}

fn iso(d: NaiveDate) -> String {
    d.format("%Y-%m-%d").to_string()
}

/// "18:30" for timed events, "" for untimed ones.
fn start_time(e: &EventJson) -> Option<String> {
    (!calendar::is_untimed(e.starts_at, e.all_day))
        .then(|| super::london(e.starts_at).format("%H:%M").to_string())
}

/// "until 31 Jan 2027".
fn until(e: &EventJson) -> String {
    let (_, last) = calendar::span(e.starts_at, e.ends_at);
    format!("until {}", last.format("%-d %b %Y"))
}

// ---------------------------------------------------------------- markup

/// ‹ Today › and the view switch.
fn toolbar(
    range: &Range,
    view: View,
    date: NaiveDate,
    link: &dyn Fn(View, NaiveDate) -> String,
) -> Markup {
    let unit = view.unit();
    html! {
        div class="cal-bar" {
            div class="cal-nav" {
                a class="sq-button" href=(link(view, calendar::step(view, date, false))) rel="prev" {
                    span aria-hidden="true" { "‹" } span class="vh" { "Previous " (unit) }
                }
                a class="sq-button today-button" href=(link(view, today())) { "Today" }
                a class="sq-button" href=(link(view, calendar::step(view, date, true))) rel="next" {
                    span aria-hidden="true" { "›" } span class="vh" { "Next " (unit) }
                }
            }
            h2 class="cal-title" id="cal-title" { (range.title(view)) }
            span class="tag-mono" { (range.week_tag()) }
            nav class="segmented" aria-label="Calendar view" {
                @for v in View::ALL {
                    a href=(link(v, date)) aria-current=[(v == view).then_some("page")] { (v.label()) }
                }
            }
        }
    }
}

/// Category and "Free only" chips plus the area form.
fn filter_row(f: &CalFilters, view: View, date: NaiveDate) -> Markup {
    let with = |change: &dyn Fn(&mut CalFilters)| {
        let mut g = f.clone();
        change(&mut g);
        g.view_link(view, date)
    };
    html! {
        div class="cal-filters" {
            p class="chip-row" aria-label="Type" {
                a class="pill" aria-current=[f.category.is_empty().then_some("true")]
                    href=(with(&|g| g.category.clear())) { "All types" }
                @for c in Category::ALL {
                    a class="pill" aria-current=[(f.category == c.as_str()).then_some("true")]
                        href=(with(&|g| g.category = c.as_str().to_string())) { (title_case(c.as_str())) }
                }
                a class="pill free-pill" aria-current=[f.free.then_some("true")]
                    href=(with(&|g| g.free = !f.free)) {
                    span class="sq" {} "Free only"
                }
            }
            form class="cal-area" method="get" action="/calendar" {
                input type="hidden" name="view" value=(view.as_str());
                input type="hidden" name="date" value=(iso(date));
                @if !f.category.is_empty() { input type="hidden" name="category" value=(f.category); }
                @if f.free { input type="hidden" name="free" value="true"; }
                @for s in &f.sources { input type="hidden" name="source" value=(s); }
                label for="cal-near" class="vh" { "Area" }
                select id="cal-near" name="near" {
                    option value="" selected[f.near.is_empty()] { "Area: all of London" }
                    @for a in &AREAS {
                        option value=(a.key) selected[f.near == a.key] { (a.label) }
                    }
                }
                button type="submit" class="secondary" { "Apply" }
            }
        }
    }
}

/// Mon…Sun headings over the grid (decorative: every day cell names its date).
fn weekday_heads(range: &Range) -> Markup {
    html! {
        div class="cal-dow" aria-hidden="true" {
            @for d in range.grid_days().into_iter().take(7) {
                span { (calendar::weekday_short(d)) }
            }
        }
    }
}

/// The grid of days. `day_body(d)` gives a day's contents and whether it is
/// empty; `day_link` makes the date a link (to the listing for that day).
fn grid(
    range: &Range,
    view: View,
    label: &str,
    day_body: &dyn Fn(NaiveDate) -> (Markup, bool),
    day_link: Option<&dyn Fn(NaiveDate) -> String>,
) -> Markup {
    let today = today();
    html! {
        div class={ "cal cal-" (view.as_str()) } {
            @if view != View::Agenda { (weekday_heads(range)) }
            ol class="cal-grid" aria-label=(label) {
                @for d in range.grid_days() {
                    @if range.contains(d) {
                        @let (body, empty) = day_body(d);
                        li class={ "cal-day" @if empty { " empty" } @if d == today { " today" } }
                            aria-current=[(d == today).then_some("date")] {
                            h3 class="cal-date" {
                                @let text = html! {
                                    span class="cal-wd" { (calendar::weekday_short(d)) " " }
                                    span class="cal-num" { (d.format("%d").to_string()) }
                                    span class="cal-mon" { " " (d.format("%b").to_string()) }
                                };
                                @if let Some(link) = day_link {
                                    a href=(link(d)) { (text) }
                                } @else {
                                    (text)
                                }
                            }
                            (body)
                        }
                    } @else {
                        li class="cal-day pad" aria-hidden="true" {
                            span class="cal-num" { (d.format("%d").to_string()) }
                        }
                    }
                }
            }
        }
    }
}

fn entry(e: &EventJson, mark: Mark) -> Markup {
    let (class, flag) = match mark {
        Mark::Event => ("cal-ev", None),
        Mark::Opens => ("cal-ev opens", Some("Opens")),
        Mark::LastDay => ("cal-ev last-day", Some("Last day")),
    };
    html! {
        li class=(class) {
            a href={ "/events/" (e.id) } {
                span class="cal-meta" {
                    @if let Some(flag) = flag {
                        span class="cal-flag" { (flag) }
                    } @else if let Some(t) = start_time(e) {
                        span class="cal-time" { (t) }
                    } @else {
                        span class="cal-time" { "All day" }
                    }
                    @if e.is_free { span class="cal-free" { "Free" } }
                }
                span class="cal-ev-title" { (e.title) }
                @if let Some(v) = &e.venue_name { span class="cal-venue" { (v) } }
            }
        }
    }
}

/// The "ongoing" strip of long-running events.
fn ongoing_strip(events: &[&EventJson], label: &str) -> Markup {
    html! {
        section class="cal-ongoing" aria-labelledby="ongoing-h" {
            div class="wrap-x" {
                h2 id="ongoing-h" class="strip-label" {
                    "Ongoing across London" span class="mono" { " // " (label) }
                }
                @if events.is_empty() {
                    p class="small" { "No long-running exhibitions in this view." }
                } @else {
                    ul class="ongoing-list" {
                        @for e in events {
                            li {
                                a href={ "/events/" (e.id) } {
                                    span class="sq" {}
                                    span class="og-title" { (e.title) }
                                    @if let Some(v) = &e.venue_name { span class="og-venue" { " · " (v) } }
                                    span class="og-until" { " " (until(e)) }
                                    @if e.is_free { span class="cal-free" { "Free" } }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}

fn head(eyebrow: &str, title: &str, lede: Markup, actions: Markup) -> Markup {
    html! {
        section class="page-head" {
            div class="wrap-x hero-row" {
                div {
                    p class="eyebrow" { span class="dot" {} (eyebrow) }
                    h1 { (title) }
                    p class="lede" { (lede) }
                }
                div class="head-actions" { (actions) }
            }
        }
    }
}

/// "My calendar" (saved) and "Subscribe" buttons of the heading band.
fn head_actions(webcal: &str) -> Markup {
    html! {
        a class="button secondary" href="/saved/calendar" {
            "My calendar"
            span class="count" data-saved-count hidden { "0" }
            span class="vh" { " (saved events)" }
        }
        // `webcal` is built from SITE_ORIGIN and our own query string (not
        // data), so it does not go through `safe_link`.
        a class="button" href=(webcal) { "Subscribe (.ics)" }
    }
}

fn feed_panel(feed_path: &str, webcal: &str) -> Markup {
    let google = format!(
        "https://calendar.google.com/calendar/r?{}",
        url::form_urlencoded::Serializer::new(String::new())
            .append_pair("cid", webcal)
            .finish()
    );
    html! {
        section class="panel feed" aria-labelledby="feed-h" {
            div class="panel-head" { h2 id="feed-h" { "Calendar feed" } }
            p class="small" {
                "Subscribe to see these events in Apple Calendar, Google Calendar, Outlook or "
                "Thunderbird: the next " (FEED_DAYS) " days, with the filters chosen above. "
                "Your calendar app fetches updates by itself; the feed changes at most hourly."
            }
            div class="feed-actions" {
                a class="button secondary" href=(webcal) { "Subscribe via Apple / iCal" }
                a class="button secondary" href=(google) rel="noopener" { "Add to Google Calendar" }
                a class="arrow-link" href=(feed_path) { "Download .ics" }
            }
            p class="feed-url mono" { span class="dot" {} (webcal) }
        }
    }
}

fn starting_panel(events: &[EventJson], see_all: &str, now: DateTime<Utc>) -> Markup {
    html! {
        section class="panel starting" aria-labelledby="starting-h" {
            div class="panel-head" {
                h2 id="starting-h" { "Starting this week" }
                span class="mono" { "Next 7 days" }
            }
            @if events.is_empty() {
                p class="small" { "Nothing with these filters starts in the next 7 days." }
            } @else {
                ul {
                    @for e in events {
                        li {
                            p class="mono" { (super::event_when(e, now)) }
                            a class="name" href={ "/events/" (e.id) } { (e.title) }
                            @if let Some(v) = &e.venue_name { span class="mono" { (v) } }
                            div class="starting-actions" {
                                @if let Some(p) = super::price(e) {
                                    span class={ "badge price" @if e.is_free { " free" } } { (p) }
                                }
                                (save_button(e))
                            }
                        }
                    }
                }
            }
            p { a class="arrow-link" href=(see_all) { "All events of the next 7 days →" } }
        }
    }
}

fn bad_filters(msg: &str) -> Response {
    super::error_page(
        StatusCode::BAD_REQUEST,
        "Check the filters",
        &format!("Check the filters: {msg}"),
    )
}

// ---------------------------------------------------------------- handlers

pub(super) async fn calendar_page(
    State(state): State<AppState>,
    RawQuery(raw): RawQuery,
) -> Response {
    let (f, view, date) = CalFilters::parse(raw.as_deref().unwrap_or(""));
    let date = date.unwrap_or_else(today);
    let range = Range::for_view(view, date);
    let (events, truncated) = match load(&state, &f, range.first, range.last, VIEW_CAP).await {
        Ok(Ok(r)) => r,
        Ok(Err(msg)) => return bad_filters(&msg),
        Err(e) => return internal_error(e),
    };
    let today = today();
    let week_end = today + Duration::days(6);
    let starting: Vec<EventJson> = match load(&state, &f, today, week_end, 100).await {
        Ok(Ok((evs, _))) => evs
            .into_iter()
            .filter(|e| calendar::london_date(e.starts_at) >= today)
            .take(5)
            .collect(),
        Ok(Err(msg)) => return bad_filters(&msg),
        Err(e) => return internal_error(e),
    };
    // A multi-session event (#207) is placed on each of its session days
    // (never as a long run); `owner[i]` is the event of `spans[i]`.
    let mut owner: Vec<usize> = Vec::new();
    let mut spans: Vec<(NaiveDate, NaiveDate)> = Vec::new();
    for (i, e) in events.iter().enumerate() {
        if e.sessions.is_empty() {
            owner.push(i);
            spans.push(calendar::span(e.starts_at, e.ends_at));
        }
        for s in &e.sessions {
            let day = calendar::london_date(s.starts_at);
            owner.push(i);
            spans.push((day, day));
        }
    }
    let placed = calendar::place(&spans, &range);
    let ongoing: Vec<&EventJson> = placed.ongoing.iter().map(|&i| &events[owner[i]]).collect();
    let feed_path = f.feed_path();
    let webcal = format!(
        "webcal://{}{}",
        SITE_ORIGIN.trim_start_matches("https://"),
        feed_path
    );
    let link = |v: View, d: NaiveDate| f.view_link(v, d);
    let day_link = |d: NaiveDate| f.day_link(d);
    let day_body = |d: NaiveDate| match placed.days.get(&d) {
        Some(entries) => (
            html! {
                ul class="cal-events" {
                    @for (i, m) in entries { (entry(&events[owner[*i]], *m)) }
                    // The month grid shows the first MONTH_CELL_MAX entries (CSS);
                    // this link (grid only) opens the day's listing for the rest.
                    @if entries.len() > MONTH_CELL_MAX {
                        li class="cal-more" {
                            a href=(f.day_link(d)) {
                                "+" (entries.len() - MONTH_CELL_MAX) " more"
                                span class="vh" { " on " (d.format("%A %-d %B").to_string()) }
                            }
                        }
                    }
                }
            },
            false,
        ),
        None => (html! {}, true),
    };
    let strip_label = match view {
        View::Week => "on all week",
        _ => "on all month",
    };
    let see_all = format!(
        "/?{}",
        f.query(&[("from", iso(today)), ("to", iso(week_end))])
    );
    let now = Utc::now();
    page(
        StatusCode::OK,
        "Calendar",
        Nav::Calendar,
        html! {
            (head(
                "London // Calendar",
                "London cultural calendar",
                html! {
                    "Every event we list, day by day. Long-running exhibitions sit in the strip "
                    "above the grid instead of filling every day; subscribe to take the "
                    "calendar into your own calendar app."
                },
                head_actions(&webcal),
            ))
            section class="cal-toolbar" aria-label="Calendar controls" {
                div class="wrap-x" {
                    (toolbar(&range, view, date, &link))
                    (filter_row(&f, view, date))
                }
            }
            (ongoing_strip(&ongoing, strip_label))
            section class="band cal-body" {
                div class="wrap-x cal-layout" {
                    div class="cal-main" {
                        @if truncated {
                            p class="results-status" role="status" {
                                "This view has more than " (VIEW_CAP) " events; only the first "
                                (VIEW_CAP) " are shown. Narrow it with a filter."
                            }
                        }
                        @if placed.days.is_empty() {
                            p class="cal-none" {
                                @if ongoing.is_empty() {
                                    "Nothing listed in this " (view.unit()) " with these filters."
                                } @else {
                                    "No one-off events in this " (view.unit()) "; see the ongoing exhibitions above."
                                }
                            }
                        }
                        (grid(&range, view, &range.title(view), &day_body, Some(&day_link)))
                    }
                    aside class="cal-rail" aria-label="More" {
                        (starting_panel(&starting, &see_all, now))
                        (feed_panel(&feed_path, &webcal))
                    }
                }
            }
        },
    )
}

/// `GET /calendar.ics`: upcoming events (today + [`FEED_DAYS`] days, London)
/// for the calendar's filters. Bad filters are a plain-text 400.
pub(super) async fn calendar_feed(
    State(state): State<AppState>,
    RawQuery(raw): RawQuery,
) -> Response {
    let (f, _, _) = CalFilters::parse(raw.as_deref().unwrap_or(""));
    let from = today();
    let to = from + Duration::days(FEED_DAYS - 1);
    let events = match load(&state, &f, from, to, FEED_CAP).await {
        Ok(Ok((events, _))) => events,
        Ok(Err(msg)) => {
            return (
                StatusCode::BAD_REQUEST,
                [(header::CONTENT_TYPE, "text/plain; charset=utf-8")],
                format!("Bad filters: {msg}\n"),
            )
                .into_response();
        }
        Err(e) => {
            tracing::error!(error = %e, "calendar feed query failed");
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                [(header::CONTENT_TYPE, "text/plain; charset=utf-8")],
                "internal error\n",
            )
                .into_response();
        }
    };
    let feed: Vec<ics::FeedEvent> = events.iter().map(feed_event).collect();
    let name = match f.category.as_str() {
        "" => format!("{BRAND}: London events"),
        c => format!("{BRAND}: London {c}s"),
    };
    let body = ics::calendar(&name, &feed, Utc::now());
    let mut resp = (StatusCode::OK, body).into_response();
    let h = resp.headers_mut();
    h.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/calendar; charset=utf-8"),
    );
    h.insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("public, max-age=3600"),
    );
    h.insert(
        header::CONTENT_DISPOSITION,
        HeaderValue::from_static("inline; filename=\"musenmingle.ics\""),
    );
    resp
}

/// An event as a feed entry: URL is the venue's page (else ours); the
/// description is the stored excerpt (none for facts-only sources, never AI
/// text) and a link back to us.
fn feed_event(e: &EventJson) -> ics::FeedEvent {
    let ours = format!("{SITE_ORIGIN}/events/{}", e.id);
    let url = primary_source(e).map(|(_, u)| u);
    let mut description = e.description.clone().unwrap_or_default();
    if !description.is_empty() {
        description.push_str("\n\n");
    }
    description.push_str(&format!("via {BRAND}: {ours}"));
    let location = match (&e.venue_name, &e.address) {
        (Some(v), Some(a)) if !a.contains(v.as_str()) => Some(format!("{v}, {a}")),
        (Some(v), _) => Some(v.clone()),
        (None, a) => a.clone(),
    };
    ics::FeedEvent {
        id: e.id,
        title: e.title.clone(),
        starts_at: e.starts_at,
        ends_at: e.ends_at,
        all_day: e.all_day,
        sessions: e.sessions.clone(),
        location,
        url: Some(url.unwrap_or(ours)),
        description,
    }
}

/// `GET /saved/calendar`: the layout of `/calendar` with empty days;
/// `/static/app.js` puts this browser's saved events into it.
pub(super) async fn saved_calendar(RawQuery(raw): RawQuery) -> Response {
    let (_, view, date) = CalFilters::parse(raw.as_deref().unwrap_or(""));
    let date = date.unwrap_or_else(today);
    let range = Range::for_view(view, date);
    let link = |v: View, d: NaiveDate| {
        format!(
            "/saved/calendar?view={}&date={}",
            v.as_str(),
            d.format("%Y-%m-%d")
        )
    };
    let day_body = |d: NaiveDate| (html! { ul class="cal-events" data-day=(iso(d)) {} }, true);
    page(
        StatusCode::OK,
        "My calendar",
        Nav::Saved,
        html! {
            (head(
                "Saved // this browser only",
                "My calendar",
                html! {
                    "The events you saved, day by day. They are kept only in this browser, so "
                    "this calendar can't be subscribed to; use "
                    a href="/saved" { "Export saved as .ics" } " on the Saved page instead."
                },
                html! { a class="button secondary" href="/saved" { "Saved list" } },
            ))
            section class="cal-toolbar" aria-label="Calendar controls" {
                div class="wrap-x" { (toolbar(&range, view, date, &link)) }
            }
            section class="cal-ongoing" aria-labelledby="ongoing-h" id="saved-ongoing-strip" hidden {
                div class="wrap-x" {
                    h2 id="ongoing-h" class="strip-label" { "Ongoing // saved" }
                    ul class="ongoing-list" id="saved-ongoing" {}
                }
            }
            section class="band cal-body" {
                div class="wrap-x" {
                    div class="cal-main" id="saved-calendar"
                        data-first=(iso(range.first)) data-last=(iso(range.last)) {
                        noscript {
                            p class="error" { "My calendar needs JavaScript (your saves live in this browser). Everything else on this site works without it." }
                        }
                        p id="saved-cal-status" class="results-status" role="status" aria-live="polite" {}
                        (grid(&range, view, &range.title(view), &day_body, None))
                    }
                }
            }
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn links_keep_the_filters() {
        let (f, view, date) = CalFilters::parse(
            "view=week&date=2026-10-07&category=talk&free=true&near=east&source=barbican&when=evening",
        );
        assert_eq!(view, View::Week);
        assert_eq!(date, NaiveDate::from_ymd_opt(2026, 10, 7));
        let d = NaiveDate::from_ymd_opt(2026, 10, 12).unwrap();
        assert_eq!(
            f.view_link(View::Month, d),
            "/calendar?view=month&date=2026-10-12&category=talk&free=true&near=east&source=barbican"
        );
        assert_eq!(
            f.day_link(d),
            "/?from=2026-10-12&to=2026-10-12&category=talk&free=true&near=east&source=barbican"
        );
        assert_eq!(
            f.feed_path(),
            "/calendar.ics?category=talk&free=true&near=east&source=barbican"
        );
        assert_eq!(CalFilters::default().feed_path(), "/calendar.ics");
        let q = f.listing_query(d, d, VIEW_CAP).unwrap();
        assert_eq!(q.limit, VIEW_CAP);
        assert!(q.filter.when.is_none());
        let bad = CalFilters {
            near: "mars".into(),
            ..CalFilters::default()
        };
        assert!(bad.listing_query(d, d, VIEW_CAP).is_err());
    }
}
