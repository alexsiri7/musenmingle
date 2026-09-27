//! Weekly opening hours (issue #168), pure.
//!
//! Exhibitions and other long-running events often say when you can actually
//! go ("on display Wednesdays, Thursdays, Fridays & Sundays from 11am–3pm").
//! [`from_text`] reads that from a listing's full description at ingest
//! (deterministic, no model: CLAUDE.md invariant 3), [`from_schema_org`] and
//! [`from_spec_json`] read schema.org `openingHours` /
//! `openingHoursSpecification`. Stored in `events.events.opening_hours` as
//! JSON (`[{"days":[3,4,5,7],"opens":"11:00","closes":"15:00"}]`, ISO
//! weekdays, Monday = 1) in Europe/London wall-clock time; the listing SQL
//! (`repo`) reads the same shape.
//!
//! Validation is strict and anything unsure is dropped: real weekdays only,
//! `opens < closes` (hours past midnight are dropped, not guessed), times
//! without am/pm or a 24-hour `HH:MM` are ambiguous ("10-5") and dropped,
//! and a text whose rules disagree about a day, or that says "except", gives
//! no hours at all. A wrong schedule would hide an event that is open, which
//! is worse than showing no hours.

use chrono::{DateTime, Datelike, NaiveTime, Timelike, Utc};
use chrono_tz::Europe::London;
use serde::{Deserialize, Serialize};

/// One line of a weekly schedule: open on `days` from `opens` to `closes`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HoursRule {
    /// ISO weekdays, Monday = 1 .. Sunday = 7, ascending, no duplicates.
    pub days: Vec<u8>,
    /// `HH:MM`, London time.
    #[serde(with = "hhmm")]
    pub opens: NaiveTime,
    /// `HH:MM`, London time, after `opens`.
    #[serde(with = "hhmm")]
    pub closes: NaiveTime,
}

/// A weekly schedule; days not named are closed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct OpeningHours(pub Vec<HoursRule>);

/// Hours read from a listing's text, plus the sentence(s) that said so.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedHours {
    pub hours: OpeningHours,
    /// The listing's own wording (at most [`MAX_NOTE`] characters), or None
    /// when it is longer.
    pub note: Option<String>,
}

/// Longest `hours_note` we keep.
pub const MAX_NOTE: usize = 200;

const DAY_SHORT: [&str; 7] = ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"];
const DAY_CODES: [&str; 7] = ["mon", "tue", "wed", "thu", "fri", "sat", "sun"];

/// The API's day code (`mon`..`sun`) for an ISO weekday.
pub fn day_code(d: u8) -> &'static str {
    DAY_CODES[usize::from(d.clamp(1, 7) - 1)]
}

/// Parse a day code or English day name (`sun`, `sunday`) as an ISO weekday.
pub fn parse_day(s: &str) -> Option<u8> {
    day_word(&s.trim().to_ascii_lowercase()).map(|(d, _)| d)
}

mod hhmm {
    use chrono::NaiveTime;
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(t: &NaiveTime, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&t.format("%H:%M").to_string())
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<NaiveTime, D::Error> {
        let s = String::deserialize(d)?;
        NaiveTime::parse_from_str(&s, "%H:%M").map_err(serde::de::Error::custom)
    }
}

impl OpeningHours {
    /// Validate and normalise: sort and dedupe days, merge rules with the
    /// same times, reject bad times and days two rules disagree about.
    pub fn new(rules: Vec<HoursRule>) -> Option<OpeningHours> {
        let mut out: Vec<HoursRule> = Vec::new();
        for mut r in rules {
            r.days.sort_unstable();
            r.days.dedup();
            if r.days.is_empty() || r.days.iter().any(|d| !(1..=7).contains(d)) {
                return None;
            }
            if r.opens >= r.closes {
                return None;
            }
            match out
                .iter_mut()
                .find(|o| o.opens == r.opens && o.closes == r.closes)
            {
                Some(o) => {
                    o.days.extend(r.days);
                    o.days.sort_unstable();
                    o.days.dedup();
                }
                None => out.push(r),
            }
        }
        if out.is_empty() {
            return None;
        }
        let mut seen = [false; 8];
        for r in &out {
            for &d in &r.days {
                if std::mem::replace(&mut seen[usize::from(d)], true) {
                    return None;
                }
            }
        }
        out.sort_by_key(|r| (r.days[0], r.opens));
        Some(OpeningHours(out))
    }

    /// The rule for an ISO weekday, if open that day.
    pub fn on_day(&self, day: u8) -> Option<&HoursRule> {
        self.0.iter().find(|r| r.days.contains(&day))
    }

    /// Open at this instant (London wall clock)?
    pub fn is_open_at(&self, at: DateTime<Utc>) -> bool {
        let l = at.with_timezone(&London);
        let t = l.time();
        self.on_day(weekday(&l))
            .is_some_and(|r| r.opens <= t && t < r.closes)
    }

    /// "Wed, Thu, Fri, Sun · 11:00–15:00" (rules joined with "; ").
    pub fn display(&self) -> String {
        self.0
            .iter()
            .map(|r| {
                let days = if r.days.len() == 7 {
                    "Daily".to_string()
                } else {
                    r.days
                        .iter()
                        .map(|&d| DAY_SHORT[usize::from(d - 1)])
                        .collect::<Vec<_>>()
                        .join(", ")
                };
                format!(
                    "{days} · {}–{}",
                    r.opens.format("%H:%M"),
                    r.closes.format("%H:%M")
                )
            })
            .collect::<Vec<_>>()
            .join("; ")
    }

    /// A card's status line for `now`: "Open now until 15:00", "Opens
    /// today at 11:00", "Closed now" (after today's hours) or "Closed
    /// today".
    pub fn today_status(&self, now: DateTime<Utc>) -> String {
        let l = now.with_timezone(&London);
        let t = l.time();
        match self.on_day(weekday(&l)) {
            None => "Closed today".into(),
            Some(r) if t < r.opens => format!("Opens today at {}", r.opens.format("%H:%M")),
            Some(r) if t < r.closes => format!("Open now until {}", r.closes.format("%H:%M")),
            Some(_) => "Closed now".into(),
        }
    }
}

fn weekday<T: Datelike>(d: &T) -> u8 {
    d.weekday().number_from_monday() as u8
}

/// A day word: (ISO weekday, plural?). Also 2-letter schema.org codes are
/// handled by [`from_schema_org`], not here.
fn day_word(w: &str) -> Option<(u8, bool)> {
    const NAMES: [(&str, &[&str]); 7] = [
        ("monday", &["mon"]),
        ("tuesday", &["tue", "tues"]),
        ("wednesday", &["wed", "weds"]),
        ("thursday", &["thu", "thur", "thurs"]),
        ("friday", &["fri"]),
        ("saturday", &["sat"]),
        ("sunday", &["sun"]),
    ];
    for (i, (full, short)) in NAMES.iter().enumerate() {
        let d = i as u8 + 1;
        if w == *full || short.contains(&w) {
            return Some((d, false));
        }
        if w.strip_suffix('s') == Some(full) {
            return Some((d, true));
        }
    }
    None
}

#[derive(Debug, Clone, PartialEq)]
enum Tok {
    Word(String),
    /// A clock time: hour, minute, meridiem ("am"/"pm"), written with a
    /// colon or dot (`10:30`).
    Time {
        h: u32,
        m: u32,
        mer: Option<bool>,
        colon: bool,
    },
    Dash,
    Comma,
    Amp,
    Slash,
    Colon,
}

fn tokenize(s: &str) -> Vec<Tok> {
    let chars: Vec<char> = s.chars().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c.is_ascii_alphabetic() {
            let st = i;
            while i < chars.len() && (chars[i].is_ascii_alphabetic() || chars[i] == '\'') {
                i += 1;
            }
            let w: String = chars[st..i].iter().collect::<String>().to_ascii_lowercase();
            // A meridiem after a bare time ("11 am").
            if let (
                Some(Tok::Time {
                    mer: mer @ None, ..
                }),
                Some(p),
            ) = (out.last_mut(), meridiem(&w))
            {
                *mer = Some(p);
                continue;
            }
            out.push(Tok::Word(w));
            continue;
        }
        if c.is_ascii_digit() {
            let st = i;
            while i < chars.len() && chars[i].is_ascii_digit() {
                i += 1;
            }
            let digits = i - st;
            let h: u32 = chars[st..i]
                .iter()
                .collect::<String>()
                .parse()
                .unwrap_or(99);
            let mut m = 0;
            let mut colon = false;
            if i + 2 < chars.len()
                && (chars[i] == ':' || chars[i] == '.')
                && chars[i + 1].is_ascii_digit()
                && chars[i + 2].is_ascii_digit()
                && chars.get(i + 3).is_none_or(|c| !c.is_ascii_digit())
            {
                m = chars[i + 1].to_digit(10).unwrap_or(0) * 10
                    + chars[i + 2].to_digit(10).unwrap_or(0);
                colon = true;
                i += 3;
            }
            let mut mer = None;
            let rest: String = chars[i..chars.len().min(i + 2)]
                .iter()
                .collect::<String>()
                .to_ascii_lowercase();
            if let Some(p) = meridiem(&rest) {
                if chars.get(i + 2).is_none_or(|c| !c.is_ascii_alphabetic()) {
                    mer = Some(p);
                    i += 2;
                }
            }
            if digits > 2 {
                out.push(Tok::Word("#".into())); // a year or other number
            } else {
                out.push(Tok::Time { h, m, mer, colon });
            }
            continue;
        }
        match c {
            '-' | '–' | '—' | '‒' => out.push(Tok::Dash),
            ',' => out.push(Tok::Comma),
            '&' | '+' => out.push(Tok::Amp),
            '/' => out.push(Tok::Slash),
            ':' => out.push(Tok::Colon),
            _ if c.is_whitespace() => {}
            _ => out.push(Tok::Word(c.to_string())),
        }
        i += 1;
    }
    out
}

fn meridiem(w: &str) -> Option<bool> {
    match w {
        "am" => Some(false),
        "pm" => Some(true),
        _ => None,
    }
}

fn is_word(t: Option<&Tok>, words: &[&str]) -> bool {
    matches!(t, Some(Tok::Word(w)) if words.contains(&w.as_str()))
}

/// A day spec at `i`: its days, whether it reads as weekly (plural day,
/// a list or range, daily/weekdays), and the index after it.
fn day_spec(t: &[Tok], mut i: usize) -> Option<(Vec<u8>, bool, usize)> {
    let mut days: Vec<u8> = Vec::new();
    let mut weekly = false;
    // "daily", "every day", "weekdays", "weekends".
    match t.get(i) {
        Some(Tok::Word(w)) if w == "daily" || w == "everyday" => {
            return Some(((1..=7).collect(), true, i + 1));
        }
        Some(Tok::Word(w)) if w == "every" && is_word(t.get(i + 1), &["day"]) => {
            return Some(((1..=7).collect(), true, i + 2));
        }
        Some(Tok::Word(w)) if w == "weekdays" => return Some(((1..=5).collect(), true, i + 1)),
        Some(Tok::Word(w)) if w == "weekends" => return Some((vec![6, 7], true, i + 1)),
        _ => {}
    }
    let first = match t.get(i) {
        Some(Tok::Word(w)) => day_word(w)?,
        _ => return None,
    };
    days.push(first.0);
    weekly |= first.1;
    i += 1;
    loop {
        // Range: "tue - sun", "tuesday to sunday".
        let is_range = matches!(t.get(i), Some(Tok::Dash))
            || is_word(t.get(i), &["to", "through", "thru", "till", "until"]);
        if is_range {
            if let Some(Tok::Word(w)) = t.get(i + 1) {
                if let Some((to, p)) = day_word(w) {
                    let from = *days.last()?;
                    let mut d = from;
                    while d != to {
                        d = d % 7 + 1;
                        days.push(d);
                    }
                    weekly = true;
                    weekly |= p;
                    i += 2;
                    continue;
                }
            }
            break;
        }
        // List: ", & and /" separators, possibly several ("Fridays, & Sundays").
        let mut j = i;
        while matches!(t.get(j), Some(Tok::Comma | Tok::Amp | Tok::Slash))
            || is_word(t.get(j), &["and"])
        {
            j += 1;
        }
        if j > i {
            if let Some(Tok::Word(w)) = t.get(j) {
                if let Some((d, p)) = day_word(w) {
                    days.push(d);
                    weekly = true;
                    weekly |= p;
                    i = j + 1;
                    continue;
                }
            }
        }
        break;
    }
    Some((days, weekly, i))
}

/// A time range at `i` ("11am-3pm", "10:00 to 17:00", "between 10am and
/// 4pm"): (opens, closes, index after).
fn time_range(t: &[Tok], mut i: usize) -> Option<(NaiveTime, NaiveTime, usize)> {
    let between = is_word(t.get(i), &["between"]);
    if between {
        i += 1;
    }
    let Some(&Tok::Time {
        h: h1,
        m: m1,
        mer: mer1,
        colon: c1,
    }) = t.get(i)
    else {
        return None;
    };
    let sep = if between {
        is_word(t.get(i + 1), &["and"])
    } else {
        matches!(t.get(i + 1), Some(Tok::Dash)) || is_word(t.get(i + 1), &["to", "until", "till"])
    };
    if !sep {
        return None;
    }
    let Some(&Tok::Time {
        h: h2,
        m: m2,
        mer: mer2,
        colon: c2,
    }) = t.get(i + 2)
    else {
        return None;
    };
    let closes = clock(h2, m2, mer2, c2)?;
    let opens = match (mer1, mer2) {
        (None, Some(p)) => {
            // "11-3pm": 11 inherits pm only when that still opens first.
            let same = clock(h1, m1, Some(p), false)?;
            if same < closes {
                same
            } else {
                clock(h1, m1, Some(false), false)?
            }
        }
        _ => clock(h1, m1, mer1, c1)?,
    };
    Some((opens, closes, i + 3))
}

/// A clock time; bare numbers without am/pm or `HH:MM` are ambiguous.
fn clock(h: u32, m: u32, mer: Option<bool>, colon: bool) -> Option<NaiveTime> {
    let h = match mer {
        Some(pm) => {
            if !(1..=12).contains(&h) {
                return None;
            }
            match (h, pm) {
                (12, false) => 0,
                (12, true) => 12,
                (h, true) => h + 12,
                (h, false) => h,
            }
        }
        None if colon => h,
        None => return None,
    };
    NaiveTime::from_hms_opt(h, m, 0)
}

/// Words that make a sentence's hours unreliable as a whole schedule.
const DOUBT: [&str; 5] = ["except", "excluding", "apart", "unless", "excl"];

/// Split into sentences (". ", "; ", "!" or line breaks; "10.30" stays).
fn sentences(s: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut start = 0;
    let b = s.as_bytes();
    for (i, &c) in b.iter().enumerate() {
        let end = match c {
            b'\n' | b';' | b'!' | b'?' => true,
            b'.' => b.get(i + 1).is_none_or(|n| n.is_ascii_whitespace()),
            _ => false,
        };
        if end {
            out.push(&s[start..=i]);
            start = i + 1;
        }
    }
    if start < s.len() {
        out.push(&s[start..]);
    }
    out.into_iter()
        .map(str::trim)
        .filter(|x| !x.is_empty())
        .collect()
}

/// Read a weekly schedule from a listing's text ("Wednesdays, Thursdays,
/// Fridays & Sundays from 11am-3pm", "Open Tue–Sun, 10:00–17:00", "10am to
/// 5pm daily"). None when there is none or it is unsure.
pub fn from_text(text: &str) -> Option<ParsedHours> {
    let text = text.replace("&amp;", "&").replace('\u{a0}', " ");
    let mut rules = Vec::new();
    let mut notes: Vec<&str> = Vec::new();
    for sentence in sentences(&text) {
        let t = tokenize(sentence);
        let mut found = false;
        let mut i = 0;
        while i < t.len() {
            // Days, then optional "from"/":"/"," / "open", then a range.
            if let Some((days, weekly, mut j)) = day_spec(&t, i) {
                let closed = i > 0 && is_word(t.get(i - 1), &["closed", "except"]);
                while matches!(t.get(j), Some(Tok::Comma | Tok::Colon))
                    || is_word(t.get(j), &["from", "open", "opening", "only"])
                {
                    j += 1;
                }
                if let Some((opens, closes, k)) = time_range(&t, j) {
                    if !weekly || closed {
                        return None;
                    }
                    rules.push(HoursRule {
                        days,
                        opens,
                        closes,
                    });
                    found = true;
                    i = k;
                    continue;
                }
                i = j.max(i + 1);
                continue;
            }
            // A range, then optional "," / "on" / "every", then days.
            if let Some((opens, closes, mut j)) = time_range(&t, i) {
                while matches!(t.get(j), Some(Tok::Comma)) || is_word(t.get(j), &["on", "every"]) {
                    j += 1;
                }
                if let Some((days, weekly, k)) = day_spec(&t, j) {
                    if !weekly && !is_word(t.get(j.saturating_sub(1)), &["every"]) {
                        return None;
                    }
                    rules.push(HoursRule {
                        days,
                        opens,
                        closes,
                    });
                    found = true;
                    i = k;
                    continue;
                }
                i = j.max(i + 1);
                continue;
            }
            i += 1;
        }
        if found {
            let lower = sentence.to_ascii_lowercase();
            if tokenize(&lower)
                .iter()
                .any(|t| matches!(t, Tok::Word(w) if DOUBT.contains(&w.as_str())))
            {
                return None;
            }
            notes.push(sentence);
        }
    }
    let hours = OpeningHours::new(rules)?;
    let note = notes.join(" ");
    let note = (note.chars().count() <= MAX_NOTE).then_some(note);
    Some(ParsedHours { hours, note })
}

/// schema.org `openingHours` strings ("Mo-Fr 10:00-17:00", "Th 10:00-16:00",
/// "Tu,We 11:00-18:00"), one per rule.
pub fn from_schema_org<S: AsRef<str>>(specs: &[S]) -> Option<OpeningHours> {
    const CODES: [&str; 7] = ["mo", "tu", "we", "th", "fr", "sa", "su"];
    let code = |c: &str| {
        CODES
            .iter()
            .position(|x| c.eq_ignore_ascii_case(x))
            .map(|p| p as u8 + 1)
    };
    let mut rules = Vec::new();
    for s in specs {
        let (days_s, times) = s.as_ref().trim().split_once(' ')?;
        let mut days = Vec::new();
        for part in days_s.split(',') {
            match part.split_once('-') {
                Some((a, b)) => {
                    let (a, b) = (code(a)?, code(b)?);
                    let mut d = a;
                    days.push(d);
                    while d != b {
                        d = d % 7 + 1;
                        days.push(d);
                    }
                }
                None => days.push(code(part)?),
            }
        }
        let (o, c) = times.trim().split_once('-')?;
        rules.push(HoursRule {
            days,
            opens: NaiveTime::parse_from_str(o.trim(), "%H:%M").ok()?,
            closes: NaiveTime::parse_from_str(c.trim(), "%H:%M").ok()?,
        });
    }
    OpeningHours::new(rules)
}

/// schema.org `openingHoursSpecification` (an object or an array of them:
/// `dayOfWeek` a name or `https://schema.org/Monday`, or a list; `opens` /
/// `closes` as `HH:MM[:SS]`).
pub fn from_spec_json(v: &serde_json::Value) -> Option<OpeningHours> {
    let specs: Vec<&serde_json::Value> = match v {
        serde_json::Value::Array(a) => a.iter().collect(),
        o => vec![o],
    };
    let time = |v: Option<&serde_json::Value>| {
        let s = v?.as_str()?;
        NaiveTime::parse_from_str(s, "%H:%M:%S")
            .or_else(|_| NaiveTime::parse_from_str(s, "%H:%M"))
            .ok()
            .filter(|t| t.second() == 0)
    };
    let mut rules = Vec::new();
    for s in specs {
        let names: Vec<&str> = match s.get("dayOfWeek")? {
            serde_json::Value::String(d) => vec![d.as_str()],
            serde_json::Value::Array(a) => a.iter().filter_map(|d| d.as_str()).collect(),
            _ => return None,
        };
        let days = names
            .iter()
            .map(|n| day_word(&n.rsplit('/').next()?.to_ascii_lowercase()).map(|(d, _)| d))
            .collect::<Option<Vec<u8>>>()?;
        rules.push(HoursRule {
            days,
            opens: time(s.get("opens"))?,
            closes: time(s.get("closes"))?,
        });
    }
    OpeningHours::new(rules)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn t(h: u32, m: u32) -> NaiveTime {
        NaiveTime::from_hms_opt(h, m, 0).unwrap()
    }

    fn rule(days: &[u8], o: NaiveTime, c: NaiveTime) -> HoursRule {
        HoursRule {
            days: days.to_vec(),
            opens: o,
            closes: c,
        }
    }

    /// The Chats Palace listing (issue #168), as the TEC feed gives it.
    const CHATS_PALACE: &str = "Once established, Chats Palace became the engine room for the \
        festival. The exhibition will be showing September and October 2026.  For the duration \
        of its stay, the Exhibition will be on display on Wednesdays, Thursday, Fridays, &amp; \
        Sundays from 11am-3pm.";

    #[test]
    fn chats_palace() {
        let p = from_text(CHATS_PALACE).unwrap();
        assert_eq!(p.hours.0, vec![rule(&[3, 4, 5, 7], t(11, 0), t(15, 0))]);
        assert_eq!(p.hours.display(), "Wed, Thu, Fri, Sun · 11:00–15:00");
        assert_eq!(
            p.note.as_deref(),
            Some(
                "For the duration of its stay, the Exhibition will be on display on Wednesdays, \
                 Thursday, Fridays, & Sundays from 11am-3pm."
            )
        );
    }

    #[test]
    fn common_wordings() {
        let cases: &[(&str, Vec<HoursRule>)] = &[
            (
                "Open Tue–Sun, 10:00–17:00.",
                vec![rule(&[2, 3, 4, 5, 6, 7], t(10, 0), t(17, 0))],
            ),
            (
                "Open daily 10am – 5.30pm",
                vec![rule(&[1, 2, 3, 4, 5, 6, 7], t(10, 0), t(17, 30))],
            ),
            (
                "10am to 6pm, Thursday to Sunday",
                vec![rule(&[4, 5, 6, 7], t(10, 0), t(18, 0))],
            ),
            (
                "Gallery hours: Tuesday - Friday 11-6pm; Saturdays 12pm-5pm",
                vec![
                    rule(&[2, 3, 4, 5], t(11, 0), t(18, 0)),
                    rule(&[6], t(12, 0), t(17, 0)),
                ],
            ),
            (
                "Fri - Mon: 12 noon - 8pm",
                // "12 noon" is not a time we read: nothing.
                vec![],
            ),
            (
                "Saturdays and Sundays between 10am and 4pm",
                vec![rule(&[6, 7], t(10, 0), t(16, 0))],
            ),
            (
                "Open Fridays 18:00-22:00",
                vec![rule(&[5], t(18, 0), t(22, 0))],
            ),
        ];
        for (text, want) in cases {
            let got = from_text(text).map(|p| p.hours.0).unwrap_or_default();
            assert_eq!(&got, want, "{text}");
        }
    }

    #[test]
    fn unsure_text_gives_no_hours() {
        for text in [
            // A single date, not a weekly schedule.
            "Private view: Thursday 2 October, 6-8pm.",
            "Join us on Saturday 11am-3pm for a drop-in.",
            // Ambiguous bare numbers.
            "Open Wed-Sun 10-5",
            // Exceptions we cannot model.
            "Open daily 10am-5pm except bank holidays.",
            // Conflicting rules for the same day.
            "Open Mon-Fri 10am-5pm. Open Fridays 10am-9pm.",
            // Overnight / past midnight is dropped, not guessed.
            "Open Fridays and Saturdays 10pm-2am",
            "Open Thursdays 6pm-12am",
            // A closure, not opening hours.
            "Closed Mondays 1pm-2pm for lunch",
            // Times without days.
            "Doors 7pm, talk 7.30-9pm.",
            "The exhibition runs 2020-2024.",
        ] {
            assert_eq!(from_text(text), None, "{text}");
        }
    }

    #[test]
    fn long_notes_are_not_kept() {
        let long = format!("{} Open Wed-Sun 11am-6pm.", "word ".repeat(60));
        let p = from_text(&long).unwrap();
        assert_eq!(p.hours.display(), "Wed, Thu, Fri, Sat, Sun · 11:00–18:00");
        assert_eq!(p.note, None);
    }

    #[test]
    fn schema_org_forms() {
        assert_eq!(
            from_schema_org(&["Th 10:00-16:00", "Sa 10:00-14:00"])
                .unwrap()
                .0,
            vec![
                rule(&[4], t(10, 0), t(16, 0)),
                rule(&[6], t(10, 0), t(14, 0))
            ]
        );
        assert_eq!(
            from_schema_org(&["Mo-Fr 10:00-17:00"]).unwrap().display(),
            "Mon, Tue, Wed, Thu, Fri · 10:00–17:00"
        );
        assert_eq!(from_schema_org(&["Xx 10:00-17:00"]), None);
        assert_eq!(from_schema_org(&["Mo 18:00-02:00"]), None);
        let spec = serde_json::json!([
            {"@type": "OpeningHoursSpecification",
             "dayOfWeek": ["https://schema.org/Saturday", "Sunday"],
             "opens": "10:00:00", "closes": "18:00:00"}
        ]);
        assert_eq!(
            from_spec_json(&spec).unwrap().0,
            vec![rule(&[6, 7], t(10, 0), t(18, 0))]
        );
        assert_eq!(
            from_spec_json(
                &serde_json::json!({"dayOfWeek": "Funday", "opens": "10:00", "closes": "11:00"})
            ),
            None
        );
    }

    #[test]
    fn json_shape_round_trips() {
        let h = OpeningHours::new(vec![rule(&[7, 3], t(11, 0), t(15, 0))]).unwrap();
        let j = serde_json::to_value(&h).unwrap();
        assert_eq!(
            j,
            serde_json::json!([{"days": [3, 7], "opens": "11:00", "closes": "15:00"}])
        );
        assert_eq!(serde_json::from_value::<OpeningHours>(j).unwrap(), h);
    }

    #[test]
    fn open_at_across_the_clock_change() {
        let h = from_text(CHATS_PALACE).unwrap().hours;
        let at = |y, mo, d, hh, mm| Utc.with_ymd_and_hms(y, mo, d, hh, mm, 0).unwrap();
        // Sun 18 Oct 2026 is BST: 10:30Z = 11:30 London.
        assert!(h.is_open_at(at(2026, 10, 18, 10, 30)));
        // Sun 25 Oct 2026 is GMT (clocks went back): 10:30Z = 10:30 London.
        assert!(!h.is_open_at(at(2026, 10, 25, 10, 30)));
        assert!(h.is_open_at(at(2026, 10, 25, 11, 30)));
        // Closing time is exclusive: 15:00 BST on Wed 14 Oct.
        assert!(!h.is_open_at(at(2026, 10, 14, 14, 0)));
        assert!(h.is_open_at(at(2026, 10, 14, 13, 59)));
        // The issue's case: a Monday at 21:00 is closed.
        assert!(!h.is_open_at(at(2026, 10, 12, 20, 0)));
        assert_eq!(h.today_status(at(2026, 10, 12, 20, 0)), "Closed today");
        assert_eq!(
            h.today_status(at(2026, 10, 14, 9, 0)),
            "Opens today at 11:00"
        );
        assert_eq!(
            h.today_status(at(2026, 10, 14, 11, 0)),
            "Open now until 15:00"
        );
        assert_eq!(h.today_status(at(2026, 10, 14, 15, 0)), "Closed now");
    }

    #[test]
    fn days() {
        assert_eq!(parse_day("Sun"), Some(7));
        assert_eq!(parse_day("monday"), Some(1));
        assert_eq!(parse_day("xyz"), None);
        assert_eq!(day_code(3), "wed");
    }
}
