//! Scraper QA: noticing when a scraper stores something other than what the
//! venue's page says (#98).
//!
//! 1. **Rules** ([`rules`]), after every successful run, no AI: impossible
//!    or suspicious dates, duplicates, jumps in missing venues/coordinates,
//!    count drops. Stored in `events.qa_findings`.
//! 2. **AI check** ([`QaChecker`]), inside the ingest tick, for a source that
//!    was never checked, whose code changed ([`code`]), whose last check is a
//!    week old or whose latest run hit a new rule. The runner turns on page
//!    capture in `FetchContext` for that source's run; the check sends the
//!    pages the run fetched ([`input`]: main-content text and JSON-LD) and
//!    what we extracted from them to a zero-retention model through
//!    Requesty, and validates its verdict strictly ([`output`]). Calls go in
//!    the `events.enrichment_calls` ledger (`pass = 'qa'`) under their own
//!    daily cap (`QA_DAILY_CAP_USD`); results in `events.qa_checks` (never
//!    the page text).
//! 3. **Issues** ([`issue`]): wrong fields and missed events become one
//!    deduped `scraper-broken` issue per source, closed by a clean check.
//!
//! The AI verifies but never supplies data: nothing it says is written to
//! `events.events`; fixes go through scraper code.

pub mod code;
pub mod input;
pub mod issue;
pub mod output;
pub mod rules;
pub mod store;

use std::time::Duration;

use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use rust_decimal::prelude::FromPrimitive;
use serde_json::{Value, json};
use sqlx::PgPool;
use url::Url;

use crate::enrich::requesty::{CallError, Requesty};
use crate::enrich::store::{CallRecord, LedgerPass, ModelPrice, model_price, record_pass_call};
use crate::enrich::{call_cost, estimate_cost, london_midnight};
use crate::fetch::{CapturedPage, FetchContext, redact};
use crate::github::IssueFiler;
use crate::normalise::london_date;
use crate::repo::SourceRow;
use crate::runner::{RunEvent, SourceRun};
use crate::sources::Source;
use input::{MAX_INPUT_CHARS, PageIn, PageKind};
use issue::IssueAction;
use output::{QA_PROMPT_VERSION, SYSTEM_PROMPT, Verdict};
use rules::Finding;

/// Output tokens allowed per call.
const MAX_TOKENS: i64 = 4000;
/// Records checked against their detail page per check.
const DETAIL_RECORDS: usize = 2;
/// Detail pages fetched when the run's own fetches don't cover them.
const MAX_DETAIL_FETCHES: usize = 4;

/// Scraper QA settings (`QA_*`; see README).
#[derive(Debug, Clone, PartialEq)]
pub struct QaConfig {
    pub model: String,
    pub daily_cap_usd: Decimal,
    /// AI checks per ingest tick; 0 = off.
    pub max_checks_per_run: usize,
    pub call_timeout: Duration,
    pub recheck_after: chrono::Duration,
}

impl Default for QaConfig {
    fn default() -> Self {
        Self {
            model: crate::enrich::DEFAULT_MODEL.into(),
            daily_cap_usd: Decimal::ONE,
            max_checks_per_run: 2,
            call_timeout: Duration::from_secs(150),
            recheck_after: chrono::Duration::days(7),
        }
    }
}

/// Why a check is due.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DueReason {
    First,
    CodeChanged,
    Weekly,
    RuleHit,
}

impl DueReason {
    pub fn as_str(self) -> &'static str {
        match self {
            DueReason::First => "first",
            DueReason::CodeChanged => "code_changed",
            DueReason::Weekly => "weekly",
            DueReason::RuleHit => "rule_hit",
        }
    }
}

/// Whether a source is due a check, from its latest completed check, its
/// current code hash (`""` = unknown) and the rules its latest run hit.
pub fn due_reason(
    latest: Option<&store::LatestCheck>,
    code_hash: &str,
    latest_rules: &[String],
    now: DateTime<Utc>,
    recheck_after: chrono::Duration,
) -> Option<DueReason> {
    let Some(last) = latest else {
        return Some(DueReason::First);
    };
    if !code_hash.is_empty() && code_hash != last.code_hash {
        Some(DueReason::CodeChanged)
    } else if now - last.checked_at >= recheck_after {
        Some(DueReason::Weekly)
    } else if latest_rules.iter().any(|r| !last.rules_hit.contains(r)) {
        Some(DueReason::RuleHit)
    } else {
        None
    }
}

/// State shared by the checks of one ingest tick.
#[derive(Debug, Default)]
pub struct QaTick {
    pub checks_made: usize,
    /// Requesty said credits are exhausted: no more checks this tick.
    pub credits_out: bool,
}

/// A check to make after the source's run.
#[derive(Debug, Clone)]
pub struct QaDue {
    pub reason: DueReason,
    pub code_hash: String,
    price: ModelPrice,
}

/// What a check did (logged by the runner).
#[derive(Debug, Clone, PartialEq)]
pub struct QaOutcome {
    /// A `qa_checks.status`, or `skipped` (over budget; nothing stored).
    pub status: &'static str,
    pub check_id: Option<i64>,
    pub wrong: usize,
    pub missed: usize,
    pub cost_usd: Decimal,
    pub issue: Option<IssueAction>,
}

impl QaOutcome {
    fn skipped() -> Self {
        Self {
            status: "skipped",
            check_id: None,
            wrong: 0,
            missed: 0,
            cost_usd: Decimal::ZERO,
            issue: None,
        }
    }
}

/// Query-less, fragment-less, trailing-slash-less form of a URL, for
/// matching records to fetched pages.
fn url_key(s: &str) -> Option<String> {
    Url::parse(s)
        .ok()
        .map(|u| redact(&u).trim_end_matches('/').to_string())
}

/// The run's kept records to check against their detail pages: those the
/// rules flagged first, then the first and the middle of the rest.
fn detail_candidates<'a>(events: &'a [RunEvent], findings: &[Finding]) -> Vec<&'a RunEvent> {
    let flagged: Vec<&str> = findings.iter().flat_map(Finding::example_ids).collect();
    let kept = events.iter().filter(|e| e.kept);
    let mut out: Vec<&RunEvent> = Vec::new();
    for id in &flagged {
        if let Some(e) = kept.clone().find(|e| e.source_event_id == *id)
            && !out.iter().any(|o| o.source_event_id == e.source_event_id)
        {
            out.push(e);
        }
    }
    let rest: Vec<&RunEvent> = kept
        .filter(|e| !flagged.contains(&e.source_event_id.as_str()))
        .collect();
    for i in [0, rest.len() / 2] {
        if let Some(e) = rest.get(i)
            && !out.iter().any(|o| o.source_event_id == e.source_event_id)
        {
            out.push(e);
        }
    }
    out
}

/// How the judge's answer ended.
enum Answer {
    Valid(Verdict),
    /// Rejected twice (the problems).
    Invalid(String),
    /// Rejected once; the retry did not fit the daily cap (`failed`: a
    /// later run checks again).
    NoRetry,
}

/// A page shown to the judge.
struct Shown {
    kind: PageKind,
    url: String,
    body: String,
    json: bool,
}

/// The scraper QA check with its client.
pub struct QaChecker {
    pub client: Requesty,
    pub config: QaConfig,
}

impl QaChecker {
    /// Whether to check `source` after its run this tick (then the runner
    /// captures the run's pages).
    pub async fn plan(
        &self,
        pool: &PgPool,
        row: &SourceRow,
        source: &dyn Source,
        now: DateTime<Utc>,
        tick: &QaTick,
    ) -> anyhow::Result<Option<QaDue>> {
        if tick.credits_out || tick.checks_made >= self.config.max_checks_per_run {
            return Ok(None);
        }
        let code_hash = code::code_hash(source);
        let latest = store::latest_check(pool, row.id).await?;
        let rules = store::latest_run_rules(pool, row.id).await?;
        let Some(reason) = due_reason(
            latest.as_ref(),
            &code_hash,
            &rules,
            now,
            self.config.recheck_after,
        ) else {
            return Ok(None);
        };
        let price = match model_price(pool, &self.config.model).await? {
            None => {
                tracing::warn!(
                    model = %self.config.model,
                    "no events.model_prices row for QA_MODEL; scraper checks are off"
                );
                return Ok(None);
            }
            // /about promises that the model seeing page text keeps nothing.
            Some(p) if p.retention_days != Some(0) => {
                tracing::warn!(
                    model = %self.config.model,
                    retention_days = ?p.retention_days,
                    "QA_MODEL is not zero-retention in events.model_prices; /about promises zero retention, so scraper checks are off"
                );
                return Ok(None);
            }
            Some(p) => p,
        };
        let worst = MAX_INPUT_CHARS + SYSTEM_PROMPT.len();
        if !self.fits(pool, now, worst, &price).await? {
            tracing::info!(source = %row.key, reason = reason.as_str(), "scraper check due but the QA daily cap is reached");
            return Ok(None);
        }
        Ok(Some(QaDue {
            reason,
            code_hash,
            price,
        }))
    }

    async fn fits(
        &self,
        pool: &PgPool,
        now: DateTime<Utc>,
        prompt_chars: usize,
        price: &ModelPrice,
    ) -> sqlx::Result<bool> {
        let spent = store::qa_spent_since(pool, london_midnight(now)).await?;
        Ok(spent + estimate_cost(prompt_chars, MAX_TOKENS, price) <= self.config.daily_cap_usd)
    }

    fn chat_body(&self, message: &str, reminder: Option<&str>) -> Value {
        let mut messages = vec![
            json!({ "role": "system", "content": SYSTEM_PROMPT }),
            json!({ "role": "user", "content": message }),
        ];
        if let Some(r) = reminder {
            messages.push(json!({ "role": "user", "content": format!(
                "Your previous answer was rejected by our validator:\n{r}\n\
                 Answer again following every rule exactly."
            ) }));
        }
        json!({
            "model": self.config.model,
            "messages": messages,
            "response_format": { "type": "json_object" },
            "max_tokens": MAX_TOKENS,
            "temperature": 0,
            "reasoning_effort": "low",
        })
    }

    /// Pick the records to check and their detail pages: pages the run
    /// fetched, else up to [`MAX_DETAIL_FETCHES`] fetches through `ctx`.
    async fn details<'a>(
        &self,
        ctx: &FetchContext,
        run: &'a SourceRun,
        captured: &[CapturedPage],
        findings: &[Finding],
    ) -> Vec<(&'a RunEvent, Shown)> {
        let candidates = detail_candidates(&run.events, findings);
        let mut out: Vec<(&RunEvent, Shown)> = Vec::new();
        let mut unmatched = Vec::new();
        for c in candidates {
            let keys: Vec<String> = [c.source_url.as_deref(), c.event.url.as_deref()]
                .into_iter()
                .flatten()
                .filter_map(url_key)
                .collect();
            let page = captured
                .iter()
                .skip(1)
                .find(|p| url_key(&p.url).is_some_and(|k| keys.contains(&k)));
            match page {
                Some(p) if out.len() < DETAIL_RECORDS => out.push((
                    c,
                    Shown {
                        kind: if p.json {
                            PageKind::Api
                        } else {
                            PageKind::Detail
                        },
                        url: p.url.clone(),
                        body: p.body.clone(),
                        json: p.json,
                    },
                )),
                Some(_) => {}
                None => unmatched.push(c),
            }
        }
        let mut attempts = 0;
        for c in unmatched {
            if out.len() >= DETAIL_RECORDS || attempts >= MAX_DETAIL_FETCHES {
                break;
            }
            let Some(url) = c
                .event
                .url
                .as_deref()
                .and_then(|u| Url::parse(u).ok())
                .filter(|u| matches!(u.scheme(), "http" | "https"))
            else {
                continue;
            };
            attempts += 1;
            match ctx.get_text(&url).await {
                Ok(body) => out.push((
                    c,
                    Shown {
                        kind: PageKind::Detail,
                        url: redact(&url),
                        body,
                        json: false,
                    },
                )),
                Err(e) => {
                    tracing::info!(url = %redact(&url), error = %e, "scraper check: detail page not fetched")
                }
            }
        }
        // Not the source's errors: they must not reach its next run.
        let _ = ctx.take_errors();
        out
    }

    /// Check a source's run against the pages it fetched. `findings` are
    /// the run's rule hits. Errors are for the database only.
    #[allow(clippy::too_many_arguments)]
    pub async fn check(
        &self,
        pool: &PgPool,
        ctx: &FetchContext,
        row: &SourceRow,
        run: &SourceRun,
        captured: Vec<CapturedPage>,
        findings: &[Finding],
        due: QaDue,
        filer: Option<&dyn IssueFiler>,
        now: DateTime<Utc>,
        tick: &mut QaTick,
    ) -> anyhow::Result<QaOutcome> {
        tick.checks_made += 1;
        let mut rules_hit: Vec<String> = findings
            .iter()
            .map(|f| f.rule.as_str().to_string())
            .collect();
        rules_hit.sort();
        let details = self.details(ctx, run, &captured, findings).await;
        let mut shown: Vec<Shown> = Vec::new();
        if let Some(first) = captured.first() {
            shown.push(Shown {
                kind: if first.json {
                    PageKind::Api
                } else {
                    PageKind::Listing
                },
                url: first.url.clone(),
                body: first.body.clone(),
                json: first.json,
            });
        }
        let offset = shown.len();
        let detail_records: Vec<(&RunEvent, usize)> = details
            .iter()
            .enumerate()
            .map(|(i, (r, _))| (*r, offset + i))
            .collect();
        shown.extend(details.into_iter().map(|(_, s)| s));
        let row_for = |status: &'static str| store::NewCheck {
            source_id: row.id,
            run_id: run.run_id,
            checked_at: now,
            reason: due.reason.as_str(),
            code_hash: &due.code_hash,
            rules_hit: &rules_hit,
            pages: json!([]),
            model: &self.config.model,
            prompt_version: QA_PROMPT_VERSION,
            cost_usd: Decimal::ZERO,
            status,
            wrong_fields: 0,
            missed_events: 0,
            verdict: None,
            error: None,
        };
        if shown.is_empty() {
            let id = store::insert_check(pool, &row_for("no_pages")).await?;
            return Ok(QaOutcome {
                status: "no_pages",
                check_id: Some(id),
                ..QaOutcome::skipped()
            });
        }

        let pages: Vec<PageIn<'_>> = shown
            .iter()
            .map(|s| PageIn {
                kind: s.kind,
                url: &s.url,
                body: &s.body,
                json: s.json,
            })
            .collect();
        let input = input::build(
            &row.key,
            london_date(now),
            &pages,
            &detail_records,
            &run.events,
        );
        let mut cost = Decimal::ZERO;
        let mut reminder: Option<String> = None;
        let answer = loop {
            let body = self.chat_body(&input.message, reminder.as_deref());
            let chars = body["messages"].to_string().len();
            if !self.fits(pool, now, chars, &due.price).await? {
                if reminder.is_none() {
                    return Ok(QaOutcome::skipped());
                }
                break Answer::NoRetry;
            }
            let completion = match self.client.chat(&body, self.config.call_timeout).await {
                Ok(c) => c,
                Err(err) => {
                    self.ledger(
                        pool,
                        input.record_ids.len(),
                        CallRecord {
                            ok: false,
                            error: Some(err.to_string()),
                            ..Default::default()
                        },
                    )
                    .await;
                    if matches!(err, CallError::CreditsExhausted { .. }) {
                        tick.credits_out = true;
                    }
                    let id = store::insert_check(
                        pool,
                        &store::NewCheck {
                            pages: input.pages_meta.clone(),
                            cost_usd: cost,
                            error: Some(err.to_string()),
                            ..row_for("failed")
                        },
                    )
                    .await?;
                    return Ok(QaOutcome {
                        status: "failed",
                        check_id: Some(id),
                        cost_usd: cost,
                        ..QaOutcome::skipped()
                    });
                }
            };
            let call = call_cost(&completion.usage, &due.price);
            cost += call;
            let result = output::validate(&completion.content, &input, &run.events);
            let u = completion.usage;
            self.ledger(
                pool,
                input.record_ids.len(),
                CallRecord {
                    events_ok: i32::from(result.is_ok()),
                    tokens_in: u.prompt_tokens,
                    tokens_cached: u.cached_tokens,
                    tokens_cache_write: u.cache_write_tokens,
                    tokens_out: u.completion_tokens,
                    cost_usd: call,
                    provider_cost_usd: u.provider_cost_usd.and_then(Decimal::from_f64),
                    ok: true,
                    error: result.as_ref().err().cloned(),
                    ..Default::default()
                },
            )
            .await;
            match result {
                Ok(v) => break Answer::Valid(v),
                Err(problems) if reminder.is_none() => {
                    tracing::info!(source = %row.key, %problems, "scraper check answer rejected; retrying once");
                    reminder = Some(problems);
                }
                Err(problems) => break Answer::Invalid(problems),
            }
        };

        let verdict = match answer {
            Answer::Valid(v) => v,
            Answer::Invalid(_) | Answer::NoRetry => {
                let (status, error) = match answer {
                    Answer::Invalid(problems) => ("invalid", problems),
                    _ => ("failed", "daily cap reached before the retry".to_string()),
                };
                let id = store::insert_check(
                    pool,
                    &store::NewCheck {
                        pages: input.pages_meta.clone(),
                        cost_usd: cost,
                        error: Some(error),
                        ..row_for(status)
                    },
                )
                .await?;
                return Ok(QaOutcome {
                    status,
                    check_id: Some(id),
                    cost_usd: cost,
                    ..QaOutcome::skipped()
                });
            }
        };
        let (wrong, missed) = (verdict.wrong_count(), verdict.missed.len());
        let status = verdict.status();
        let id = store::insert_check(
            pool,
            &store::NewCheck {
                pages: input.pages_meta.clone(),
                cost_usd: cost,
                wrong_fields: i32::try_from(wrong).unwrap_or(i32::MAX),
                missed_events: i32::try_from(missed).unwrap_or(i32::MAX),
                verdict: Some(serde_json::to_value(&verdict)?),
                ..row_for(status)
            },
        )
        .await?;

        let issue = match filer {
            None => None,
            Some(filer) => {
                let report_pages: Vec<(String, String)> = shown
                    .iter()
                    .map(|s| (s.kind.as_str().to_string(), s.url.clone()))
                    .collect();
                let title = issue::title(&row.key, wrong, missed);
                let body = issue::body(&issue::Report {
                    row,
                    check_id: id,
                    reason: due.reason.as_str(),
                    model: &self.config.model,
                    pages: &report_pages,
                    verdict: &verdict,
                    fetched_on: london_date(now),
                });
                let report = (status == "issues").then_some((title.as_str(), body.as_str()));
                match issue::sync(pool, filer, row, report, now).await {
                    Ok(action) => action,
                    Err(e) => {
                        tracing::error!(source = %row.key, error = %e, "scraper check issue update failed");
                        None
                    }
                }
            }
        };
        if let Some(a) = issue
            && a != IssueAction::Closed(a.number())
        {
            store::set_check_issue(pool, id, a.number()).await?;
        }
        Ok(QaOutcome {
            status,
            check_id: Some(id),
            wrong,
            missed,
            cost_usd: cost,
            issue,
        })
    }

    async fn ledger(&self, pool: &PgPool, records: usize, record: CallRecord) {
        let record = CallRecord {
            model: self.config.model.clone(),
            prompt_version: QA_PROMPT_VERSION,
            events_requested: i32::try_from(records).unwrap_or(i32::MAX),
            ..record
        };
        if let Err(e) = record_pass_call(pool, LedgerPass::Qa, &record, Utc::now()).await {
            tracing::error!(error = %e, "recording a scraper check call failed");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn now() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 10, 1, 9, 0, 0).unwrap()
    }

    fn last(days_ago: i64, hash: &str, rules: &[&str]) -> store::LatestCheck {
        store::LatestCheck {
            checked_at: now() - chrono::Duration::days(days_ago),
            code_hash: hash.into(),
            rules_hit: rules.iter().map(|r| r.to_string()).collect(),
        }
    }

    #[test]
    fn due_reasons_in_priority_order() {
        let week = chrono::Duration::days(7);
        let rules = |r: &[&str]| r.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        let due = |l: Option<&store::LatestCheck>, hash: &str, r: &[&str]| {
            due_reason(l, hash, &rules(r), now(), week)
        };
        assert_eq!(due(None, "h", &[]), Some(DueReason::First));
        assert_eq!(
            due(Some(&last(8, "old", &[])), "h", &["long_span"]),
            Some(DueReason::CodeChanged)
        );
        assert_eq!(
            due(Some(&last(1, "old", &[])), "", &[]),
            None,
            "unknown hash"
        );
        assert_eq!(
            due(Some(&last(7, "h", &[])), "h", &["long_span"]),
            Some(DueReason::Weekly)
        );
        assert_eq!(
            due(Some(&last(1, "h", &["long_span"])), "h", &["same_date"]),
            Some(DueReason::RuleHit)
        );
        assert_eq!(
            due(
                Some(&last(1, "h", &["long_span", "same_date"])),
                "h",
                &["same_date"]
            ),
            None,
            "only a rule the last check did not see re-checks"
        );
    }

    fn ev(id: &str, kept: bool) -> RunEvent {
        RunEvent {
            kept,
            ..input::tests::run_event(id, id, now())
        }
    }

    #[test]
    fn flagged_records_come_first_then_first_and_middle() {
        let events: Vec<RunEvent> = ["a", "b", "c", "d", "e"]
            .iter()
            .map(|id| ev(id, *id != "b"))
            .collect();
        let pick = |f: &[Finding]| {
            detail_candidates(&events, f)
                .iter()
                .map(|e| e.source_event_id.clone())
                .collect::<Vec<_>>()
        };
        assert_eq!(pick(&[]), ["a", "d"]);
        let flagged = Finding {
            rule: rules::Rule::LongSpan,
            affected: 1,
            detail: String::new(),
            examples: vec![
                json!({"source_event_id": "e"}),
                json!({"source_event_id": "b"}),
            ],
        };
        assert_eq!(pick(&[flagged]), ["e", "a", "c"]);
    }

    #[test]
    fn url_keys_ignore_query_fragment_and_trailing_slash() {
        assert_eq!(
            url_key("https://v.test/e/1/?utm=x#top"),
            url_key("https://v.test/e/1")
        );
        assert_eq!(url_key("not a url"), None);
    }
}
