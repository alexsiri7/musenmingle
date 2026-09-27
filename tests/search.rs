//! Full-text search (`q=`, issue #77) against a real database: stemming,
//! accents, typos, prefixes, venue names, injection safety, relevance order,
//! counts and the home page's search box and empty state. Event dates are
//! relative to now because typo correction only looks at upcoming events.

mod common;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use chrono::{Duration, Utc};
use common::TestDb;
use musenmingle::api::ApiSettings;
use musenmingle::config::SuggestionConfig;
use musenmingle::suggestions::Suggestions;
use serde_json::Value;
use sqlx::PgPool;
use tower::ServiceExt;

fn app(pool: &PgPool) -> Router {
    let suggestions = Suggestions::new(
        SuggestionConfig {
            ip_salt: Some("salt".into()),
            ..Default::default()
        },
        None,
    )
    .unwrap();
    let settings = ApiSettings {
        github_repo: "alexsiri7/musenmingle".into(),
        cors_origins: Vec::new(),
    };
    musenmingle::api::router(pool.clone(), suggestions, settings)
}

async fn get_raw(app: &Router, uri: &str) -> (StatusCode, String) {
    let resp = app
        .clone()
        .oneshot(Request::get(uri).body(Body::empty()).unwrap())
        .await
        .unwrap();
    let status = resp.status();
    let bytes = axum::body::to_bytes(resp.into_body(), 4 * 1024 * 1024)
        .await
        .unwrap();
    (status, String::from_utf8_lossy(&bytes).into_owned())
}

async fn get(app: &Router, uri: &str) -> (StatusCode, Value) {
    let (status, body) = get_raw(app, uri).await;
    (status, serde_json::from_str(&body).unwrap())
}

fn titles(body: &Value) -> Vec<String> {
    body["events"]
        .as_array()
        .unwrap_or_else(|| panic!("no events in {body}"))
        .iter()
        .map(|e| e["title"].as_str().unwrap().to_string())
        .collect()
}

/// `q` as a query-string value.
fn enc(q: &str) -> String {
    url::form_urlencoded::byte_serialize(q.as_bytes()).collect()
}

async fn search(app: &Router, q: &str) -> Vec<String> {
    let (status, body) = get(app, &format!("/v1/events?limit=100&q={}", enc(q))).await;
    assert_eq!(status, StatusCode::OK, "q={q}: {body}");
    let mut t = titles(&body);
    t.sort();
    t
}

struct Ev {
    title: &'static str,
    venue: Option<&'static str>,
    description: Option<&'static str>,
    category: &'static str,
    days_ahead: i64,
    at: Option<(f64, f64)>,
}

impl Ev {
    fn new(title: &'static str, venue: Option<&'static str>) -> Self {
        Ev {
            title,
            venue,
            description: None,
            category: "exhibition",
            days_ahead: 3,
            at: None,
        }
    }
}

async fn insert(pool: &PgPool, e: Ev) {
    sqlx::query(
        "INSERT INTO events.events
            (title, venue_name, description, starts_at, category, dedupe_key, lat, lng)
         VALUES ($1, $2, $3, $4, $5, $1, $6, $7)",
    )
    .bind(e.title)
    .bind(e.venue)
    .bind(e.description)
    .bind(Utc::now() + Duration::days(e.days_ahead))
    .bind(e.category)
    .bind(e.at.map(|a| a.0))
    .bind(e.at.map(|a| a.1))
    .execute(pool)
    .await
    .unwrap();
}

async fn seed(pool: &PgPool) {
    for e in [
        Ev {
            description: Some("Large works on canvas from northern Europe."),
            ..Ev::new("Paintings by Sámi artists", Some("Whitechapel Gallery"))
        },
        Ev {
            category: "workshop",
            at: Some((51.5200, -0.0937)),
            ..Ev::new("Life drawing class", Some("Barbican Centre"))
        },
        Ev {
            description: Some("A talk about ceramics and glaze chemistry."),
            category: "talk",
            days_ahead: 1,
            ..Ev::new("Studio evening", Some("Camden Arts Centre"))
        },
        Ev {
            days_ahead: 5,
            ..Ev::new("Ceramics now", Some("Design Museum"))
        },
        Ev::new("The Room", Some("Somerset House")),
        Ev::new("Photography prize", None),
    ] {
        insert(pool, e).await;
    }
}

#[tokio::test]
async fn search_stems_folds_accents_corrects_typos_and_matches_venues() {
    let Some(db) = TestDb::create("search_stems_folds_accents_corrects_typos").await else {
        return;
    };
    let pool = db.migrated_pool().await;
    seed(&pool).await;
    let app = app(&pool);
    let sami = vec!["Paintings by Sámi artists".to_string()];

    // Stemming: 'painting' finds 'Paintings'.
    assert_eq!(search(&app, "painting").await, sami);
    // Accents, either way round, any case.
    for q in ["Sami", "sámi", "SÁMI", "SAMI ARTISTS"] {
        assert_eq!(search(&app, q).await, sami, "{q}");
    }
    // Venue names, and words of the excerpt.
    assert_eq!(search(&app, "barbican").await, ["Life drawing class"]);
    assert_eq!(search(&app, "canvas").await, sami);
    // Prefix of the last word (as typed).
    assert_eq!(search(&app, "whitech").await, sami);
    assert_eq!(search(&app, "life draw").await, ["Life drawing class"]);
    // Web-search syntax: phrases and exclusions.
    assert_eq!(
        search(&app, "\"life drawing\"").await,
        ["Life drawing class"]
    );
    assert_eq!(
        search(&app, "ceramics -glaze").await,
        ["Ceramics now".to_string()]
    );

    // Typos: corrected against upcoming titles and venues, and reported.
    let (_, body) = get(&app, "/v1/events?q=Whitechaple").await;
    assert_eq!(titles(&body), sami);
    assert_eq!(body["search"]["q"], "Whitechaple");
    assert_eq!(body["search"]["corrected"], "whitechapel");
    assert_eq!(
        search(&app, "barbcian centre").await,
        ["Life drawing class"]
    );
    // A correct word is never "corrected".
    let (_, body) = get(&app, "/v1/events?q=barbican").await;
    assert_eq!(body["search"]["corrected"], Value::Null);
    // Nothing near enough: no results, no suggestion.
    let (_, body) = get(&app, "/v1/events?q=xylophonist").await;
    assert!(titles(&body).is_empty());
    assert_eq!(body["search"]["corrected"], Value::Null);

    // Stop words only: substring of the title or venue.
    assert_eq!(search(&app, "the").await, ["The Room"]);

    // Blank q is no filter.
    let all = search(&app, "").await;
    assert_eq!(all.len(), 6);
    assert_eq!(search(&app, "   ").await, all);
    let (_, body) = get(&app, "/v1/events?q=").await;
    assert!(body.get("search").is_none());
}

#[tokio::test]
async fn search_is_injection_safe_and_bounded() {
    let Some(db) = TestDb::create("search_is_injection_safe_and_bounded").await else {
        return;
    };
    let pool = db.migrated_pool().await;
    seed(&pool).await;
    let app = app(&pool);
    for q in [
        "'); DROP TABLE events.events; --",
        "a:* | b & !c <-> (d",
        "\\ ' \" '' \"\"",
        "painting:* | !x",
        "&&&",
        "!!!",
        "-",
        "\"",
        "null",
        "%_%",
    ] {
        let (status, body) = get(&app, &format!("/v1/events?q={}", enc(q))).await;
        assert_eq!(status, StatusCode::OK, "q={q}: {body}");
    }
    assert_eq!(
        search(&app, "%").await,
        Vec::<String>::new(),
        "no LIKE wildcards"
    );
    let n: i64 = sqlx::query_scalar("SELECT count(*) FROM events.events")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(n, 6);

    let long = "a".repeat(musenmingle::search::MAX_QUERY_CHARS + 1);
    let (status, body) = get(&app, &format!("/v1/events?q={long}")).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    let max = "painting ".repeat(22);
    let (status, _) = get(&app, &format!("/v1/events?q={}", enc(max.trim()))).await;
    assert_eq!(status, StatusCode::OK);
}

#[tokio::test]
async fn search_ranks_by_relevance_pages_and_counts() {
    let Some(db) = TestDb::create("search_ranks_by_relevance_pages_and_counts").await else {
        return;
    };
    let pool = db.migrated_pool().await;
    seed(&pool).await;
    let app = app(&pool);

    // Title match (weight A) before an excerpt match (C), whatever the dates.
    let (_, body) = get(&app, "/v1/events?q=ceramics").await;
    assert_eq!(titles(&body), ["Ceramics now", "Studio evening"]);
    assert_eq!(body["counts"]["when"]["daytime"], 2, "counts follow q");

    // Pages of one walk every match once, in the same order.
    let mut walked = Vec::new();
    let mut uri = "/v1/events?q=ceramics&limit=1".to_string();
    loop {
        let (status, body) = get(&app, &uri).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        walked.extend(titles(&body));
        match body["next_cursor"].as_str() {
            Some(c) => uri = format!("/v1/events?q=ceramics&limit=1&cursor={c}"),
            None => break,
        }
    }
    assert_eq!(walked, ["Ceramics now", "Studio evening"]);
    // A relevance cursor is refused without q (another sort).
    let (_, body) = get(&app, "/v1/events?q=ceramics&limit=1").await;
    let c = body["next_cursor"].as_str().unwrap();
    let (status, _) = get(&app, &format!("/v1/events?limit=1&cursor={c}")).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    // Combines with filters; an area is a filter (still best match first)
    // unless another sort is chosen.
    assert_eq!(
        titles(&get(&app, "/v1/events?q=ceramics&category=talk").await.1),
        ["Studio evening"]
    );
    let (_, body) = get(&app, "/v1/events?q=drawing&near=51.52,-0.09&radius_km=2").await;
    assert_eq!(titles(&body), ["Life drawing class"]);
    assert_eq!(body["sort"], "relevance");
    assert!(body["events"][0]["distance_km"].is_number());
    let (_, body) = get(&app, "/v1/events?q=ceramics&sort=soonest").await;
    assert_eq!(body["sort"], "soonest");
    assert_eq!(titles(&body), ["Studio evening", "Ceramics now"]);
    let (_, body) = get(&app, "/v1/events?sort=relevance").await;
    assert_eq!(body["sort"], "soonest");
    assert_eq!(body["sort_fallback"]["requested"], "relevance");
    let (_, body) = get(&app, "/v1/events?q=ceramics&facets=true").await;
    assert!(body["facets"].is_object());
}

#[tokio::test]
async fn home_page_search_box_suggestions_and_filter_removal() {
    let Some(db) = TestDb::create("home_page_search_box_suggestions").await else {
        return;
    };
    let pool = db.migrated_pool().await;
    seed(&pool).await;
    let app = app(&pool);

    let (status, html) = get_raw(&app, "/?q=Whitechaple").await;
    assert_eq!(status, StatusCode::OK);
    assert!(html.contains(r#"role="search""#), "search form");
    assert!(html.contains(r#"value="Whitechaple""#), "keeps the query");
    assert!(html.contains("Including results for"), "{html}");
    assert!(html.contains("Paintings by Sámi artists"));
    assert!(html.contains("Best match first"));

    // Empty: a suggestion that keeps the filters, and one link per filter.
    let (_, html) = get_raw(&app, "/?q=Whitechaple&category=talk").await;
    assert!(html.contains("No events match"), "{html}");
    assert!(html.contains("Did you mean"), "{html}");
    assert!(html.contains(r#"href="/?q=whitechapel""#), "{html}");
    assert!(html.contains("(1 event without these filters)"), "{html}");
    assert!(html.contains("Try removing a filter"));
    assert!(html.contains("Type: Talk"));
    // The removal link keeps the search.
    assert!(
        html.contains(r#"href="/?q=Whitechaple&amp;from="#),
        "{html}"
    );
    assert!(html.contains("Clear the search"));

    // The date and type quick links keep the search too.
    let (_, html) = get_raw(&app, "/?q=ceramics").await;
    assert!(html.contains("category=talk") && html.contains("?q=ceramics&amp;from="));
}

#[tokio::test]
async fn rust_and_sql_accent_folding_agree() {
    let Some(db) = TestDb::create("rust_and_sql_accent_folding_agree").await else {
        return;
    };
    let pool = db.migrated_pool().await;
    let samples = [
        "Sámi ÉLAN Łódź Straße Æsop Œuvre Ørsted Þór Ðá O’Brien",
        "áÁàÀâÂäÄãÃåÅāĀăĂąĄçÇćĆĉĈčČďĎđĐéÉèÈêÊëËēĒĕĔėĖęĘěĚĝĜğĞġĠģĢĥĤħĦíÍìÌîÎïÏĩĨīĪĭĬįĮıĵĴķĶ",
        "ĺĹļĻľĽŀĿłŁñÑńŃņŅňŇóÓòÒôÔöÖõÕōŌŏŎőŐŕŔŗŖřŘśŚŝŜşŞšŠșȘţŢťŤțȚúÚùÙûÛüÜũŨūŪŭŬůŮűŰųŲŵŴýÝÿŸŷŶźŹżŻžŽ",
        "Plain ASCII 123 & punctuation!",
    ];
    for s in samples {
        let sql: String = sqlx::query_scalar("SELECT events.search_fold($1)")
            .bind(s)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(sql, musenmingle::search::fold(s), "{s}");
        assert!(sql.is_ascii(), "{sql}");
    }
}
