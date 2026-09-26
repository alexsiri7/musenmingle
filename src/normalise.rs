//! Normalisation helpers shared by all sources.
//!
//! Everything here is pure (no I/O, no clock) so it is unit-testable.
//!
//! # Dedupe key algorithm
//!
//! `dedupe_key(title, starts_at, venue)` produces `"{title}|{date}|{venue}"`:
//!
//! 1. **title**: lowercased; `&` and `+` become spaces; every character that is
//!    not alphanumeric becomes a space; common Latin diacritics are folded
//!    (`é` → `e`); the stop words `the`, `a`, `an`, `and` are dropped; the
//!    remaining words are joined with `-`.
//! 2. **date**: `starts_at` converted to **Europe/London** local time and
//!    formatted `YYYY-MM-DD`. Using the London date (not the UTC date) keeps a
//!    23:30 BST event on the right day regardless of how a source expressed it.
//! 3. **venue**: normalised like the title, additionally dropping generic venue
//!    words (`london`, `gallery`, `galleries`, `centre`, `center`, `museum`,
//!    `theatre`, `theater`, `hall`), so "Serpentine North Gallery" and
//!    "Serpentine North" agree. A missing venue becomes `unknown`.
//!
//! Two sources describing the same event therefore collide on the key and are
//! merged by `repo::upsert_event`. Near-matches (differing titles, venue names
//! or exhibition opening dates) are handled there by `crate::matching`.

use chrono::{DateTime, NaiveDate, NaiveDateTime, TimeZone, Utc};
use chrono_tz::Europe::London;
use rust_decimal::Decimal;
use std::str::FromStr;

use crate::model::{Category, Price};

/// Maximum stored description length (characters).
pub const MAX_DESCRIPTION_CHARS: usize = 5_000;

/// Strip HTML tags (dropping `<script>`/`<style>` content), decode entities
/// and collapse all whitespace (including NBSP) to single spaces.
pub fn clean_text(input: &str) -> String {
    let text = if input.contains('<') || input.contains('&') {
        let frag = scraper::Html::parse_fragment(input);
        let mut parts: Vec<String> = Vec::new();
        for node in frag.tree.root().descendants() {
            if let Some(t) = node.value().as_text() {
                let in_ignored = node.ancestors().any(|a| {
                    a.value()
                        .as_element()
                        .is_some_and(|e| matches!(e.name(), "script" | "style"))
                });
                if !in_ignored {
                    parts.push(t.to_string());
                }
            }
        }
        parts.join(" ")
    } else {
        input.to_string()
    };
    collapse_whitespace(&text)
}

fn collapse_whitespace(s: &str) -> String {
    s.split(|c: char| c.is_whitespace() || c == '\u{a0}')
        .filter(|w| !w.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
        // Tidy spaces introduced before punctuation by tag stripping.
        .replace(" ,", ",")
        .replace(" .", ".")
}

/// Clean an optional description; empty results become `None` and long ones
/// are truncated on a character boundary with an ellipsis.
pub fn clean_description(input: Option<&str>) -> Option<String> {
    let s = clean_text(input?);
    if s.is_empty() {
        return None;
    }
    if s.chars().count() > MAX_DESCRIPTION_CHARS {
        let mut t: String = s.chars().take(MAX_DESCRIPTION_CHARS - 1).collect();
        t.push('…');
        Some(t)
    } else {
        Some(s)
    }
}

/// Interpret a wall-clock time in Europe/London and convert it to UTC.
///
/// Ambiguous times (the repeated hour when clocks go back) resolve to the
/// earlier instant; non-existent times (the skipped hour when clocks go
/// forward) are shifted forward one hour.
pub fn london_to_utc(local: NaiveDateTime) -> DateTime<Utc> {
    use chrono::LocalResult;
    match London.from_local_datetime(&local) {
        LocalResult::Single(t) => t.with_timezone(&Utc),
        LocalResult::Ambiguous(a, _) => a.with_timezone(&Utc),
        LocalResult::None => {
            let shifted = local + chrono::Duration::hours(1);
            London
                .from_local_datetime(&shifted)
                .earliest()
                .map(|t| t.with_timezone(&Utc))
                .unwrap_or_else(|| Utc.from_utc_datetime(&local))
        }
    }
}

/// The Europe/London calendar date of an instant.
pub fn london_date(t: DateTime<Utc>) -> NaiveDate {
    t.with_timezone(&London).date_naive()
}

const NAIVE_FORMATS: &[&str] = &[
    "%Y-%m-%dT%H:%M:%S%.f",
    "%Y-%m-%dT%H:%M:%S",
    "%Y-%m-%dT%H:%M",
    "%Y-%m-%d %H:%M:%S",
    "%Y-%m-%d %H:%M",
];

/// Parse a date/time string.
///
/// * RFC 3339 with an offset (`2026-10-02T20:00:00+01:00`, `...Z`): the offset
///   is honoured.
/// * Naive date-time (`2026-10-02T20:00`): interpreted as Europe/London.
/// * Date only (`2026-10-02`): London midnight.
pub fn parse_datetime(s: &str) -> Option<DateTime<Utc>> {
    let s = s.trim();
    if let Ok(t) = DateTime::parse_from_rfc3339(s) {
        return Some(t.with_timezone(&Utc));
    }
    parse_london_wall_clock(s)
}

/// Parse a date/time string as Europe/London wall-clock time, IGNORING any
/// offset present. For sources known to emit local times with a bogus offset
/// (e.g. `+00:00` in summer).
pub fn parse_london_wall_clock(s: &str) -> Option<DateTime<Utc>> {
    let s = s.trim();
    if let Ok(t) = DateTime::parse_from_rfc3339(s) {
        return Some(london_to_utc(t.naive_local()));
    }
    for f in NAIVE_FORMATS {
        if let Ok(n) = NaiveDateTime::parse_from_str(s, f) {
            return Some(london_to_utc(n));
        }
    }
    NaiveDate::parse_from_str(s, "%Y-%m-%d")
        .ok()
        .and_then(|d| d.and_hms_opt(0, 0, 0))
        .map(london_to_utc)
}

fn currency_for_symbol(c: char) -> Option<&'static str> {
    match c {
        '£' => Some("GBP"),
        '€' => Some("EUR"),
        '$' => Some("USD"),
        _ => None,
    }
}

fn contains_word(haystack: &str, word: &str) -> bool {
    haystack
        .split(|c: char| !c.is_alphanumeric())
        .any(|w| w == word)
}

/// Parse free-text price information such as `"£10, £7 conc."`,
/// `"Free"`, `"Free entry, donations welcome"`, `"GBP 12.50 - 20"`.
///
/// Only amounts attached to a currency (a symbol or ISO code before/after)
/// are considered, so dates and times in the same string are ignored.
/// `is_free` is true when the word "free" appears and no positive amount is
/// found, or when every amount found is zero.
pub fn parse_price(text: &str) -> Price {
    let lower = text.to_lowercase();
    let mut amounts: Vec<Decimal> = Vec::new();
    let mut currency: Option<String> = None;

    let chars: Vec<char> = text.chars().collect();
    let mut i = 0;
    let mut pending_currency: Option<&'static str> = None;
    let mut last_currency: Option<&'static str> = None;
    while i < chars.len() {
        let c = chars[i];
        if let Some(cur) = currency_for_symbol(c) {
            pending_currency = Some(cur);
            i += 1;
            continue;
        }
        // ISO code prefix, e.g. "GBP 12".
        if c.is_ascii_uppercase() && i + 3 <= chars.len() {
            let code: String = chars[i..i + 3].iter().collect();
            if matches!(code.as_str(), "GBP" | "EUR" | "USD") {
                pending_currency = Some(match code.as_str() {
                    "GBP" => "GBP",
                    "EUR" => "EUR",
                    _ => "USD",
                });
                i += 3;
                continue;
            }
        }
        if c.is_ascii_digit() {
            let start = i;
            while i < chars.len()
                && (chars[i].is_ascii_digit() || chars[i] == '.' || chars[i] == ',')
            {
                // Stop at a comma used as a list separator ("£10, £7").
                if chars[i] == ',' && !(i + 1 < chars.len() && chars[i + 1].is_ascii_digit()) {
                    break;
                }
                i += 1;
            }
            let num: String = chars[start..i]
                .iter()
                .filter(|c| **c != ',')
                .collect::<String>()
                .trim_end_matches('.')
                .to_string();
            // Continuation of a range after a currency amount: "£10 - 20"/"£10–£20".
            let prev_non_space = chars[..start].iter().rev().find(|c| !c.is_whitespace());
            let is_range_tail =
                last_currency.is_some() && matches!(prev_non_space, Some('-' | '–' | '—'));
            let cur = pending_currency
                .take()
                .or(if is_range_tail { last_currency } else { None });
            if let (Some(cur), Ok(d)) = (cur, Decimal::from_str(&num)) {
                amounts.push(d);
                currency.get_or_insert_with(|| cur.to_string());
                last_currency = Some(cur);
            }
            continue;
        }
        if !c.is_whitespace() {
            pending_currency = None;
        }
        i += 1;
    }

    let min = amounts.iter().min().copied();
    let max = amounts.iter().max().copied();
    let all_zero = !amounts.is_empty() && amounts.iter().all(|a| a.is_zero());
    let has_positive = amounts.iter().any(|a| *a > Decimal::ZERO);
    let is_free = all_zero || (contains_word(&lower, "free") && !has_positive);
    Price {
        is_free,
        min: if is_free && min.is_none() {
            Some(Decimal::ZERO)
        } else {
            min
        },
        max: if is_free && max.is_none() {
            Some(Decimal::ZERO)
        } else {
            max
        },
        currency,
    }
}

/// Admission phrases, as lower-case token sequences, that make prose say an
/// event is free. Bare "free", "free access" ("step-free access") and
/// "for free" ("join Young Barbican for free") are deliberately absent.
const FREE_PHRASES: &[&[&str]] = &[
    &["free", "entry"],
    &["free", "admission"],
    &["free", "event"],
    &["free", "exhibition"],
    &["free", "installation"],
    &["free", "display"],
    &["free", "talk"],
    &["free", "tour"],
    &["free", "to", "attend"],
    &["free", "to", "enter"],
    &["free", "to", "visit"],
    &["entry", "is", "free"],
    &["admission", "is", "free"],
    &["free", "of", "charge"],
];

/// Whether a description explicitly says the event is free to attend
/// ("This free installation…", "Entry is free"). A fallback for sources with
/// no structured price: it only understands explicit admission wording, and
/// any positive currency amount in the text vetoes it.
pub fn describes_free_entry(text: &str) -> bool {
    let lower = text.to_lowercase();
    // Hyphens stay inside tokens so "step-free" and "debt-free" never read as "free".
    let tokens: Vec<&str> = lower
        .split(|c: char| !c.is_alphanumeric() && c != '-')
        .filter(|t| !t.is_empty())
        .collect();
    FREE_PHRASES
        .iter()
        .any(|p| tokens.windows(p.len()).any(|w| w == *p))
        && !parse_price(text).max.is_some_and(|m| m > Decimal::ZERO)
}

/// Build a [`Price`] from structured min/max amounts (e.g. an API).
pub fn price_from_amounts(
    min: Option<Decimal>,
    max: Option<Decimal>,
    currency: Option<&str>,
) -> Price {
    let (min, max) = match (min, max) {
        (Some(a), Some(b)) if b < a => (Some(b), Some(a)),
        (Some(a), None) => (Some(a), Some(a)),
        (None, Some(b)) => (Some(b), Some(b)),
        other => other,
    };
    let is_free = max.is_some_and(|m| m.is_zero());
    Price {
        is_free,
        min: min.map(|d| d.round_dp(2).normalize()),
        max: max.map(|d| d.round_dp(2).normalize()),
        currency: currency
            .map(|c| c.trim().to_ascii_uppercase())
            .filter(|c| !c.is_empty()),
    }
}

/// Keyword table for [`map_category`], checked in order (first match wins).
/// Multi-word phrases are matched on word boundaries.
const CATEGORY_KEYWORDS: &[(Category, &[&str])] = &[
    (
        Category::Workshop,
        &[
            "workshop",
            "workshops",
            "masterclass",
            "class",
            "classes",
            "course",
            "hands on",
        ],
    ),
    (
        Category::Talk,
        &[
            "talk",
            "talks",
            "lecture",
            "lectures",
            "seminar",
            "symposium",
            "panel",
            "in conversation",
            "conversation",
            "discussion",
            "reading",
            "spoken word",
            "lecture seminar",
        ],
    ),
    (
        Category::Expo,
        &[
            "expo",
            "expos",
            "fair",
            "fairs",
            "convention",
            "trade show",
            "exposition",
        ],
    ),
    (
        Category::Exhibition,
        &[
            "exhibition",
            "exhibitions",
            "fine art",
            "installation",
            "display",
            "retrospective",
        ],
    ),
    (
        Category::Community,
        &[
            "community",
            "meetup",
            "meet up",
            "networking",
            "creativemornings",
            "writing group",
            "social",
            "club",
            "civic",
        ],
    ),
];

/// Tokenise into lowercase alphanumeric words (diacritics folded).
pub(crate) fn words(s: &str) -> Vec<String> {
    fold_diacritics(&s.to_lowercase())
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .map(str::to_string)
        .collect()
}

fn contains_phrase(hay: &[String], phrase: &str) -> bool {
    let p: Vec<&str> = phrase.split(' ').collect();
    hay.windows(p.len())
        .any(|w| w.iter().zip(&p).all(|(a, b)| a == b))
}

/// Map free-text hints (API classifications, titles, URL slugs) to a
/// category. Hints are checked in the order given, and within each hint the
/// keyword table is checked in order, so pass the most specific hint first.
/// Returns `None` when nothing matches: the caller decides whether to skip.
pub fn map_category<S: AsRef<str>>(hints: &[S]) -> Option<Category> {
    for hint in hints {
        let w = words(hint.as_ref());
        if w.is_empty() {
            continue;
        }
        for (cat, keywords) in CATEGORY_KEYWORDS {
            if keywords.iter().any(|k| contains_phrase(&w, k)) {
                return Some(*cat);
            }
        }
    }
    None
}

fn fold_diacritics(s: &str) -> String {
    s.chars()
        .map(|c| match c {
            'à' | 'á' | 'â' | 'ã' | 'ä' | 'å' => 'a',
            'ç' => 'c',
            'è' | 'é' | 'ê' | 'ë' => 'e',
            'ì' | 'í' | 'î' | 'ï' => 'i',
            'ñ' => 'n',
            'ò' | 'ó' | 'ô' | 'õ' | 'ö' | 'ø' => 'o',
            'ù' | 'ú' | 'û' | 'ü' => 'u',
            'ý' | 'ÿ' => 'y',
            '’' | '\'' => '\0',
            other => other,
        })
        .filter(|c| *c != '\0')
        .collect()
}

pub(crate) const TITLE_STOP_WORDS: &[&str] = &["the", "a", "an", "and"];
const VENUE_STOP_WORDS: &[&str] = &[
    "the",
    "a",
    "an",
    "and",
    "london",
    "gallery",
    "galleries",
    "centre",
    "center",
    "museum",
    "theatre",
    "theater",
    "hall",
];

fn key_part(s: &str, stop: &[&str]) -> String {
    words(s)
        .into_iter()
        .filter(|w| !stop.contains(&w.as_str()))
        .collect::<Vec<_>>()
        .join("-")
}

/// Normalised title component of the dedupe key.
pub fn normalise_title_for_key(title: &str) -> String {
    key_part(title, TITLE_STOP_WORDS)
}

/// Normalised venue component of the dedupe key.
pub fn normalise_venue_for_key(venue: Option<&str>) -> String {
    let v = venue
        .map(|v| key_part(v, VENUE_STOP_WORDS))
        .unwrap_or_default();
    if v.is_empty() { "unknown".into() } else { v }
}

/// Cross-source dedupe key; see the module docs for the algorithm.
pub fn dedupe_key(title: &str, starts_at: DateTime<Utc>, venue: Option<&str>) -> String {
    format!(
        "{}|{}|{}",
        normalise_title_for_key(title),
        london_date(starts_at).format("%Y-%m-%d"),
        normalise_venue_for_key(venue)
    )
}

/// Maximum length (characters, ellipsis included) of a stored description.
/// We keep only a short excerpt and link out to the venue for the rest.
pub const EXCERPT_MAX_CHARS: usize = 300;

/// A sentence boundary earlier than this is too short to be a useful
/// excerpt; cut at a word boundary instead.
const EXCERPT_MIN_SENTENCE_CHARS: usize = 80;

/// Cut `text` to a short excerpt of at most [`EXCERPT_MAX_CHARS`]
/// characters. Text that already fits is returned unchanged (trimmed).
/// Longer text is cut after the last complete sentence that fits
/// (followed by " …"), else at the last word boundary, else mid-word
/// (followed by "…"). Applied to every description at persistence time
/// (`repo::upsert_event`), not by sources.
pub fn excerpt(text: &str) -> String {
    let text = text.trim();
    if text.chars().count() <= EXCERPT_MAX_CHARS {
        return text.to_string();
    }
    let chars: Vec<(usize, char)> = text.char_indices().collect();
    // Room for " …" after a sentence.
    let sentence_room = EXCERPT_MAX_CHARS - 2;
    let mut sentence_end: Option<usize> = None;
    for i in EXCERPT_MIN_SENTENCE_CHARS.saturating_sub(1)..sentence_room.min(chars.len()) {
        let c = chars[i].1;
        let next = chars.get(i + 1).map(|(_, c)| *c);
        let ends = match c {
            '.' | '!' | '?' | '…' => next.is_none_or(char::is_whitespace),
            '。' | '！' | '？' => true,
            _ => false,
        };
        if ends {
            sentence_end = Some(i);
        }
    }
    if let Some(i) = sentence_end {
        let end = chars[i].0 + chars[i].1.len_utf8();
        return format!("{} …", &text[..end]);
    }
    // Room for "…".
    let room = EXCERPT_MAX_CHARS - 1;
    let cut = chars[room].0;
    let head = &text[..cut];
    let head = match head.rfind(char::is_whitespace) {
        Some(ws) if ws > 0 => &head[..ws],
        _ => head,
    };
    let head = head.trim_end_matches(|c: char| c.is_whitespace() || ",;:-–—".contains(c));
    format!("{head}…")
}

/// Outward code (e.g. `E1W`) of the first UK postcode in `text`.
pub fn postcode_outward(text: &str) -> Option<String> {
    let tokens: Vec<&str> = text
        .split(|c: char| c.is_whitespace() || c == ',')
        .filter(|t| !t.is_empty())
        .collect();
    tokens.windows(2).find_map(|pair| {
        let (outward, inward) = (pair[0], pair[1].as_bytes());
        let outward_ok = (2..=4).contains(&outward.len())
            && outward.chars().all(|c| c.is_ascii_alphanumeric())
            && outward.starts_with(|c: char| c.is_ascii_alphabetic())
            && outward.chars().any(|c| c.is_ascii_digit());
        let inward_ok = inward.len() == 3
            && inward[0].is_ascii_digit()
            && inward[1..].iter().all(u8::is_ascii_alphabetic);
        (outward_ok && inward_ok).then(|| outward.to_ascii_uppercase())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(s: &str) -> Decimal {
        Decimal::from_str(s).unwrap()
    }

    fn utc(s: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
    }

    #[test]
    fn clean_text_strips_html_and_whitespace() {
        let cases = [
            ("  hello   world ", "hello world"),
            ("<p>First</p><p>Second&nbsp;para</p>", "First Second para"),
            ("Tom &amp; Jerry<br/>live", "Tom & Jerry live"),
            ("a<script>alert(1)</script>b<style>p{}</style>", "a b"),
            ("Line\n\n\tbreaks\u{a0}and nbsp", "Line breaks and nbsp"),
            ("<b>Bold</b>, then <i>italic</i>.", "Bold, then italic."),
            ("", ""),
        ];
        for (input, want) in cases {
            assert_eq!(clean_text(input), want, "input {input:?}");
        }
    }

    #[test]
    fn clean_description_handles_empty_and_long() {
        assert_eq!(clean_description(None), None);
        assert_eq!(clean_description(Some("  <p> </p> ")), None);
        let long = "x".repeat(MAX_DESCRIPTION_CHARS + 10);
        let d = clean_description(Some(&long)).unwrap();
        assert_eq!(d.chars().count(), MAX_DESCRIPTION_CHARS);
        assert!(d.ends_with('…'));
    }

    #[test]
    fn london_times_convert_to_utc() {
        let n = |s: &str| NaiveDateTime::parse_from_str(s, "%Y-%m-%dT%H:%M").unwrap();
        // BST (UTC+1)
        assert_eq!(
            london_to_utc(n("2026-07-01T20:00")),
            utc("2026-07-01T19:00:00Z")
        );
        // GMT (UTC+0)
        assert_eq!(
            london_to_utc(n("2026-12-01T20:00")),
            utc("2026-12-01T20:00:00Z")
        );
        // Clocks go forward 2026-03-29 01:00 GMT -> 02:00 BST: 01:30 does not exist.
        assert_eq!(
            london_to_utc(n("2026-03-29T01:30")),
            utc("2026-03-29T01:30:00Z")
        );
        // Clocks go back 2026-10-25 02:00 BST -> 01:00 GMT: 01:30 is ambiguous, take earlier.
        assert_eq!(
            london_to_utc(n("2026-10-25T01:30")),
            utc("2026-10-25T00:30:00Z")
        );
    }

    #[test]
    fn parse_datetime_variants() {
        assert_eq!(
            parse_datetime("2026-10-02T20:00:00Z"),
            Some(utc("2026-10-02T20:00:00Z"))
        );
        assert_eq!(
            parse_datetime("2026-10-02T20:00:00+01:00"),
            Some(utc("2026-10-02T19:00:00Z"))
        );
        assert_eq!(
            parse_datetime("2026-10-02T20:00"),
            Some(utc("2026-10-02T19:00:00Z"))
        );
        assert_eq!(
            parse_datetime("2026-10-02"),
            Some(utc("2026-10-01T23:00:00Z"))
        );
        assert_eq!(
            parse_datetime("2026-01-15"),
            Some(utc("2026-01-15T00:00:00Z"))
        );
        assert_eq!(parse_datetime("not a date"), None);
    }

    #[test]
    fn wall_clock_ignores_bogus_offset() {
        // A site printing 8pm London time as "+00:00" during BST.
        assert_eq!(
            parse_london_wall_clock("2026-10-02T20:00:00+00:00"),
            Some(utc("2026-10-02T19:00:00Z"))
        );
        assert_eq!(
            parse_london_wall_clock("2026-12-02T20:00:00+00:00"),
            Some(utc("2026-12-02T20:00:00Z"))
        );
    }

    #[test]
    fn free_wording_table() {
        let cases = [
            (
                "This free installation invites visitors to orient themselves in the buildings",
                true,
            ),
            (
                "This free exhibition presents video works by Lawrence Abu Hamdan",
                true,
            ),
            ("Free entry", true),
            ("Free admission, donations welcome", true),
            ("A free event for all ages", true),
            ("Free to attend, booking recommended", true),
            ("Entry is free", true),
            ("FREE ENTRY", true),
            ("free drink with ticket £12", false),
            (
                "This free installation is accompanied by a workshop, £12",
                false,
            ),
            ("Step-free access from the Pit floor foyer", false),
            ("16–29? Join Young Barbican for free", false),
            ("went from nothing to debt-free", false),
            ("a supportive, judgment-free space", false),
            ("goes deep on sound, beauty and creative freedom", false),
            ("check bags into our free cloakrooms", false),
            ("we're giving away FREE advance copies", false),
            ("", false),
        ];
        for (input, expected) in cases {
            assert_eq!(describes_free_entry(input), expected, "{input:?}");
        }
    }

    #[test]
    fn price_parsing_table() {
        struct Case {
            input: &'static str,
            free: bool,
            min: Option<Decimal>,
            max: Option<Decimal>,
            currency: Option<&'static str>,
        }
        let cases = [
            Case {
                input: "Free",
                free: true,
                min: Some(d("0")),
                max: Some(d("0")),
                currency: None,
            },
            Case {
                input: "Price: Free ",
                free: true,
                min: Some(d("0")),
                max: Some(d("0")),
                currency: None,
            },
            Case {
                input: "FREE entry, booking required",
                free: true,
                min: Some(d("0")),
                max: Some(d("0")),
                currency: None,
            },
            Case {
                input: "Price: £10, £7 conc.",
                free: false,
                min: Some(d("7")),
                max: Some(d("10")),
                currency: Some("GBP"),
            },
            Case {
                input: "£8",
                free: false,
                min: Some(d("8")),
                max: Some(d("8")),
                currency: Some("GBP"),
            },
            Case {
                input: "£12.50 - 20",
                free: false,
                min: Some(d("12.50")),
                max: Some(d("20")),
                currency: Some("GBP"),
            },
            Case {
                input: "£5–£15",
                free: false,
                min: Some(d("5")),
                max: Some(d("15")),
                currency: Some("GBP"),
            },
            Case {
                input: "GBP 1,200",
                free: false,
                min: Some(d("1200")),
                max: Some(d("1200")),
                currency: Some("GBP"),
            },
            Case {
                input: "£0",
                free: true,
                min: Some(d("0")),
                max: Some(d("0")),
                currency: Some("GBP"),
            },
            // Dates/times without a currency are not prices.
            Case {
                input: "2 & 3 October 2026, 8pm",
                free: false,
                min: None,
                max: None,
                currency: None,
            },
            // "free" with a positive price is not free.
            Case {
                input: "Free for members, £5 otherwise",
                free: false,
                min: Some(d("5")),
                max: Some(d("5")),
                currency: Some("GBP"),
            },
            // "gluten-free" contains the word free... a known, accepted false positive.
            Case {
                input: "Tickets on the door",
                free: false,
                min: None,
                max: None,
                currency: None,
            },
        ];
        for c in cases {
            let p = parse_price(c.input);
            assert_eq!(p.is_free, c.free, "free for {:?}", c.input);
            assert_eq!(p.min, c.min, "min for {:?}", c.input);
            assert_eq!(p.max, c.max, "max for {:?}", c.input);
            assert_eq!(
                p.currency.as_deref(),
                c.currency,
                "currency for {:?}",
                c.input
            );
        }
    }

    #[test]
    fn price_from_amounts_orders_and_detects_free() {
        let p = price_from_amounts(Some(d("20")), Some(d("10")), Some("gbp"));
        assert_eq!(
            (p.min, p.max, p.is_free),
            (Some(d("10")), Some(d("20")), false)
        );
        assert_eq!(p.currency.as_deref(), Some("GBP"));
        assert!(price_from_amounts(Some(d("0")), Some(d("0")), None).is_free);
        assert!(!price_from_amounts(None, None, None).is_free);
    }

    #[test]
    fn category_mapping_table() {
        let cases: &[(&[&str], Option<Category>)] = &[
            (&["Fine Art"], Some(Category::Exhibition)),
            (&["Hobby/Special Interest Expos"], Some(Category::Expo)),
            (&["Lecture/Seminar"], Some(Category::Talk)),
            (&["Saturday Talks: Liz Stumpf"], Some(Category::Talk)),
            (&["Printmaking Workshop"], Some(Category::Workshop)),
            (&["CreativeMornings London"], Some(Category::Community)),
            (&["Community/Civic"], Some(Category::Community)),
            (&["London Art Fair"], Some(Category::Expo)),
            (&["Theatre", "Musical"], None),
            (&["Rock"], None),
            // Earlier hints win.
            (&["Workshop", "Exhibition"], Some(Category::Workshop)),
            (&["", "Exhibition"], Some(Category::Exhibition)),
            // Word boundaries: "classic" is not "class", "fairy" not "fair".
            (&["Classic Fairy Tales"], None),
        ];
        for (hints, want) in cases {
            assert_eq!(map_category(hints), *want, "hints {hints:?}");
        }
    }

    #[test]
    fn dedupe_key_same_event_from_two_sources() {
        // Ticketmaster-style: UTC instant, venue with suffix.
        let a = dedupe_key(
            "Park Nights 2026: Shala Miller",
            utc("2026-10-02T19:00:00Z"),
            Some("Serpentine North Gallery"),
        );
        // Venue site: different punctuation/case, offset-bearing local time, shorter venue.
        let b = dedupe_key(
            "PARK NIGHTS 2026 – Shala Miller",
            utc("2026-10-02T20:00:00+01:00"),
            Some("The Serpentine North"),
        );
        assert_eq!(a, b);
        assert_eq!(
            a,
            "park-nights-2026-shala-miller|2026-10-02|serpentine-north"
        );
    }

    #[test]
    fn dedupe_key_uses_london_date() {
        // 23:30 BST on 2 Oct is 22:30 UTC on 2 Oct; 00:30 BST on 3 Oct is 23:30 UTC on 2 Oct.
        let late = dedupe_key("Night Talk", utc("2026-10-02T23:30:00Z"), Some("ICA"));
        assert!(late.contains("|2026-10-03|"), "{late}");
    }

    #[test]
    fn dedupe_key_differs_by_date_title_and_venue() {
        let base = dedupe_key(
            "Drawing Club",
            utc("2026-10-02T18:00:00Z"),
            Some("Barbican"),
        );
        assert_ne!(
            base,
            dedupe_key(
                "Drawing Club",
                utc("2026-10-09T18:00:00Z"),
                Some("Barbican")
            )
        );
        assert_ne!(
            base,
            dedupe_key(
                "Writing Club",
                utc("2026-10-02T18:00:00Z"),
                Some("Barbican")
            )
        );
        assert_ne!(
            base,
            dedupe_key(
                "Drawing Club",
                utc("2026-10-02T18:00:00Z"),
                Some("Tate Modern")
            )
        );
        // Same day, different time: same key (one event per title/venue/day).
        assert_eq!(
            base,
            dedupe_key(
                "Drawing Club",
                utc("2026-10-02T19:00:00Z"),
                Some("Barbican Centre")
            )
        );
    }

    #[test]
    fn dedupe_key_folds_symbols_and_diacritics() {
        let a = dedupe_key("Café & Conversation", utc("2026-10-02T18:00:00Z"), None);
        let b = dedupe_key("Cafe and Conversation", utc("2026-10-02T18:00:00Z"), None);
        assert_eq!(a, b);
        assert!(a.ends_with("|unknown"));
    }

    #[test]
    fn excerpt_rule() {
        let n = EXCERPT_MAX_CHARS;
        let long_words = "word ".repeat(100);
        let sentences = format!(
            "{} First sentence ends here. {}",
            "a".repeat(100),
            "b ".repeat(200)
        );
        let short_sentence_then_words = format!("Hi. {}", "word ".repeat(100));
        let cjk = "日本語".repeat(150);
        let accented = format!("{}. {}", "é".repeat(150), "ü ".repeat(200));
        // (input, expected output or None = check rule only)
        let cases: Vec<(String, String)> = vec![
            ("".into(), "".into()),
            ("  Short text.  ".into(), "Short text.".into()),
            ("x".repeat(n), "x".repeat(n)),
            (
                sentences.clone(),
                format!("{} First sentence ends here. …", "a".repeat(100)),
            ),
            (
                long_words.clone(),
                format!("{}…", "word ".repeat(59).trim_end()),
            ),
            (
                short_sentence_then_words.clone(),
                format!("Hi. {}…", "word ".repeat(59).trim_end()),
            ),
            (
                cjk.clone(),
                format!("{}…", cjk.chars().take(n - 1).collect::<String>()),
            ),
            (accented.clone(), format!("{}. …", "é".repeat(150))),
            (
                format!("{}。{}", "日".repeat(100), "本".repeat(300)),
                format!("{}。 …", "日".repeat(100)),
            ),
        ];
        for (input, want) in cases {
            let got = excerpt(&input);
            assert_eq!(got, want, "input {input:?}");
            assert!(got.chars().count() <= n, "{} chars", got.chars().count());
        }
        // Idempotent: an excerpt is its own excerpt.
        for s in [long_words, sentences, cjk, accented] {
            let once = excerpt(&s);
            assert_eq!(excerpt(&once), once);
        }
    }
}
