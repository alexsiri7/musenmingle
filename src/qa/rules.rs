//! Deterministic sanity rules over one run's normalised events (no AI).
//!
//! They run after every successful run of a source; hits are stored in
//! `events.qa_findings`, shown on `/sources` and make the source due for an
//! AI check ([`super::QaChecker::plan`]).

use std::collections::HashMap;

use chrono::{DateTime, Duration, Utc};
use chrono_tz::Europe::London;
use serde_json::{Value, json};

use crate::health::{self, HealthConfig, RunStats, Trip};
use crate::normalise::{is_london_midnight, london_date, normalise_title_for_key};

/// What the rules need to know about one normalised event.
#[derive(Debug, Clone, PartialEq)]
pub struct RuleEvent {
    pub source_event_id: String,
    pub title: String,
    pub starts_at: DateTime<Utc>,
    pub ends_at: Option<DateTime<Utc>>,
    pub all_day: bool,
    pub venue_missing: bool,
    pub coords_missing: bool,
    /// False for events the runner did not store (past, or rejected by the
    /// database, e.g. ending before they start).
    pub kept: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Rule {
    EndBeforeStart,
    YearOutOfRange,
    LongSpan,
    MidnightNotAllDay,
    DuplicateTitle,
    MissingVenueJump,
    MissingCoordsJump,
    SameDate,
    CountDrop,
}

impl Rule {
    /// The name stored in `events.qa_findings.rule`.
    pub fn as_str(self) -> &'static str {
        match self {
            Rule::EndBeforeStart => "end_before_start",
            Rule::YearOutOfRange => "year_out_of_range",
            Rule::LongSpan => "long_span",
            Rule::MidnightNotAllDay => "midnight_not_all_day",
            Rule::DuplicateTitle => "duplicate_title",
            Rule::MissingVenueJump => "missing_venue_jump",
            Rule::MissingCoordsJump => "missing_coords_jump",
            Rule::SameDate => "same_date",
            Rule::CountDrop => "count_drop",
        }
    }
}

/// One rule hit.
#[derive(Debug, Clone, PartialEq)]
pub struct Finding {
    pub rule: Rule,
    /// Events concerned (for `count_drop`: the events found).
    pub affected: i32,
    pub detail: String,
    /// Up to [`MAX_EXAMPLES`] of the events concerned.
    pub examples: Vec<Value>,
}

impl Finding {
    /// The `source_event_id`s of the examples.
    pub fn example_ids(&self) -> impl Iterator<Item = &str> {
        self.examples
            .iter()
            .filter_map(|e| e["source_event_id"].as_str())
    }
}

/// The per-run counts the jump rules compare (`events.source_runs`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RunCounts {
    /// Kept events the rules looked at.
    pub events_checked: i32,
    pub missing_venue: i32,
    pub missing_coords: i32,
}

impl RunCounts {
    pub fn of(events: &[RuleEvent]) -> Self {
        let kept = events.iter().filter(|e| e.kept);
        let count = |f: fn(&RuleEvent) -> bool| {
            i32::try_from(kept.clone().filter(|e| f(e)).count()).unwrap_or(i32::MAX)
        };
        Self {
            events_checked: count(|_| true),
            missing_venue: count(|e| e.venue_missing),
            missing_coords: count(|e| e.coords_missing),
        }
    }

    fn pct(self, missing: i32) -> f64 {
        f64::from(missing) * 100.0 / f64::from(self.events_checked)
    }
}

pub const MAX_EXAMPLES: usize = 5;
/// The jump rules need this many kept events in the run...
const JUMP_MIN_EVENTS: i32 = 5;
/// ... and this many earlier runs with counts ...
const JUMP_MIN_HISTORY: usize = 3;
/// ... of which at most this many (newest) are averaged.
const JUMP_HISTORY: usize = 5;
/// Percentage points above the average that count as a jump.
const JUMP_POINTS: f64 = 30.0;
const SAME_DATE_MIN_EVENTS: usize = 5;

fn london(t: DateTime<Utc>) -> String {
    t.with_timezone(&London)
        .format("%Y-%m-%d %H:%M")
        .to_string()
}

fn example(e: &RuleEvent) -> Value {
    json!({
        "source_event_id": e.source_event_id,
        "title": e.title,
        "starts_at": london(e.starts_at),
        "ends_at": e.ends_at.map(london),
    })
}

fn count(n: usize) -> i32 {
    i32::try_from(n).unwrap_or(i32::MAX)
}

fn finding(rule: Rule, hits: &[&RuleEvent], detail: String) -> Option<Finding> {
    (!hits.is_empty()).then(|| Finding {
        rule,
        affected: count(hits.len()),
        detail,
        examples: hits.iter().take(MAX_EXAMPLES).map(|e| example(e)).collect(),
    })
}

fn jump(
    rule: Rule,
    what: &str,
    now: RunCounts,
    history: &[RunCounts],
    missing: fn(RunCounts) -> i32,
    hits: &[&RuleEvent],
) -> Option<Finding> {
    let prior: Vec<RunCounts> = history
        .iter()
        .copied()
        .filter(|h| h.events_checked > 0)
        .take(JUMP_HISTORY)
        .collect();
    if now.events_checked < JUMP_MIN_EVENTS || prior.len() < JUMP_MIN_HISTORY {
        return None;
    }
    let before = prior.iter().map(|h| h.pct(missing(*h))).sum::<f64>() / prior.len() as f64;
    let current = now.pct(missing(now));
    (current - before >= JUMP_POINTS).then(|| Finding {
        rule,
        affected: missing(now),
        detail: format!(
            "{current:.0}% of events have no {what}, up from an average of {before:.0}%"
        ),
        examples: hits.iter().take(MAX_EXAMPLES).map(|e| example(e)).collect(),
    })
}

/// Apply every rule to a run's events. `history` is the counts of earlier
/// runs, newest first (this run excluded); `health_runs` is newest first
/// with this run first, as for [`health::evaluate`].
pub fn evaluate(
    events: &[RuleEvent],
    history: &[RunCounts],
    health_runs: &[RunStats],
    now: DateTime<Utc>,
) -> Vec<Finding> {
    let kept: Vec<&RuleEvent> = events.iter().filter(|e| e.kept).collect();
    let mut out = Vec::new();

    let backwards: Vec<&RuleEvent> = events
        .iter()
        .filter(|e| e.ends_at.is_some_and(|end| end < e.starts_at))
        .collect();
    out.extend(finding(
        Rule::EndBeforeStart,
        &backwards,
        format!("{} event(s) end before they start", backwards.len()),
    ));

    let year = Duration::days(365);
    let out_of_range: Vec<&RuleEvent> = kept
        .iter()
        .copied()
        .filter(|e| e.starts_at < now - year || e.ends_at.unwrap_or(e.starts_at) > now + year * 3)
        .collect();
    out.extend(finding(
        Rule::YearOutOfRange,
        &out_of_range,
        format!(
            "{} event(s) start over a year ago or end over three years ahead",
            out_of_range.len()
        ),
    ));

    let long: Vec<&RuleEvent> = kept
        .iter()
        .copied()
        .filter(|e| e.ends_at.is_some_and(|end| end - e.starts_at > year))
        .collect();
    out.extend(finding(
        Rule::LongSpan,
        &long,
        format!("{} event(s) run for more than a year", long.len()),
    ));

    let midnight: Vec<&RuleEvent> = kept
        .iter()
        .copied()
        .filter(|e| !e.all_day && is_london_midnight(e.starts_at))
        .collect();
    out.extend(finding(
        Rule::MidnightNotAllDay,
        &midnight,
        format!(
            "{} event(s) start at exactly midnight but are not all-day (a missing time?)",
            midnight.len()
        ),
    ));

    let mut groups: HashMap<(String, chrono::NaiveDate), Vec<&RuleEvent>> = HashMap::new();
    for e in &kept {
        groups
            .entry((normalise_title_for_key(&e.title), london_date(e.starts_at)))
            .or_default()
            .push(e);
    }
    let dupes: Vec<&RuleEvent> = kept
        .iter()
        .copied()
        .filter(|e| {
            groups[&(normalise_title_for_key(&e.title), london_date(e.starts_at))].len() > 1
        })
        .collect();
    out.extend(finding(
        Rule::DuplicateTitle,
        &dupes,
        format!(
            "{} event(s) share a title and start date with another event",
            dupes.len()
        ),
    ));

    let counts = RunCounts::of(events);
    let no_venue: Vec<&RuleEvent> = kept.iter().copied().filter(|e| e.venue_missing).collect();
    out.extend(jump(
        Rule::MissingVenueJump,
        "venue",
        counts,
        history,
        |c| c.missing_venue,
        &no_venue,
    ));
    let no_coords: Vec<&RuleEvent> = kept.iter().copied().filter(|e| e.coords_missing).collect();
    out.extend(jump(
        Rule::MissingCoordsJump,
        "coordinates",
        counts,
        history,
        |c| c.missing_coords,
        &no_coords,
    ));

    if kept.len() >= SAME_DATE_MIN_EVENTS
        && kept
            .iter()
            .all(|e| london_date(e.starts_at) == london_date(kept[0].starts_at))
    {
        out.extend(finding(
            Rule::SameDate,
            &kept,
            format!(
                "all {} events start on {}",
                kept.len(),
                london_date(kept[0].starts_at)
            ),
        ));
    }

    for trip in health::evaluate(health_runs, &HealthConfig::default()) {
        if let Trip::CountDrop { current, .. } = trip {
            out.push(Finding {
                rule: Rule::CountDrop,
                affected: current,
                detail: trip.to_string(),
                examples: Vec::new(),
            });
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn now() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 10, 1, 9, 0, 0).unwrap()
    }

    /// A kept, timed event starting `days` from now at 19:00 London.
    fn ev(id: &str, days: i64) -> RuleEvent {
        let date = london_date(now()) + Duration::days(days);
        RuleEvent {
            source_event_id: id.into(),
            title: format!("Event {id}"),
            starts_at: crate::normalise::london_to_utc(date.and_hms_opt(19, 0, 0).unwrap()),
            ends_at: None,
            all_day: false,
            venue_missing: false,
            coords_missing: false,
            kept: true,
        }
    }

    fn spread(n: usize) -> Vec<RuleEvent> {
        (0..n).map(|i| ev(&format!("e{i}"), i as i64)).collect()
    }

    fn history(n: usize, checked: i32, missing: i32) -> Vec<RunCounts> {
        vec![
            RunCounts {
                events_checked: checked,
                missing_venue: missing,
                missing_coords: missing,
            };
            n
        ]
    }

    fn fired(events: &[RuleEvent], hist: &[RunCounts], runs: &[RunStats]) -> Vec<(Rule, i32)> {
        evaluate(events, hist, runs, now())
            .into_iter()
            .map(|f| (f.rule, f.affected))
            .collect()
    }

    fn run(n: i32) -> RunStats {
        RunStats {
            events_found: n,
            errors: 0,
            ok: true,
        }
    }

    #[test]
    fn rules_table() {
        let with = |f: fn(&mut RuleEvent)| {
            let mut e = ev("x", 3);
            f(&mut e);
            e
        };
        let midnight = crate::normalise::london_to_utc(
            (london_date(now()) + Duration::days(3))
                .and_hms_opt(0, 0, 0)
                .unwrap(),
        );
        let missing_venue = |n: usize| {
            let mut v = spread(n);
            for e in &mut v {
                e.venue_missing = true;
            }
            v
        };
        let mut same_day: Vec<RuleEvent> = (0..5).map(|i| ev(&format!("s{i}"), 2)).collect();
        for (i, e) in same_day.iter_mut().enumerate() {
            e.title = format!("Different {i}");
        }

        type Case = (
            &'static str,
            Vec<RuleEvent>,
            Vec<RunCounts>,
            Vec<(Rule, i32)>,
        );
        let cases: Vec<Case> = vec![
            ("clean run", spread(4), vec![], vec![]),
            (
                "end before start counts events that were not kept",
                vec![
                    with(|e| e.ends_at = Some(e.starts_at - Duration::hours(1))),
                    RuleEvent {
                        kept: false,
                        ..with(|e| e.ends_at = Some(e.starts_at - Duration::days(1)))
                    },
                ],
                vec![],
                vec![(Rule::EndBeforeStart, 2)],
            ),
            (
                "started over a year ago",
                vec![with(|e| {
                    e.starts_at = now() - Duration::days(366);
                    e.ends_at = Some(now() + Duration::days(10));
                })],
                vec![],
                vec![(Rule::YearOutOfRange, 1), (Rule::LongSpan, 1)],
            ),
            (
                "ends over three years ahead",
                vec![with(|e| e.starts_at = now() + Duration::days(3 * 365 + 1))],
                vec![],
                vec![(Rule::YearOutOfRange, 1)],
            ),
            (
                "exactly a year long is not a long span",
                vec![with(|e| {
                    e.ends_at = Some(e.starts_at + Duration::days(365))
                })],
                vec![],
                vec![],
            ),
            (
                "a year and a day is",
                vec![with(|e| {
                    e.ends_at = Some(e.starts_at + Duration::days(366))
                })],
                vec![],
                vec![(Rule::LongSpan, 1)],
            ),
            (
                "a start that is not London midnight",
                vec![with(|e| e.starts_at = now() + Duration::days(3))],
                vec![],
                vec![],
            ),
            (
                "London midnight, timed",
                vec![RuleEvent {
                    starts_at: midnight,
                    ..ev("m", 3)
                }],
                vec![],
                vec![(Rule::MidnightNotAllDay, 1)],
            ),
            (
                "London midnight, all day",
                vec![RuleEvent {
                    starts_at: midnight,
                    all_day: true,
                    ..ev("m", 3)
                }],
                vec![],
                vec![],
            ),
            (
                "same title, same day",
                vec![
                    RuleEvent {
                        title: "The Show".into(),
                        ..ev("a", 3)
                    },
                    RuleEvent {
                        title: "the show!".into(),
                        ..ev("b", 3)
                    },
                    ev("c", 3),
                ],
                vec![],
                vec![(Rule::DuplicateTitle, 2)],
            ),
            (
                "same title, different days (a series)",
                vec![
                    RuleEvent {
                        title: "Late".into(),
                        ..ev("a", 3)
                    },
                    RuleEvent {
                        title: "Late".into(),
                        ..ev("b", 10)
                    },
                ],
                vec![],
                vec![],
            ),
            (
                "venue jump",
                missing_venue(5),
                history(3, 10, 0),
                vec![(Rule::MissingVenueJump, 5)],
            ),
            (
                "jump needs three earlier runs",
                missing_venue(5),
                history(2, 10, 0),
                vec![],
            ),
            (
                "jump needs five events",
                missing_venue(4),
                history(3, 10, 0),
                vec![],
            ),
            (
                "no jump when it was always missing",
                missing_venue(5),
                history(3, 10, 8),
                vec![],
            ),
            (
                "runs without counts are ignored",
                missing_venue(5),
                [history(2, 10, 0), history(3, 0, 0)].concat(),
                vec![],
            ),
            (
                "coords jump",
                {
                    let mut v = spread(6);
                    for e in &mut v[..3] {
                        e.coords_missing = true;
                    }
                    v
                },
                history(5, 10, 0),
                vec![(Rule::MissingCoordsJump, 3)],
            ),
            (
                "five events on one day",
                same_day.clone(),
                vec![],
                vec![(Rule::SameDate, 5)],
            ),
            (
                "four events on one day",
                same_day[..4].to_vec(),
                vec![],
                vec![],
            ),
            (
                "past events are ignored by the other rules",
                vec![RuleEvent {
                    starts_at: midnight,
                    kept: false,
                    ..ev("m", 3)
                }],
                vec![],
                vec![],
            ),
        ];
        for (name, events, hist, want) in cases {
            assert_eq!(fired(&events, &hist, &[]), want, "{name}");
        }
    }

    #[test]
    fn count_drop_reuses_the_health_rule() {
        let events = spread(2);
        let drop = [run(2), run(10), run(10), run(10)];
        let got = evaluate(&events, &[], &drop, now());
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].rule, Rule::CountDrop);
        assert_eq!(got[0].affected, 2);
        assert!(got[0].detail.contains("80% below"), "{}", got[0].detail);
        // Too little history: nothing.
        assert!(fired(&events, &[], &drop[..3]).is_empty());
    }

    #[test]
    fn examples_are_capped_and_in_london_time() {
        let events: Vec<RuleEvent> = (0..7)
            .map(|i| {
                let mut e = ev(&format!("e{i}"), 3);
                e.ends_at = Some(e.starts_at - Duration::hours(1));
                e
            })
            .collect();
        let got = evaluate(&events, &[], &[], now());
        let f = got.iter().find(|f| f.rule == Rule::EndBeforeStart).unwrap();
        assert_eq!(f.affected, 7);
        assert_eq!(f.examples.len(), MAX_EXAMPLES);
        assert_eq!(f.examples[0]["starts_at"], "2026-10-04 19:00");
        assert_eq!(f.examples[0]["ends_at"], "2026-10-04 18:00");
        assert_eq!(f.example_ids().next(), Some("e0"));
    }
}
