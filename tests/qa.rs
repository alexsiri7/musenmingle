//! Scraper QA end to end: a fake source fetching a wiremock venue, the
//! runner with a `QaChecker` against a wiremock Requesty, and a wiremock
//! GitHub. First check and issue, not re-checking, adopting an open issue,
//! closing on a clean check, the caps, running out of credit, invalid
//! answers, a failed issue update and the zero-retention gate.

mod common;

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use chrono::{DateTime, TimeZone, Utc};
use common::TestDb;
use musenmingle::config::RateLimitConfig;
use musenmingle::enrich::requesty::Requesty;
use musenmingle::enrich::store::{CallRecord, LedgerPass, record_pass_call};
use musenmingle::fetch::FetchContext;
use musenmingle::github::{GitHubIssueFiler, IssueFiler};
use musenmingle::health::{HealthChecker, HealthConfig};
use musenmingle::model::{Category, NewEvent, Price, RawEvent, SourceKind};
use musenmingle::normalise::dedupe_key;
use musenmingle::qa::{QaChecker, QaConfig};
use musenmingle::repo;
use musenmingle::runner::{RunSummary, Runner, SourceReport};
use musenmingle::sources::{Source, SourceError};
use rust_decimal::Decimal;
use serde_json::{Value, json};
use sqlx::PgPool;
use url::Url;
use wiremock::matchers::{method, path, query_param};
use wiremock::{Mock, MockServer, Request, ResponseTemplate};

const REPO: &str = "alexsiri7/musenmingle";
const TITLE: &str = "Scraper check: fake — 1 field(s) wrong, 1 missed event(s)";
const LISTING: &str = "<html><head><title>What's on</title></head><body><h1>What's on</h1><ul>\
    <li><a href=\"/e/1\">Night Talk</a> — 12 November, 8pm</li>\
    <li>Print Fair — 20 November</li></ul></body></html>";
const DETAIL: &str = "<html><head><script type=\"application/ld+json\">\
    {\"@type\":\"Event\",\"name\":\"Night Talk\",\"startDate\":\"2026-11-12T20:00\"}</script></head>\
    <body><h1>Night Talk</h1><p>Thursday 12 November, 8pm. Tickets £5.</p></body></html>";

fn now() -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 10, 1, 9, 0, 0).unwrap()
}

/// London midnight (GMT in November): the time is missing.
fn stored_start() -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 11, 12, 0, 0, 0).unwrap()
}

/// Fetches the venue's listing and one detail page; emits one event with
/// the time missing.
struct VenueSource {
    key: String,
    base: Url,
}

#[async_trait]
impl Source for VenueSource {
    fn key(&self) -> &str {
        &self.key
    }

    async fn fetch(&self, ctx: &FetchContext) -> Result<Vec<RawEvent>, SourceError> {
        ctx.get_text(&self.base.join("/whats-on").unwrap()).await?;
        let detail = self.base.join("/e/1").unwrap();
        ctx.get_text(&detail).await?;
        Ok(vec![RawEvent {
            source_event_id: "1".into(),
            source_url: Some(detail.to_string()),
            payload: json!({}),
        }])
    }

    fn normalise(&self, raw: &RawEvent) -> Result<Option<NewEvent>, SourceError> {
        Ok(Some(NewEvent {
            sessions: Vec::new(),
            dedupe_key: dedupe_key("Night Talk", stored_start(), Some("Venue")),
            title: "Night Talk".into(),
            description: None,
            venue_name: Some("Venue".into()),
            address: None,
            lat: Some(51.5),
            lng: Some(-0.1),
            starts_at: stored_start(),
            ends_at: None,
            all_day: false,
            price: Price::default(),
            url: raw.source_url.clone(),
            image_url: None,
            category: Category::Talk,
            tags: vec![],
        }))
    }
}

struct Env {
    db: TestDb,
    pool: PgPool,
    venue: MockServer,
    requesty: MockServer,
    github: MockServer,
}

impl Env {
    async fn new(name: &str, keys: &[&str]) -> Option<Env> {
        let db = TestDb::create(name).await?;
        let pool = db.migrated_pool().await;
        sqlx::query("UPDATE events.sources SET enabled = false")
            .execute(&pool)
            .await
            .unwrap();
        let venue = MockServer::start().await;
        for (p, body) in [("/whats-on", LISTING), ("/e/1", DETAIL)] {
            Mock::given(method("GET"))
                .and(path(p))
                .respond_with(ResponseTemplate::new(200).set_body_string(body))
                .mount(&venue)
                .await;
        }
        for key in keys {
            repo::upsert_source(&pool, key, SourceKind::Scraper, &venue.uri(), 1440, true)
                .await
                .unwrap();
        }
        Some(Env {
            db,
            pool,
            venue,
            requesty: MockServer::start().await,
            github: MockServer::start().await,
        })
    }

    fn runner(&self, config: QaConfig) -> Runner {
        let base = Url::parse(&self.venue.uri()).unwrap();
        let filer: Box<dyn IssueFiler> =
            Box::new(GitHubIssueFiler::new(&self.github.uri(), REPO, "test-token").unwrap());
        Runner {
            pool: self.pool.clone(),
            ctx: FetchContext::new_allowing_loopback(RateLimitConfig::disabled()).unwrap(),
            factory: Box::new(move |row| {
                Ok(Box::new(VenueSource {
                    key: row.key.clone(),
                    base: base.clone(),
                }))
            }),
            health: HealthChecker::new(HealthConfig::default(), Some(filer)),
            source_timeout: Duration::from_secs(10),
            enrich: None,
            qa: Some(QaChecker {
                client: Requesty::new(&self.requesty.uri(), "test-key").unwrap(),
                config,
            }),
            venues: None,
            form_issues: Default::default(),
        }
    }

    /// One ingest tick with every source due.
    async fn tick(&self, runner: &Runner) -> Vec<SourceReport> {
        sqlx::query("UPDATE events.sources SET last_run_at = NULL")
            .execute(&self.pool)
            .await
            .unwrap();
        let RunSummary::Ran(reports) = runner.run_once(now()).await.unwrap() else {
            panic!("expected a run");
        };
        reports
    }

    async fn checks(&self) -> Vec<(String, String, i32, i32, Option<i64>, Option<String>)> {
        sqlx::query_as(
            "SELECT reason, status, wrong_fields, missed_events, github_issue_number, error
             FROM events.qa_checks ORDER BY id",
        )
        .fetch_all(&self.pool)
        .await
        .unwrap()
    }

    async fn done(self) {
        self.pool.close().await;
        self.db.drop_db().await;
    }
}

fn config() -> QaConfig {
    QaConfig {
        call_timeout: Duration::from_secs(5),
        ..Default::default()
    }
}

fn chat_reply(answer: Value) -> ResponseTemplate {
    ResponseTemplate::new(200).set_body_json(json!({
        "choices": [{
            "message": { "role": "assistant", "content": answer.to_string() },
            "finish_reason": "stop"
        }],
        "usage": { "prompt_tokens": 3000, "completion_tokens": 500 }
    }))
}

fn wrong_start_and_missed() -> Value {
    json!({
        "findings": [
            {"id": "r1", "field": "title", "page_says": "Night Talk", "verdict": "correct",
             "evidence_quote": "Night Talk"},
            {"id": "r1", "field": "starts_at", "page_says": "12 November 2026, 20:00",
             "verdict": "wrong", "evidence_quote": "Thursday 12 November, 8pm"}
        ],
        "missed_events": [{"title": "Print Fair", "evidence_quote": "Print Fair — 20 November"}]
    })
}

fn all_correct() -> Value {
    json!({
        "findings": [{"id": "r1", "field": "title", "page_says": "Night Talk",
                      "verdict": "correct", "evidence_quote": "Night Talk"}],
        "missed_events": []
    })
}

/// Answers every chat call with `answer`, counting calls.
async fn mount_chat(server: &MockServer, answer: Value, calls: Arc<AtomicUsize>) {
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(move |_: &Request| {
            calls.fetch_add(1, Ordering::SeqCst);
            chat_reply(answer.clone())
        })
        .mount(server)
        .await;
}

async fn mock_list(server: &MockServer, issues: Value, times: u64) {
    Mock::given(method("GET"))
        .and(path(format!("/repos/{REPO}/issues")))
        .and(query_param("labels", "scraper-broken"))
        .respond_with(ResponseTemplate::new(200).set_body_json(issues))
        .expect(times)
        .mount(server)
        .await;
}

async fn spend(pool: &PgPool, pass: LedgerPass, usd: i64) {
    let rec = CallRecord {
        model: "anthropic/claude-opus-5-5".into(),
        cost_usd: Decimal::from(usd),
        ok: true,
        ..Default::default()
    };
    record_pass_call(pool, pass, &rec, now() - chrono::Duration::hours(1))
        .await
        .unwrap();
}

#[tokio::test]
async fn first_check_files_one_issue_and_never_changes_events() {
    let Some(env) = Env::new(
        "first_check_files_one_issue_and_never_changes_events",
        &["fake"],
    )
    .await
    else {
        return;
    };
    let calls = Arc::new(AtomicUsize::new(0));
    mount_chat(&env.requesty, wrong_start_and_missed(), calls.clone()).await;
    // Listed once by the health checker (per runner) and once to file.
    mock_list(&env.github, json!([]), 2).await;
    Mock::given(method("POST"))
        .and(path(format!("/repos/{REPO}/issues")))
        .respond_with(
            ResponseTemplate::new(201).set_body_json(json!({"number": 42, "title": TITLE})),
        )
        .expect(1)
        .mount(&env.github)
        .await;
    // The weekly re-check reports on the open issue.
    Mock::given(method("POST"))
        .and(path(format!("/repos/{REPO}/issues/42/comments")))
        .respond_with(ResponseTemplate::new(201).set_body_json(json!({})))
        .expect(1)
        .mount(&env.github)
        .await;
    let runner = env.runner(config());

    let reports = env.tick(&runner).await;
    assert_eq!(reports[0].qa_check.as_deref(), Some("issues"));
    assert_eq!(reports[0].qa_findings, 1);
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_eq!(
        env.checks().await,
        [(
            "first".to_string(),
            "issues".to_string(),
            1,
            1,
            Some(42),
            None
        )]
    );
    let (rule, examples): (String, Value) =
        sqlx::query_as("SELECT rule, examples FROM events.qa_findings")
            .fetch_one(&env.pool)
            .await
            .unwrap();
    assert_eq!(rule, "midnight_not_all_day");
    assert_eq!(examples[0]["starts_at"], "2026-11-12 00:00");
    let (pass, cost): (String, Decimal) =
        sqlx::query_as("SELECT pass, cost_usd FROM events.enrichment_calls")
            .fetch_one(&env.pool)
            .await
            .unwrap();
    // 3000 in at $4/M + 500 out at $20/M.
    assert_eq!((pass.as_str(), cost), ("qa", Decimal::new(22, 3)));
    let (pages, verdict): (Value, Value) =
        sqlx::query_as("SELECT pages, verdict FROM events.qa_checks")
            .fetch_one(&env.pool)
            .await
            .unwrap();
    assert_eq!(pages[0]["kind"], "listing");
    assert_eq!(pages[1]["url"], format!("{}/e/1", env.venue.uri()));
    assert_eq!(verdict["findings"][1]["ours"], "2026-11-12 00:00");

    // What the judge saw: the pages' text and our record, not our HTML.
    let chat = &env.requesty.received_requests().await.unwrap()[0];
    let body: Value = serde_json::from_slice(&chat.body).unwrap();
    assert_eq!(body["response_format"]["type"], "json_object");
    assert!(body.get("requesty").is_none());
    let input: Value =
        serde_json::from_str(body["messages"][1]["content"].as_str().unwrap()).unwrap();
    assert_eq!(input["records"][0]["starts_at"], "2026-11-12 00:00");
    assert!(
        input["pages"][1]["text"]
            .as_str()
            .unwrap()
            .contains("Thursday 12 November, 8pm")
    );
    assert_eq!(
        input["pages"][1]["json_ld"][0]["startDate"],
        "2026-11-12T20:00"
    );

    // The issue: table, fixture path, page URL, no page HTML.
    let reqs = env.github.received_requests().await.unwrap();
    let create = reqs.iter().find(|r| r.method.as_str() == "POST").unwrap();
    let issue: Value = serde_json::from_slice(&create.body).unwrap();
    assert_eq!(issue["title"], TITLE);
    assert_eq!(issue["labels"], json!(["scraper-broken"]));
    let text = issue["body"].as_str().unwrap();
    assert!(
        text.contains("| `Night Talk` | starts_at | `2026-11-12 00:00` |"),
        "{text}"
    );
    assert!(text.contains("tests/fixtures/scrapers/fake/qa-2026-10-01.html"));
    assert!(text.contains(&format!("{}/e/1", env.venue.uri())));
    assert!(text.contains("| `Print Fair` |"));
    assert!(!text.contains("<h1>"), "{text}");

    // The AI never supplies data.
    let stored: (DateTime<Utc>, bool) =
        sqlx::query_as("SELECT starts_at, all_day FROM events.events")
            .fetch_one(&env.pool)
            .await
            .unwrap();
    assert_eq!(stored, (stored_start(), false));

    // Same code, same rules, checked today: not due again.
    let reports = env.tick(&runner).await;
    assert_eq!(reports[0].qa_check, None);
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_eq!(env.checks().await.len(), 1);
    // A week later it is.
    sqlx::query("UPDATE events.qa_checks SET checked_at = checked_at - interval '7 days'")
        .execute(&env.pool)
        .await
        .unwrap();
    let reports = env.tick(&runner).await;
    assert_eq!(reports[0].qa_check.as_deref(), Some("issues"));
    let weekly = env.checks().await.pop().unwrap();
    assert_eq!((weekly.0.as_str(), weekly.4), ("weekly", Some(42)));
    env.done().await;
}

#[tokio::test]
async fn a_new_rule_hit_rechecks_and_adopts_the_open_issue() {
    let Some(env) = Env::new(
        "a_new_rule_hit_rechecks_and_adopts_the_open_issue",
        &["fake"],
    )
    .await
    else {
        return;
    };
    let calls = Arc::new(AtomicUsize::new(0));
    mount_chat(&env.requesty, wrong_start_and_missed(), calls.clone()).await;
    // Our qa_issues row is gone (e.g. a DB reset) but the issue is open.
    mock_list(
        &env.github,
        json!([
            {"number": 4, "title": "Scraper check: fake-2 — 1 field(s) wrong"},
            {"number": 5, "title": "Scraper check: fake — 3 field(s) wrong"}
        ]),
        2,
    )
    .await;
    Mock::given(method("POST"))
        .and(path(format!("/repos/{REPO}/issues/5/comments")))
        .respond_with(ResponseTemplate::new(201).set_body_json(json!({})))
        .expect(1)
        .mount(&env.github)
        .await;
    Mock::given(method("POST"))
        .and(path(format!("/repos/{REPO}/issues")))
        .respond_with(ResponseTemplate::new(201).set_body_json(json!({"number": 99, "title": "x"})))
        .expect(0)
        .mount(&env.github)
        .await;
    // Checked yesterday, when no rule fired.
    sqlx::query(
        "INSERT INTO events.qa_checks (source_id, checked_at, reason, code_hash, model,
             prompt_version, status)
         SELECT id, $1, 'first', '', 'm', 1, 'ok' FROM events.sources WHERE key = 'fake'",
    )
    .bind(now() - chrono::Duration::days(1))
    .execute(&env.pool)
    .await
    .unwrap();
    let runner = env.runner(config());

    // The first run hits a rule; the next one is checked for it.
    assert_eq!(env.tick(&runner).await[0].qa_check, None);
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    assert_eq!(
        env.tick(&runner).await[0].qa_check.as_deref(),
        Some("issues")
    );
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    let latest = env.checks().await.pop().unwrap();
    assert_eq!((latest.0.as_str(), latest.4), ("rule_hit", Some(5)));
    let open: i64 = sqlx::query_scalar(
        "SELECT github_issue_number FROM events.qa_issues WHERE closed_at IS NULL",
    )
    .fetch_one(&env.pool)
    .await
    .unwrap();
    assert_eq!(open, 5);
    env.done().await;
}

#[tokio::test]
async fn a_clean_check_closes_the_open_issue() {
    let Some(env) = Env::new("a_clean_check_closes_the_open_issue", &["fake"]).await else {
        return;
    };
    let calls = Arc::new(AtomicUsize::new(0));
    mount_chat(&env.requesty, all_correct(), calls.clone()).await;
    sqlx::query(
        "INSERT INTO events.qa_issues (source_id, github_issue_number)
         SELECT id, 5 FROM events.sources WHERE key = 'fake'",
    )
    .execute(&env.pool)
    .await
    .unwrap();
    Mock::given(method("POST"))
        .and(path(format!("/repos/{REPO}/issues/5/comments")))
        .respond_with(ResponseTemplate::new(201).set_body_json(json!({})))
        .expect(1)
        .mount(&env.github)
        .await;
    Mock::given(method("PATCH"))
        .and(path(format!("/repos/{REPO}/issues/5")))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"number": 5, "title": "x"})))
        .expect(1)
        .mount(&env.github)
        .await;
    let runner = env.runner(config());

    assert_eq!(env.tick(&runner).await[0].qa_check.as_deref(), Some("ok"));
    let check = env.checks().await.pop().unwrap();
    assert_eq!((check.1.as_str(), check.4), ("ok", None));
    let closed: Option<DateTime<Utc>> =
        sqlx::query_scalar("SELECT closed_at FROM events.qa_issues")
            .fetch_one(&env.pool)
            .await
            .unwrap();
    assert_eq!(closed, Some(now()));
    env.done().await;
}

#[tokio::test]
async fn caps_bound_checks_and_are_separate_from_enrichment() {
    let Some(env) = Env::new(
        "caps_bound_checks_and_are_separate_from_enrichment",
        &["fake", "fake-2"],
    )
    .await
    else {
        return;
    };
    let calls = Arc::new(AtomicUsize::new(0));
    mount_chat(&env.requesty, all_correct(), calls.clone()).await;

    // A cap below one pessimistic check: no call, nothing stored.
    let tiny = env.runner(QaConfig {
        daily_cap_usd: Decimal::new(1, 2),
        ..config()
    });
    env.tick(&tiny).await;
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    assert!(env.checks().await.is_empty());

    // QA's own spend counts; enrichment's does not.
    let one_per_tick = env.runner(QaConfig {
        max_checks_per_run: 1,
        ..config()
    });
    spend(&env.pool, LedgerPass::Qa, 1).await;
    env.tick(&one_per_tick).await;
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    sqlx::query("DELETE FROM events.enrichment_calls")
        .execute(&env.pool)
        .await
        .unwrap();
    spend(&env.pool, LedgerPass::Enrich, 5).await;

    // Both sources are due; one check per tick.
    let reports = env.tick(&one_per_tick).await;
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    let statuses: Vec<Option<&str>> = reports.iter().map(|r| r.qa_check.as_deref()).collect();
    assert_eq!(statuses, [Some("ok"), None]);
    env.tick(&one_per_tick).await;
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    assert_eq!(env.checks().await.len(), 2);
    env.done().await;
}

#[tokio::test]
async fn an_answer_rejected_twice_is_invalid_and_files_nothing() {
    let Some(env) = Env::new(
        "an_answer_rejected_twice_is_invalid_and_files_nothing",
        &["fake"],
    )
    .await
    else {
        return;
    };
    let calls = Arc::new(AtomicUsize::new(0));
    let mut answer = wrong_start_and_missed();
    answer["findings"][1]["evidence_quote"] = json!("Friday 13 November, 9pm");
    mount_chat(&env.requesty, answer, calls.clone()).await;
    let runner = env.runner(config());

    assert_eq!(
        env.tick(&runner).await[0].qa_check.as_deref(),
        Some("invalid")
    );
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    let check = env.checks().await.pop().unwrap();
    assert_eq!((check.1.as_str(), check.4), ("invalid", None));
    assert!(check.5.unwrap().contains("not copied verbatim"));
    // The retry carried the validator's complaint.
    let retry = &env.requesty.received_requests().await.unwrap()[1];
    let body: Value = serde_json::from_slice(&retry.body).unwrap();
    assert!(
        body["messages"][2]["content"]
            .as_str()
            .unwrap()
            .contains("rejected by our validator")
    );
    let ledger: i64 =
        sqlx::query_scalar("SELECT count(*) FROM events.enrichment_calls WHERE pass = 'qa'")
            .fetch_one(&env.pool)
            .await
            .unwrap();
    assert_eq!(ledger, 2);
    let writes = env
        .github
        .received_requests()
        .await
        .unwrap()
        .into_iter()
        .filter(|r| r.method.as_str() != "GET")
        .count();
    assert_eq!(writes, 0);
    env.done().await;
}

#[tokio::test]
async fn a_model_that_keeps_data_is_never_called() {
    let Some(env) = Env::new("qa_model_that_keeps_data_is_never_called", &["fake"]).await else {
        return;
    };
    let calls = Arc::new(AtomicUsize::new(0));
    mount_chat(&env.requesty, all_correct(), calls.clone()).await;
    // Seeded with 30-day retention.
    let runner = env.runner(QaConfig {
        model: "anthropic/claude-sonnet-5".into(),
        ..config()
    });
    let reports = env.tick(&runner).await;
    assert_eq!(reports[0].qa_check, None);
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    assert!(env.checks().await.is_empty());
    env.done().await;
}

#[tokio::test]
async fn out_of_credit_stops_checks_for_the_rest_of_the_tick() {
    let Some(env) = Env::new(
        "qa_out_of_credit_stops_checks_for_the_rest_of_the_tick",
        &["fake", "fake-2"],
    )
    .await
    else {
        return;
    };
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(
            ResponseTemplate::new(402)
                .set_body_json(json!({ "error": { "message": "organization balance exhausted" } })),
        )
        .mount(&env.requesty)
        .await;
    let runner = env.runner(config());

    let reports = env.tick(&runner).await;
    let statuses: Vec<Option<&str>> = reports.iter().map(|r| r.qa_check.as_deref()).collect();
    assert_eq!(statuses, [Some("failed"), None]);
    assert_eq!(env.requesty.received_requests().await.unwrap().len(), 1);
    assert_eq!(env.checks().await.len(), 1);
    env.done().await;
}

#[tokio::test]
async fn a_failed_issue_update_is_not_a_finished_check() {
    let Some(env) = Env::new(
        "qa_a_failed_issue_update_is_not_a_finished_check",
        &["fake"],
    )
    .await
    else {
        return;
    };
    let calls = Arc::new(AtomicUsize::new(0));
    mount_chat(&env.requesty, wrong_start_and_missed(), calls.clone()).await;
    Mock::given(method("GET"))
        .and(path(format!("/repos/{REPO}/issues")))
        .respond_with(ResponseTemplate::new(500))
        .mount(&env.github)
        .await;
    let runner = env.runner(config());

    assert_eq!(env.tick(&runner).await[0].qa_check, None);
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    let check = env.checks().await.pop().unwrap();
    assert_eq!((check.1.as_str(), check.4), ("issues", None));
    env.done().await;
}
