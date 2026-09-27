//! Calendar views (`GET /calendar`, `/saved/calendar`): London-date ranges,
//! navigation and where each event goes (pure; the pages are in `web.rs`).
//!
//! Every date here is a Europe/London calendar date. An event runs from the
//! London date of `starts_at` to the London date of `ends_at` (or its start
//! day when it has no end). Events that run on more than
//! [`LONG_RUN_MIN_DAYS`] - 1 days (typically exhibitions) are *long-running*:
//! they are listed once in the "ongoing" strip above the grid, plus an
//! "Opens" / "Last day" marker on the day they start or end when that day is
//! visible, instead of being repeated in every cell. Every other event sits
//! on its start day (or the first visible day if it started earlier).
//! `src/web.js` (`placeEvents`) implements the same rules for the saved-events
//! calendar; keep them in step.

use std::collections::BTreeMap;

use chrono::{DateTime, Datelike, Duration, NaiveDate, NaiveTime, Utc, Weekday};
use chrono_tz::Europe::London;

/// An event running on at least this many London days is long-running.
pub const LONG_RUN_MIN_DAYS: i64 = 4;

/// Which calendar layout to show.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum View {
    /// A month grid (an agenda list at phone width).
    Month,
    /// One Monday–Sunday week.
    Week,
    /// The month as a list of days with events, at every width.
    Agenda,
}

impl View {
    pub const ALL: [View; 3] = [View::Month, View::Week, View::Agenda];

    pub fn as_str(self) -> &'static str {
        match self {
            View::Month => "month",
            View::Week => "week",
            View::Agenda => "agenda",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            View::Month => "Month",
            View::Week => "Week",
            View::Agenda => "Agenda",
        }
    }

    /// Unknown values fall back to the month view (it is a page).
    pub fn parse(s: &str) -> View {
        View::ALL
            .into_iter()
            .find(|v| v.as_str() == s)
            .unwrap_or(View::Month)
    }

    /// "month" / "week" (the agenda shows a month).
    pub fn unit(self) -> &'static str {
        match self {
            View::Week => "week",
            View::Month | View::Agenda => "month",
        }
    }
}

/// The dates a view shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Range {
    /// First and last date whose events are shown (inclusive).
    pub first: NaiveDate,
    pub last: NaiveDate,
    /// The Monday–Sunday grid around them (padding days are empty).
    pub grid_first: NaiveDate,
    pub grid_last: NaiveDate,
}

impl Range {
    pub fn for_view(view: View, date: NaiveDate) -> Range {
        let (first, last) = match view {
            View::Week => {
                let monday = monday_of(date);
                (monday, monday + Duration::days(6))
            }
            View::Month | View::Agenda => (first_of_month(date), last_of_month(date)),
        };
        Range {
            first,
            last,
            grid_first: monday_of(first),
            grid_last: monday_of(last) + Duration::days(6),
        }
    }

    pub fn contains(&self, d: NaiveDate) -> bool {
        self.first <= d && d <= self.last
    }

    /// Every date of the grid, Monday first.
    pub fn grid_days(&self) -> Vec<NaiveDate> {
        let last = self.grid_last;
        self.grid_first
            .iter_days()
            .take_while(|d| *d <= last)
            .collect()
    }

    /// "W40 — W44": the ISO weeks the range covers.
    pub fn week_tag(&self) -> String {
        let (a, b) = (self.first.iso_week().week(), self.last.iso_week().week());
        if a == b {
            format!("W{a:02}")
        } else {
            format!("W{a:02} — W{b:02}")
        }
    }

    /// "October 2026" (month views) or "5–11 October 2026" / "28 September –
    /// 4 October 2026" / "28 December 2026 – 3 January 2027" (week view).
    pub fn title(&self, view: View) -> String {
        if view != View::Week {
            return self.first.format("%B %Y").to_string();
        }
        let (a, b) = (self.first, self.last);
        if a.year() != b.year() {
            format!("{} – {}", a.format("%-d %B %Y"), b.format("%-d %B %Y"))
        } else if a.month() != b.month() {
            format!("{} – {}", a.format("%-d %B"), b.format("%-d %B %Y"))
        } else {
            format!("{}–{}", a.format("%-d"), b.format("%-d %B %Y"))
        }
    }
}

pub fn monday_of(d: NaiveDate) -> NaiveDate {
    d - Duration::days(i64::from(d.weekday().num_days_from_monday()))
}

pub fn first_of_month(d: NaiveDate) -> NaiveDate {
    d.with_day(1).expect("day 1 exists")
}

pub fn last_of_month(d: NaiveDate) -> NaiveDate {
    next_month(d) - Duration::days(1)
}

fn next_month(d: NaiveDate) -> NaiveDate {
    let (y, m) = if d.month() == 12 {
        (d.year() + 1, 1)
    } else {
        (d.year(), d.month() + 1)
    };
    NaiveDate::from_ymd_opt(y, m, 1).expect("valid month")
}

fn prev_month(d: NaiveDate) -> NaiveDate {
    let (y, m) = if d.month() == 1 {
        (d.year() - 1, 12)
    } else {
        (d.year(), d.month() - 1)
    };
    NaiveDate::from_ymd_opt(y, m, 1).expect("valid month")
}

/// The date the previous/next view is anchored on: the first of the
/// adjacent month, or the Monday of the adjacent week.
pub fn step(view: View, date: NaiveDate, forward: bool) -> NaiveDate {
    match (view, forward) {
        (View::Week, true) => monday_of(date) + Duration::days(7),
        (View::Week, false) => monday_of(date) - Duration::days(7),
        (_, true) => next_month(date),
        (_, false) => prev_month(date),
    }
}

/// The London date of an instant.
pub fn london_date(t: DateTime<Utc>) -> NaiveDate {
    t.with_timezone(&London).date_naive()
}

/// Whether an event has no time of day: flagged all-day, or it starts at
/// London midnight (date-only listings stored before `all_day` existed).
pub fn is_untimed(starts_at: DateTime<Utc>, all_day: bool) -> bool {
    all_day || starts_at.with_timezone(&London).time() == NaiveTime::MIN
}

/// The London dates an event runs on (first, last); an end before the start
/// is ignored.
pub fn span(starts_at: DateTime<Utc>, ends_at: Option<DateTime<Utc>>) -> (NaiveDate, NaiveDate) {
    let start = london_date(starts_at);
    let end = ends_at.map(london_date).filter(|e| *e > start);
    (start, end.unwrap_or(start))
}

/// Runs on [`LONG_RUN_MIN_DAYS`] or more London days.
pub fn is_long_running(first: NaiveDate, last: NaiveDate) -> bool {
    (last - first).num_days() + 1 >= LONG_RUN_MIN_DAYS
}

/// What an entry in a day cell stands for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mark {
    /// The event itself (it starts that day, or started before the range).
    Event,
    /// A long-running event's first day.
    Opens,
    /// A long-running event's last day.
    LastDay,
}

/// Where the events of a range go: indexes into the input slice.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Placement {
    /// Long-running events that overlap the range, in input order.
    pub ongoing: Vec<usize>,
    /// Entries per visible day, in input order (sort the input by start).
    pub days: BTreeMap<NaiveDate, Vec<(usize, Mark)>>,
}

/// Place events given as their London (first, last) dates.
pub fn place(spans: &[(NaiveDate, NaiveDate)], range: &Range) -> Placement {
    let mut out = Placement::default();
    for (i, &(first, last)) in spans.iter().enumerate() {
        if last < range.first || first > range.last {
            continue;
        }
        if is_long_running(first, last) {
            out.ongoing.push(i);
            if range.contains(first) {
                out.days.entry(first).or_default().push((i, Mark::Opens));
            }
            if range.contains(last) {
                out.days.entry(last).or_default().push((i, Mark::LastDay));
            }
        } else {
            let day = first.max(range.first);
            out.days.entry(day).or_default().push((i, Mark::Event));
        }
    }
    out
}

/// Parse `date=YYYY-MM-DD`; anything else is `None` (callers use today).
pub fn parse_date(s: &str) -> Option<NaiveDate> {
    NaiveDate::parse_from_str(s.trim(), "%Y-%m-%d")
        .ok()
        .filter(|d| (1970..=9000).contains(&d.year()))
}

/// Short weekday name ("Mon").
pub fn weekday_short(d: NaiveDate) -> &'static str {
    match d.weekday() {
        Weekday::Mon => "Mon",
        Weekday::Tue => "Tue",
        Weekday::Wed => "Wed",
        Weekday::Thu => "Thu",
        Weekday::Fri => "Fri",
        Weekday::Sat => "Sat",
        Weekday::Sun => "Sun",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn d(y: i32, m: u32, day: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, day).unwrap()
    }

    #[test]
    fn month_range_pads_to_whole_weeks() {
        // October 2026 starts on a Thursday and ends on a Saturday.
        let r = Range::for_view(View::Month, d(2026, 10, 17));
        assert_eq!((r.first, r.last), (d(2026, 10, 1), d(2026, 10, 31)));
        assert_eq!(
            (r.grid_first, r.grid_last),
            (d(2026, 9, 28), d(2026, 11, 1))
        );
        assert_eq!(r.grid_days().len(), 35);
        assert_eq!(r.title(View::Month), "October 2026");
        assert_eq!(r.week_tag(), "W40 — W44");
        // February 2027 starts on a Monday: no leading padding.
        let r = Range::for_view(View::Month, d(2027, 2, 28));
        assert_eq!((r.first, r.last), (d(2027, 2, 1), d(2027, 2, 28)));
        assert_eq!((r.grid_first, r.grid_last), (d(2027, 2, 1), d(2027, 2, 28)));
        // Leap year.
        assert_eq!(last_of_month(d(2028, 2, 10)), d(2028, 2, 29));
    }

    #[test]
    fn week_range_is_monday_to_sunday() {
        for day in 26..=31 {
            let r = Range::for_view(View::Week, d(2026, 10, day));
            assert_eq!(
                (r.first, r.last),
                (d(2026, 10, 26), d(2026, 11, 1)),
                "{day}"
            );
            assert_eq!(r.grid_days().len(), 7);
        }
        let r = Range::for_view(View::Week, d(2026, 11, 1));
        assert_eq!(r.first, d(2026, 10, 26));
        assert_eq!(r.title(View::Week), "26 October – 1 November 2026");
        assert_eq!(
            Range::for_view(View::Week, d(2026, 10, 7)).title(View::Week),
            "5–11 October 2026"
        );
        assert_eq!(
            Range::for_view(View::Week, d(2026, 12, 30)).title(View::Week),
            "28 December 2026 – 3 January 2027"
        );
        assert_eq!(
            Range::for_view(View::Week, d(2026, 10, 7)).week_tag(),
            "W41"
        );
    }

    #[test]
    fn prev_and_next_cross_month_and_year_ends() {
        assert_eq!(step(View::Month, d(2026, 12, 31), true), d(2027, 1, 1));
        assert_eq!(step(View::Month, d(2027, 1, 31), false), d(2026, 12, 1));
        assert_eq!(step(View::Agenda, d(2026, 1, 31), true), d(2026, 2, 1));
        assert_eq!(step(View::Week, d(2026, 12, 31), true), d(2027, 1, 4));
        assert_eq!(step(View::Week, d(2027, 1, 1), false), d(2026, 12, 21));
    }

    #[test]
    fn london_dates_follow_daylight_saving() {
        // 23:30 UTC on Sat 24 Oct 2026 is 00:30 BST on Sun 25 Oct.
        let t = Utc.with_ymd_and_hms(2026, 10, 24, 23, 30, 0).unwrap();
        assert_eq!(london_date(t), d(2026, 10, 25));
        // After the clocks go back, 23:30 UTC on 25 Oct is still the 25th.
        let t = Utc.with_ymd_and_hms(2026, 10, 25, 23, 30, 0).unwrap();
        assert_eq!(london_date(t), d(2026, 10, 25));
        // Spring forward: 23:30 UTC on Sat 27 Mar 2027 is 23:30 GMT that day,
        // 23:30 UTC on Sun 28 Mar is 00:30 BST on Mon 29.
        let t = Utc.with_ymd_and_hms(2027, 3, 27, 23, 30, 0).unwrap();
        assert_eq!(london_date(t), d(2027, 3, 27));
        let t = Utc.with_ymd_and_hms(2027, 3, 28, 23, 30, 0).unwrap();
        assert_eq!(london_date(t), d(2027, 3, 29));
        // London midnight during BST (23:00 UTC the day before) is untimed.
        let midnight = Utc.with_ymd_and_hms(2026, 10, 2, 23, 0, 0).unwrap();
        assert!(is_untimed(midnight, false));
        assert!(!is_untimed(midnight + Duration::hours(18), false));
    }

    #[test]
    fn long_running_events_go_in_the_strip_with_markers() {
        let r = Range::for_view(View::Month, d(2026, 10, 1));
        let spans = [
            // 0: one-off talk.
            (d(2026, 10, 7), d(2026, 10, 7)),
            // 1: exhibition running all month (opened in September).
            (d(2026, 9, 1), d(2027, 1, 31)),
            // 2: exhibition opening on the 16th, closing on the 25th.
            (d(2026, 10, 16), d(2026, 10, 25)),
            // 3: three-day festival started before the range: on the 1st.
            (d(2026, 9, 30), d(2026, 10, 2)),
            // 4: outside the range.
            (d(2026, 11, 2), d(2026, 11, 2)),
            // 5: four days: long-running, closes on the 3rd.
            (d(2026, 9, 30), d(2026, 10, 3)),
        ];
        let p = place(&spans, &r);
        assert_eq!(p.ongoing, [1, 2, 5]);
        assert_eq!(p.days[&d(2026, 10, 7)], [(0, Mark::Event)]);
        assert_eq!(p.days[&d(2026, 10, 1)], [(3, Mark::Event)]);
        assert_eq!(p.days[&d(2026, 10, 16)], [(2, Mark::Opens)]);
        assert_eq!(p.days[&d(2026, 10, 25)], [(2, Mark::LastDay)]);
        assert_eq!(p.days[&d(2026, 10, 3)], [(5, Mark::LastDay)]);
        // Never repeated per day: 5 entries in all.
        assert_eq!(p.days.values().map(Vec::len).sum::<usize>(), 5);
        assert!(!p.days.keys().any(|k| !r.contains(*k)));
    }

    #[test]
    fn span_ignores_ends_before_the_start() {
        let s = Utc.with_ymd_and_hms(2026, 10, 7, 17, 30, 0).unwrap();
        assert_eq!(span(s, None), (d(2026, 10, 7), d(2026, 10, 7)));
        assert_eq!(
            span(s, Some(s - Duration::days(2))),
            (d(2026, 10, 7), d(2026, 10, 7))
        );
        assert_eq!(
            span(s, Some(s + Duration::days(3))),
            (d(2026, 10, 7), d(2026, 10, 10))
        );
    }

    #[test]
    fn views_and_dates_parse_leniently() {
        assert_eq!(View::parse("week"), View::Week);
        assert_eq!(View::parse("agenda"), View::Agenda);
        assert_eq!(View::parse("year"), View::Month);
        assert_eq!(parse_date("2026-10-07"), Some(d(2026, 10, 7)));
        assert_eq!(parse_date("2026-13-01"), None);
        assert_eq!(parse_date("99999-01-01"), None);
    }
}
