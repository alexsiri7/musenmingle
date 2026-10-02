//! The QA judge's answer: its shape, the prompt, and strict validation.
//!
//! The judge only flags differences; nothing it says is written to
//! `events.events`. Our values in a verdict come from our records, never
//! from the model, and the status is computed here.

use serde::Serialize;
use serde_json::Value;

use super::input::{JudgeInput, is_run_record};
use crate::enrich::output::normalise_for_match;
use crate::runner::RunEvent;

/// Bump when the prompt, the input or the validation changes.
pub const QA_PROMPT_VERSION: i32 = 3;
pub const SYSTEM_PROMPT: &str = include_str!("prompt.txt");

pub const FIELDS: &[&str] = &[
    "title",
    "starts_at",
    "ends_at",
    "all_day",
    "venue_name",
    "address",
    "price",
    "category",
];
pub const VERDICTS: &[&str] = &["correct", "wrong", "missing", "not_on_page"];
pub const MAX_QUOTE_CHARS: usize = 200;
pub const MAX_MISSED: usize = 10;

/// The judge's view of one field of one of our records.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct FieldVerdict {
    pub id: String,
    /// Our record's title.
    pub record: String,
    pub field: String,
    /// Our value (from our record, not the model).
    pub ours: Value,
    pub page_says: Option<String>,
    pub verdict: String,
    pub evidence_quote: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct MissedEvent {
    pub title: String,
    pub evidence_quote: String,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Verdict {
    pub findings: Vec<FieldVerdict>,
    pub missed: Vec<MissedEvent>,
}

impl Verdict {
    pub fn wrong(&self) -> impl Iterator<Item = &FieldVerdict> {
        self.findings.iter().filter(|f| f.verdict == "wrong")
    }

    pub fn wrong_count(&self) -> usize {
        self.wrong().count()
    }

    /// `issues` when a field is wrong or an event was missed, else `ok`.
    pub fn status(&self) -> &'static str {
        if self.wrong_count() > 0 || !self.missed.is_empty() {
            "issues"
        } else {
            "ok"
        }
    }
}

fn opt_str(v: &Value, key: &str, at: &str, problems: &mut Vec<String>) -> Option<String> {
    match v.get(key) {
        None | Some(Value::Null) => None,
        Some(Value::String(s)) => Some(s.trim().to_string()),
        Some(_) => {
            problems.push(format!("{at}: {key} must be a string or null"));
            None
        }
    }
}

fn check_quote(
    quote: &str,
    grounding: &str,
    at: &str,
    where_: &str,
    problems: &mut Vec<String>,
) -> bool {
    let n = quote.chars().count();
    if n == 0 || n > MAX_QUOTE_CHARS {
        problems.push(format!(
            "{at}: evidence_quote must be 1-{MAX_QUOTE_CHARS} characters, got {n}"
        ));
        return false;
    }
    if !grounding.contains(&normalise_for_match(quote)) {
        problems.push(format!(
            "{at}: evidence_quote {quote:?} is not copied verbatim from {where_}"
        ));
        return false;
    }
    true
}

/// Validate the judge's answer against what we sent. `Err` lists every
/// problem (sent back to the model for its one retry). Missed events that
/// match any record of the run (`all_records`) are dropped, not errors.
pub fn validate(
    content: &str,
    input: &JudgeInput,
    all_records: &[RunEvent],
) -> Result<Verdict, String> {
    let v: Value = serde_json::from_str(content.trim())
        .map_err(|e| format!("the answer is not a JSON object: {e}"))?;
    let obj = v.as_object().ok_or("the answer is not a JSON object")?;
    let mut problems = Vec::new();
    let empty = Vec::new();
    let array = |key: &str, problems: &mut Vec<String>| match obj.get(key) {
        Some(Value::Array(a)) => a,
        _ => {
            problems.push(format!("{key} must be an array"));
            &empty
        }
    };
    let raw_findings = array("findings", &mut problems);
    let raw_missed = array("missed_events", &mut problems);

    let mut findings = Vec::new();
    for (i, f) in raw_findings.iter().enumerate() {
        let at = format!("findings[{i}]");
        let Some(id) = f.get("id").and_then(Value::as_str) else {
            problems.push(format!("{at}: id must be a string"));
            continue;
        };
        let Some(ours) = input.records_by_id.get(id) else {
            problems.push(format!("{at}: unknown id {id:?}"));
            continue;
        };
        let field = f.get("field").and_then(Value::as_str).unwrap_or_default();
        if !FIELDS.contains(&field) {
            problems.push(format!("{at}: unknown field {field:?}"));
            continue;
        }
        let verdict = f.get("verdict").and_then(Value::as_str).unwrap_or_default();
        if !VERDICTS.contains(&verdict) {
            problems.push(format!(
                "{at}: verdict {verdict:?} is not one of {VERDICTS:?}"
            ));
            continue;
        }
        if id.starts_with('l') && verdict != "wrong" {
            problems.push(format!(
                "{at}: listing rows ({id}) may only be reported as wrong"
            ));
            continue;
        }
        let page_says = opt_str(f, "page_says", &at, &mut problems);
        if page_says
            .as_deref()
            .is_some_and(|s| s.chars().count() > MAX_QUOTE_CHARS)
        {
            problems.push(format!(
                "{at}: page_says is over {MAX_QUOTE_CHARS} characters"
            ));
        }
        let quote = opt_str(f, "evidence_quote", &at, &mut problems);
        match (verdict, &quote) {
            ("not_on_page", Some(_)) => {
                problems.push(format!("{at}: evidence_quote must be null for not_on_page"));
            }
            ("not_on_page", None) => {}
            (_, None) => problems.push(format!("{at}: evidence_quote is required for {verdict}")),
            (_, Some(q)) => {
                check_quote(q, &input.grounding_all, &at, "the pages", &mut problems);
            }
        }
        findings.push(FieldVerdict {
            id: id.to_string(),
            record: ours["title"].as_str().unwrap_or_default().to_string(),
            field: field.to_string(),
            ours: ours[field].clone(),
            page_says,
            verdict: verdict.to_string(),
            evidence_quote: quote,
        });
    }
    for id in &input.record_ids {
        if !findings.iter().any(|f| &f.id == id) {
            problems.push(format!("{id}: no finding for this record"));
        }
    }

    let mut missed = Vec::new();
    for (i, m) in raw_missed.iter().enumerate() {
        let at = format!("missed_events[{i}]");
        let title = opt_str(m, "title", &at, &mut problems).unwrap_or_default();
        let n = title.chars().count();
        if n == 0 || n > MAX_QUOTE_CHARS {
            problems.push(format!(
                "{at}: title must be 1-{MAX_QUOTE_CHARS} characters"
            ));
            continue;
        }
        let Some(quote) = opt_str(m, "evidence_quote", &at, &mut problems) else {
            problems.push(format!("{at}: evidence_quote is required"));
            continue;
        };
        if check_quote(
            &quote,
            &input.grounding_listing,
            &at,
            "the listing page",
            &mut problems,
        ) {
            missed.push(MissedEvent {
                title,
                evidence_quote: quote,
            });
        }
    }

    missed.retain(|m| !is_run_record(&m.title, all_records));
    if missed.len() > MAX_MISSED {
        problems.push(format!(
            "missed_events: at most {MAX_MISSED} that aren't our records, got {}",
            missed.len()
        ));
    }

    if !problems.is_empty() {
        return Err(problems.join("\n"));
    }
    Ok(Verdict { findings, missed })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::qa::input::tests::run_event;
    use crate::qa::input::{PageIn, PageKind, build};
    use chrono::{NaiveDate, TimeZone, Utc};
    use serde_json::json;

    const LISTING: &str = "<html><body><h1>What's on</h1><ul>\
        <li>Night Talk — 12 November, 8pm</li><li>Print Fair — 20 November</li>\
        <li>Kept Show — 1 December</li></ul></body></html>";
    const DETAIL: &str = "<html><head><script type=\"application/ld+json\">\
        {\"@type\":\"Event\",\"name\":\"Night Talk\",\"startDate\":\"2026-11-12T20:00\"}</script></head>\
        <body><h1>Night Talk</h1><p>Thursday 12 November, 8pm. Tickets £5.</p></body></html>";

    fn records() -> Vec<RunEvent> {
        let midnight = Utc.with_ymd_and_hms(2026, 11, 12, 0, 0, 0).unwrap();
        vec![
            run_event("talk", "Night Talk", midnight),
            run_event(
                "show",
                "Kept Show",
                Utc.with_ymd_and_hms(2026, 12, 1, 10, 0, 0).unwrap(),
            ),
        ]
    }

    fn input(all: &[RunEvent]) -> JudgeInput {
        build(
            "fake",
            NaiveDate::from_ymd_opt(2026, 10, 1).unwrap(),
            &[
                PageIn {
                    kind: PageKind::Listing,
                    url: "https://venue.test/whats-on",
                    body: LISTING,
                    json: false,
                },
                PageIn {
                    kind: PageKind::Detail,
                    url: "https://venue.test/talk",
                    body: DETAIL,
                    json: false,
                },
            ],
            &[(&all[0], 1)],
            all,
        )
    }

    fn answer(findings: Value, missed: Value) -> String {
        json!({ "findings": findings, "missed_events": missed }).to_string()
    }

    fn wrong_start() -> Value {
        json!({"id": "r1", "field": "starts_at", "page_says": "12 November, 8pm",
               "verdict": "wrong", "evidence_quote": "Thursday 12 November, 8pm"})
    }

    #[test]
    fn a_valid_answer_is_filled_from_our_record() {
        let all = records();
        let input = input(&all);
        assert_eq!(input.listing_ids, ["l1"], "Kept Show is on the listing");
        let v = validate(
            &answer(
                json!([wrong_start(),
                       {"id": "r1", "field": "venue_name", "page_says": null,
                        "verdict": "not_on_page", "evidence_quote": null},
                       {"id": "r1", "field": "starts_at", "page_says": "2026-11-12T20:00",
                        "verdict": "wrong", "evidence_quote": "2026-11-12T20:00"}]),
                json!([{"title": "Print Fair", "evidence_quote": "Print Fair — 20 November"}]),
            ),
            &input,
            &all,
        )
        .unwrap();
        assert_eq!(v.findings[0].ours, json!("2026-11-12 00:00"));
        assert_eq!(v.findings[0].record, "Night Talk");
        assert_eq!(v.findings[1].ours, json!("Hall"));
        assert_eq!(v.wrong_count(), 2);
        assert_eq!(v.missed.len(), 1);
        assert_eq!(v.status(), "issues");
    }

    #[test]
    fn status_is_ok_without_wrong_fields_or_missed_events() {
        let all = records();
        let input = input(&all);
        let v = validate(
            &answer(
                json!([{"id": "r1", "field": "title", "page_says": "Night Talk",
                        "verdict": "correct", "evidence_quote": "Night Talk"},
                       {"id": "r1", "field": "address", "page_says": null,
                        "verdict": "not_on_page", "evidence_quote": null},
                       {"id": "r1", "field": "price", "page_says": "£5",
                        "verdict": "missing", "evidence_quote": "Tickets £5."}]),
                // One of ours (listed by the run): dropped, not an error.
                json!([{"title": "Kept show", "evidence_quote": "Kept Show — 1 December"}]),
            ),
            &input,
            &all,
        )
        .unwrap();
        assert!(v.missed.is_empty());
        assert_eq!(v.status(), "ok");
    }

    #[test]
    fn the_missed_events_cap_counts_only_events_that_are_not_ours() {
        let all = records();
        let input = input(&all);
        let ours = json!({"title": "Kept show", "evidence_quote": "Kept Show — 1 December"});
        let other = json!({"title": "Print Fair", "evidence_quote": "Print Fair"});
        let mut missed = vec![ours.clone(), ours];
        missed.extend(std::iter::repeat_n(other, MAX_MISSED - 1));
        let v = validate(&answer(json!([wrong_start()]), json!(missed)), &input, &all).unwrap();
        assert_eq!(v.missed.len(), MAX_MISSED - 1);
    }

    #[test]
    fn every_violation_is_rejected() {
        let all = records();
        let input = input(&all);
        let missed = |m: Value| answer(json!([wrong_start()]), m);
        let cases: Vec<(String, &str)> = vec![
            ("not json".into(), "not a JSON object"),
            ("[]".into(), "not a JSON object"),
            (
                json!({"findings": []}).to_string(),
                "missed_events must be an array",
            ),
            (
                answer(
                    json!([wrong_start(), {"id": "r9", "field": "title", "verdict": "wrong",
                                           "evidence_quote": "Night Talk"}]),
                    json!([]),
                ),
                "unknown id \"r9\"",
            ),
            (
                answer(
                    json!([{"id": "r1", "field": "colour", "verdict": "wrong",
                            "evidence_quote": "Night Talk"}]),
                    json!([]),
                ),
                "unknown field \"colour\"",
            ),
            (
                answer(
                    json!([{"id": "r1", "field": "title", "verdict": "maybe",
                            "evidence_quote": "Night Talk"}]),
                    json!([]),
                ),
                "verdict \"maybe\"",
            ),
            (
                answer(
                    json!([wrong_start(), {"id": "l1", "field": "title", "verdict": "correct",
                                           "evidence_quote": "Kept Show"}]),
                    json!([]),
                ),
                "may only be reported as wrong",
            ),
            (
                answer(json!([]), json!([])),
                "r1: no finding for this record",
            ),
            (
                answer(
                    json!([{"id": "r1", "field": "starts_at", "verdict": "wrong",
                            "evidence_quote": "Friday 13 November"}]),
                    json!([]),
                ),
                "not copied verbatim from the pages",
            ),
            (
                answer(
                    json!([{"id": "r1", "field": "address", "verdict": "not_on_page",
                            "evidence_quote": "Night Talk"}]),
                    json!([]),
                ),
                "must be null for not_on_page",
            ),
            (
                answer(
                    json!([{"id": "r1", "field": "price", "verdict": "correct",
                            "evidence_quote": null}]),
                    json!([]),
                ),
                "evidence_quote is required for correct",
            ),
            (
                missed(json!(
                    (0..11)
                        .map(|_| json!({"title": "Print Fair", "evidence_quote": "Print Fair"}))
                        .collect::<Vec<_>>()
                )),
                "at most 10",
            ),
            (
                missed(json!([{"title": "Night Talk tickets", "evidence_quote": "Tickets £5."}])),
                "not copied verbatim from the listing page",
            ),
        ];
        for (content, want) in cases {
            let err = validate(&content, &input, &all).unwrap_err();
            assert!(err.contains(want), "{content}\n→ {err}");
        }
    }
}
