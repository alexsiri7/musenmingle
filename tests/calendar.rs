//! `GET /calendar` (month / week / agenda), `GET /calendar.ics` and
//! `GET /saved/calendar` against a real database. Views use fixed dates
//! (`?date=`) in October 2026, when the clocks go back (Sun 25 Oct); the
//! feed is relative to today.

mod common;

use std::net::SocketAddr;

use axum::Router;
use axum::body::Body;
use axum::extract::connect_info::MockConnectInfo;
use axum::http::{Request, StatusCode, header};
use chrono::{DateTime, Duration, TimeZone, Utc};
use common::TestDb;
use musenmingle::api::ApiSettings;
use musenmingle::config::SuggestionConfig;
use musenmingle::suggestions::Suggestions;
use sqlx::PgPool;
use tower::ServiceExt;
use uuid::Uuid;

fn app(pool: &PgPool) -> Router {
    let config = SuggestionConfig {
        ip_salt: Some("test-salt".into()),
        ..SuggestionConfig::default()
    };
    let settings = ApiSettings {
        github_repo: "alexsiri7/musenmingle".into(),
        cors_origins: Vec::new(),
    };
    musenmingle::api::router(
        pool.clone(),
        Suggestions::new(config, None).unwrap(),
        settings,
    )
    .layer(MockConnectInfo(SocketAddr::from(([10, 0, 0, 1], 4000))))
}

struct Page {
    status: StatusCode,
    headers: axum::http::HeaderMap,
    body: String,
}

async fn get(app: &Router, uri: &str) -> Page {
    let resp = app
        .clone()
        .oneshot(Request::get(uri).body(Body::empty()).unwrap())
        .await
        .unwrap();
    let status = resp.status();
    let headers = resp.headers().clone();
    let bytes = axum::body::to_bytes(resp.into_body(), 8 * 1024 * 1024)
        .await
        .unwrap();
    Page {
        status,
        headers,
        body: String::from_utf8_lossy(&bytes).into_owned(),
    }
}

fn assert_html(p: &Page) {
    assert_eq!(p.status, StatusCode::OK, "{}", p.body);
    assert_eq!(p.headers[header::CONTENT_TYPE], "text/html; charset=utf-8");
    assert_eq!(
        p.headers[header::CONTENT_SECURITY_POLICY],
        musenmingle::web::CSP
    );
    assert_eq!(p.body.matches("<script").count(), 1, "{}", p.body);
    assert!(!p.body.contains("style=\""), "{}", p.body);
    assert!(!p.body.contains(" onclick="));
    assert!(!p.body.contains("github.com/alexsiri7/"));
}

struct Ev {
    title: &'static str,
    starts_at: DateTime<Utc>,
    ends_at: Option<DateTime<Utc>>,
    all_day: bool,
    category: &'static str,
    is_free: bool,
    description: Option<&'static str>,
}

impl Ev {
    fn new(title: &'static str, starts_at: DateTime<Utc>) -> Self {
        Ev {
            title,
            starts_at,
            ends_at: None,
            all_day: false,
            category: "talk",
            is_free: false,
            description: None,
        }
    }
}

async fn insert(pool: &PgPool, e: Ev) -> Uuid {
    sqlx::query_scalar(
        "INSERT INTO events.events
            (title, starts_at, ends_at, all_day, category, is_free, dedupe_key, venue_name,
             description)
         VALUES ($1, $2, $3, $4, $5, $6, $1, 'Barbican', $7)
         RETURNING id",
    )
    .bind(e.title)
    .bind(e.starts_at)
    .bind(e.ends_at)
    .bind(e.all_day)
    .bind(e.category)
    .bind(e.is_free)
    .bind(e.description)
    .fetch_one(pool)
    .await
    .unwrap()
}

async fn link(pool: &PgPool, event: Uuid, url: &str) {
    sqlx::query(
        "INSERT INTO events.event_sources
            (event_id, source_id, source_event_id, source_url, raw, first_seen_at, last_seen_at)
         SELECT $1, id, $2, $2, '{}', now(), now() FROM events.sources WHERE key = 'barbican'",
    )
    .bind(event)
    .bind(url)
    .execute(pool)
    .await
    .unwrap();
}

fn utc(y: i32, m: u32, d: u32, h: u32, min: u32) -> DateTime<Utc> {
    Utc.with_ymd_and_hms(y, m, d, h, min, 0).unwrap()
}

/// London midnight of a BST date (23:00 UTC the day before).
fn bst_midnight(y: i32, m: u32, d: u32) -> DateTime<Utc> {
    utc(y, m, d, 0, 0) - Duration::hours(1)
}

/// October 2026 fixtures: returns (talk, show, short run, late night).
async fn seed_october(pool: &PgPool) -> (Uuid, Uuid, Uuid, Uuid) {
    // 18:30 BST on Wed 7 Oct.
    let talk = insert(
        pool,
        Ev {
            is_free: true,
            ..Ev::new("Type Club talk", utc(2026, 10, 7, 17, 30))
        },
    )
    .await;
    // Runs all month and beyond: the strip only.
    let show = insert(
        pool,
        Ev {
            ends_at: Some(utc(2027, 1, 31, 0, 0)),
            all_day: true,
            category: "exhibition",
            ..Ev::new("Platform show", bst_midnight(2026, 9, 1))
        },
    )
    .await;
    // Opens Fri 16 Oct, last day Sun 25 Oct.
    let short = insert(
        pool,
        Ev {
            ends_at: Some(utc(2026, 10, 25, 0, 0)),
            all_day: true,
            category: "exhibition",
            ..Ev::new("Pavilion run", bst_midnight(2026, 10, 16))
        },
    )
    .await;
    // 00:30 BST on Sun 25 Oct (23:30 UTC on the 24th), the night the clocks go back.
    let late = insert(
        pool,
        Ev {
            category: "community",
            ..Ev::new("Late night social", utc(2026, 10, 24, 23, 30))
        },
    )
    .await;
    // November: never in October's views.
    insert(pool, Ev::new("November talk", utc(2026, 11, 2, 19, 0))).await;
    (talk, show, short, late)
}

/// The `<li class="cal-day…">` of a date (up to the next day), by its `data`
/// in the day link.
fn day_cell<'a>(body: &'a str, date: &str) -> &'a str {
    let key = format!("href=\"/?from={date}&amp;to={date}");
    let at = body.find(&key).unwrap_or_else(|| panic!("no day {date}"));
    let start = body[..at].rfind("<li class=\"cal-day").unwrap();
    let end = body[at..]
        .find("<li class=\"cal-day")
        .map_or(body.len(), |e| at + e);
    &body[start..end]
}

#[tokio::test]
async fn month_view_places_events_once_with_markers() {
    let Some(db) = TestDb::create("month_view_places_events_once_with_markers").await else {
        return;
    };
    let pool = db.migrated_pool().await;
    let (talk, show, short, late) = seed_october(&pool).await;
    let app = app(&pool);

    let p = get(&app, "/calendar?date=2026-10-14").await;
    assert_html(&p);
    let b = &p.body;
    assert!(b.contains("<title>Calendar · Muse &amp; Mingle</title>"));
    assert!(
        b.contains("<a href=\"/calendar\" aria-current=\"page\">Calendar</a>"),
        "{b}"
    );
    assert!(b.contains("<h1>London cultural calendar</h1>"));
    assert!(b.contains("<h2 class=\"cal-title\" id=\"cal-title\">October 2026</h2>"));
    assert!(b.contains("<span class=\"tag-mono\">W40 — W44</span>"));
    // No placeholder copy from the Stitch screen.
    for untrue in [
        "Curator",
        "curated",
        "synchroni",
        "nstitutional",
        "monograph",
    ] {
        assert!(!b.contains(untrue), "{untrue}");
    }
    // Prev / next / today and the active view.
    assert!(b.contains("href=\"/calendar?view=month&amp;date=2026-09-01\" rel=\"prev\""));
    assert!(b.contains("href=\"/calendar?view=month&amp;date=2026-11-01\" rel=\"next\""));
    assert!(b.contains(
        "<a href=\"/calendar?view=month&amp;date=2026-10-14\" aria-current=\"page\">Month</a>"
    ));
    assert!(b.contains("<a href=\"/calendar?view=week&amp;date=2026-10-14\">Week</a>"));
    // 35 grid cells: 31 days + 3 padding days before and 1 after.
    assert_eq!(b.matches("<li class=\"cal-day").count(), 35);
    assert_eq!(
        b.matches("<li class=\"cal-day pad\" aria-hidden=\"true\">")
            .count(),
        4
    );

    // Long-running show: once, in the strip, never in a day cell.
    assert_eq!(b.matches(&format!("href=\"/events/{show}\"")).count(), 1);
    let strip =
        &b[b.find("class=\"cal-ongoing\"").unwrap()..b.find("class=\"band cal-body\"").unwrap()];
    assert!(strip.contains("Ongoing across London"));
    assert!(strip.contains("on all month"));
    assert!(strip.contains("Platform show"));
    assert!(strip.contains("until 31 Jan 2027"), "{strip}");
    // The 16–25 Oct run: in the strip, plus "Opens" and "Last day" markers.
    // Count outside the sidebar: its "starting" panel uses the wall-clock
    // `today()`, so it lists this run when the test runs in early October.
    let grid = &b[..b.find("<aside").unwrap()];
    assert_eq!(
        grid.matches(&format!("href=\"/events/{short}\"")).count(),
        3
    );
    let opens = day_cell(b, "2026-10-16");
    assert!(opens.contains("<li class=\"cal-ev opens\">"), "{opens}");
    assert!(opens.contains("<span class=\"cal-flag\">Opens</span>"));
    let closing = day_cell(b, "2026-10-25");
    assert!(
        closing.contains("<li class=\"cal-ev last-day\">"),
        "{closing}"
    );
    assert!(closing.contains("<span class=\"cal-flag\">Last day</span>"));
    // 00:30 BST on the 25th is on the 25th, not the 24th (UTC).
    assert!(closing.contains("Late night social"), "{closing}");
    assert!(closing.contains("<span class=\"cal-time\">00:30</span>"));
    assert!(!day_cell(b, "2026-10-24").contains("Late night social"));
    assert!(b.contains(&format!("href=\"/events/{late}\"")));
    // The talk on its day with its London time and Free.
    let wed = day_cell(b, "2026-10-07");
    assert!(wed.contains(&format!("href=\"/events/{talk}\"")));
    assert!(
        wed.contains("<span class=\"cal-time\">18:30</span><span class=\"cal-free\">Free</span>"),
        "{wed}"
    );
    assert!(wed.contains("<span class=\"cal-venue\">Barbican</span>"));
    assert!(!b.contains("November talk"));

    // Agenda markup: days without events are marked (hidden at phone width),
    // every day heading names the weekday and month for the list layout.
    assert!(wed.starts_with("<li class=\"cal-day\">"), "{wed}");
    assert!(day_cell(b, "2026-10-08").starts_with("<li class=\"cal-day empty\">"));
    assert!(wed.contains(
        "<span class=\"cal-wd\">Wed </span><span class=\"cal-num\">07</span><span class=\"cal-mon\"> Oct</span>"
    ));
    // The agenda view is the same list with the agenda class.
    let a = get(&app, "/calendar?view=agenda&date=2026-10-14").await;
    assert_html(&a);
    assert!(a.body.contains("<div class=\"cal cal-agenda\">"));
    assert!(!a.body.contains("class=\"cal-dow\""));

    // Feed links carry the filters but not the view or date.
    assert!(b.contains("href=\"webcal://musenmingle.interstellarai.net/calendar.ics\""));
    assert!(b.contains("href=\"/calendar.ics\">Download .ics</a>"));
    assert!(b.contains("https://calendar.google.com/calendar/r?cid=webcal%3A%2F%2Fmusenmingle.interstellarai.net%2Fcalendar.ics"));
    // My calendar (saved) link with the saved count filled in by app.js.
    assert!(b.contains("<a class=\"button secondary\" href=\"/saved/calendar\">My calendar<span class=\"count\" data-saved-count hidden>0</span>"));
    db.drop_db().await;
}

#[tokio::test]
async fn week_view_across_the_clock_change() {
    let Some(db) = TestDb::create("week_view_across_the_clock_change").await else {
        return;
    };
    let pool = db.migrated_pool().await;
    let (talk, show, short, late) = seed_october(&pool).await;
    let app = app(&pool);
    // Mon 19 – Sun 25 Oct 2026.
    let p = get(&app, "/calendar?view=week&date=2026-10-22").await;
    assert_html(&p);
    let b = &p.body;
    assert!(b.contains(">19–25 October 2026</h2>"), "{b}");
    assert!(b.contains("on all week"));
    assert_eq!(b.matches("<li class=\"cal-day").count(), 7);
    assert!(!b.contains("cal-day pad"));
    assert!(b.contains("href=\"/calendar?view=week&amp;date=2026-10-12\" rel=\"prev\""));
    assert!(b.contains("href=\"/calendar?view=week&amp;date=2026-10-26\" rel=\"next\""));
    // Exclude the sidebar: its "starting" panel is the real next 7 days
    // (wall-clock `today()`, not this view's `date`), so it can legitimately
    // list the talk when the test happens to run in early October.
    let grid = &b[..b.find("<aside").unwrap()];
    assert!(!grid.contains(&format!("/events/{talk}")));
    assert_eq!(b.matches(&format!("href=\"/events/{show}\"")).count(), 1);
    // Strip + last day (it opened the week before), outside the sidebar.
    assert_eq!(
        grid.matches(&format!("href=\"/events/{short}\"")).count(),
        2
    );
    let sun = day_cell(b, "2026-10-25");
    assert!(sun.contains(&format!("/events/{late}")));
    assert!(sun.contains("Last day"));
    // The next week (26 Oct – 1 Nov) crosses the month end.
    let n = get(&app, "/calendar?view=week&date=2026-10-30").await;
    assert!(n.body.contains(">26 October – 1 November 2026</h2>"));
    assert!(n.body.contains("W44"));
    db.drop_db().await;
}

#[tokio::test]
async fn filters_are_reflected_in_the_grid_links_and_feed() {
    let Some(db) = TestDb::create("filters_are_reflected_in_the_grid_links_and_feed").await else {
        return;
    };
    let pool = db.migrated_pool().await;
    let (talk, show, _, late) = seed_october(&pool).await;
    let app = app(&pool);
    let p = get(
        &app,
        "/calendar?date=2026-10-01&category=talk&free=true&view=month",
    )
    .await;
    assert_html(&p);
    let b = &p.body;
    assert!(b.contains(&format!("/events/{talk}")));
    assert!(!b.contains(&format!("/events/{show}")));
    assert!(!b.contains(&format!("/events/{late}")));
    assert!(b.contains("No long-running exhibitions in this view."));
    // Active chips; toggling keeps the other filters.
    assert!(b.contains("<a class=\"pill\" aria-current=\"true\" href=\"/calendar?view=month&amp;date=2026-10-01&amp;category=talk&amp;free=true\">Talk</a>"), "{b}");
    assert!(b.contains("<a class=\"pill free-pill\" aria-current=\"true\" href=\"/calendar?view=month&amp;date=2026-10-01&amp;category=talk\">"));
    // Day links open the listing for that day with the same filters.
    assert!(
        b.contains("href=\"/?from=2026-10-07&amp;to=2026-10-07&amp;category=talk&amp;free=true\"")
    );
    // The area form keeps them as hidden fields.
    assert!(b.contains("<input type=\"hidden\" name=\"category\" value=\"talk\">"));
    assert!(b.contains("<input type=\"hidden\" name=\"free\" value=\"true\">"));
    // Feed links carry them.
    assert!(b.contains(
        "href=\"webcal://musenmingle.interstellarai.net/calendar.ics?category=talk&amp;free=true\""
    ));
    // An empty month says so.
    let e = get(&app, "/calendar?date=2026-12-01&category=workshop").await;
    assert!(
        e.body
            .contains("Nothing listed in this month with these filters.")
    );
    // Bad filters are a 400 page, not a 500.
    let bad = get(&app, "/calendar?near=mars").await;
    assert_eq!(bad.status, StatusCode::BAD_REQUEST);
    // Nonsense view/date fall back to this month.
    let ok = get(&app, "/calendar?view=year&date=soon").await;
    assert_html(&ok);
    db.drop_db().await;
}

#[tokio::test]
async fn ics_feed_is_valid_and_filtered() {
    let Some(db) = TestDb::create("ics_feed_is_valid_and_filtered").await else {
        return;
    };
    let pool = db.migrated_pool().await;
    let now = Utc::now();
    let talk = insert(
        &pool,
        Ev {
            ends_at: Some(now + Duration::days(3) + Duration::hours(2)),
            description: Some("Poets, printers; and makers.\nBring a pen."),
            ..Ev::new("Talk, with; punctuation", now + Duration::days(3))
        },
    )
    .await;
    link(&pool, talk, "https://www.barbican.org.uk/talk").await;
    let show = insert(
        &pool,
        Ev {
            ends_at: Some(now + Duration::days(200)),
            all_day: true,
            category: "exhibition",
            ..Ev::new("Long show", now - Duration::days(30))
        },
    )
    .await;
    insert(&pool, Ev::new("Too far ahead", now + Duration::days(120))).await;
    insert(&pool, Ev::new("Last month", now - Duration::days(30))).await;
    let app = app(&pool);

    let p = get(&app, "/calendar.ics").await;
    assert_eq!(p.status, StatusCode::OK, "{}", p.body);
    assert_eq!(
        p.headers[header::CONTENT_TYPE],
        "text/calendar; charset=utf-8"
    );
    assert_eq!(p.headers[header::CACHE_CONTROL], "public, max-age=3600");
    let b = &p.body;
    assert!(b.starts_with("BEGIN:VCALENDAR\r\nVERSION:2.0\r\n"));
    assert!(b.ends_with("END:VCALENDAR\r\n"));
    assert!(!b.replace("\r\n", "").contains('\n'), "bare LF");
    for line in b.split("\r\n") {
        assert!(line.len() <= 75, "{line}");
    }
    let unfolded = b.replace("\r\n ", "");
    assert!(unfolded.contains("X-WR-CALNAME:Muse & Mingle: London events\r\n"));
    assert!(unfolded.contains("REFRESH-INTERVAL;VALUE=DURATION:PT1H\r\n"));
    assert_eq!(unfolded.matches("BEGIN:VEVENT").count(), 2, "{unfolded}");
    assert!(unfolded.contains(&format!("UID:{talk}@musenmingle.interstellarai.net\r\n")));
    assert!(unfolded.contains(&format!("UID:{show}@musenmingle.interstellarai.net\r\n")));
    assert!(unfolded.contains("SUMMARY:Talk\\, with\\; punctuation\r\n"));
    assert!(unfolded.contains(&format!(
        "DESCRIPTION:Poets\\, printers\\; and makers.\\nBring a pen.\\n\\nvia Muse & Mingle: https://musenmingle.interstellarai.net/events/{talk}\r\n"
    )));
    // URL is the venue's page; without one, ours.
    assert!(unfolded.contains("URL:https://www.barbican.org.uk/talk\r\n"));
    assert!(unfolded.contains(&format!(
        "URL:https://musenmingle.interstellarai.net/events/{show}\r\n"
    )));
    assert!(unfolded.contains(&format!(
        "DTSTART:{}\r\n",
        (now + Duration::days(3)).format("%Y%m%dT%H%M%SZ")
    )));
    // The ongoing exhibition is an all-day span.
    assert!(unfolded.contains("DTSTART;VALUE=DATE:"));
    assert!(!unfolded.contains("Too far ahead"));
    assert!(!unfolded.contains("Last month"));

    // Filters apply; bad ones are a plain 400.
    let f = get(&app, "/calendar.ics?category=exhibition").await;
    assert_eq!(f.body.matches("BEGIN:VEVENT").count(), 1);
    assert!(
        f.body
            .contains("X-WR-CALNAME:Muse & Mingle: London exhibitions")
    );
    let bad = get(&app, "/calendar.ics?near=mars").await;
    assert_eq!(bad.status, StatusCode::BAD_REQUEST);
    assert_eq!(
        bad.headers[header::CONTENT_TYPE],
        "text/plain; charset=utf-8"
    );
    db.drop_db().await;
}

#[tokio::test]
async fn saved_calendar_is_a_script_filled_shell() {
    let Some(db) = TestDb::create("saved_calendar_is_a_script_filled_shell").await else {
        return;
    };
    let pool = db.migrated_pool().await;
    seed_october(&pool).await;
    let app = app(&pool);
    let p = get(&app, "/saved/calendar?date=2026-10-05").await;
    assert_html(&p);
    let b = &p.body;
    assert!(b.contains("<title>My calendar · Muse &amp; Mingle</title>"));
    assert!(b.contains("<a href=\"/saved\" aria-current=\"page\">Saved"));
    assert!(b.contains("id=\"saved-calendar\" data-first=\"2026-10-01\" data-last=\"2026-10-31\""));
    assert_eq!(
        b.matches("<ul class=\"cal-events\" data-day=\"2026-10-")
            .count(),
        31
    );
    assert!(b.contains("<noscript>"));
    assert!(b.contains("id=\"saved-ongoing-strip\" hidden"));
    // No events are rendered by the server (saves are only in the browser).
    assert!(!b.contains("/events/"));
    assert!(b.contains("href=\"/saved/calendar?view=month&amp;date=2026-11-01\" rel=\"next\""));
    // /saved links to it.
    let s = get(&app, "/saved").await;
    assert!(s.body.contains("href=\"/saved/calendar\""));
    db.drop_db().await;
}

#[tokio::test]
async fn busy_days_link_to_the_rest_in_the_month_grid() {
    let Some(db) = TestDb::create("busy_days_link_to_the_rest_in_the_month_grid").await else {
        return;
    };
    let pool = db.migrated_pool().await;
    let titles = ["One", "Two", "Three", "Four", "Five", "Six"];
    for (i, t) in titles.iter().enumerate() {
        insert(&pool, Ev::new(t, utc(2026, 10, 8, 9 + i as u32, 0))).await;
    }
    let app = app(&pool);
    let p = get(&app, "/calendar?date=2026-10-08").await;
    assert_html(&p);
    let cell = day_cell(&p.body, "2026-10-08");
    // All six are in the markup (the agenda shows them all); the grid shows
    // four and links to the day's listing for the other two.
    assert_eq!(cell.matches("<li class=\"cal-ev\">").count(), 6);
    assert!(cell.contains(
        "<li class=\"cal-more\"><a href=\"/?from=2026-10-08&amp;to=2026-10-08\">+2 more<span class=\"vh\"> on Thursday 8 October</span></a></li>"
    ), "{cell}");
    assert!(!day_cell(&p.body, "2026-10-09").contains("cal-more"));
    db.drop_db().await;
}
