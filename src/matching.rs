//! Fuzzy cross-source matching: decides whether two listings whose
//! `dedupe_key`s differ still describe the same happening.
//!
//! Everything here is pure (no I/O, no clock) so it is unit-testable;
//! `repo::upsert_event` applies it to candidates from the database.
//!
//! Two events match when all three gates pass:
//!
//! 1. **Dates** ([`dates_compatible`]): an event is *multi-day* when its
//!    Europe/London end date is after its London start date. Events match on
//!    the same London start date, or when both are multi-day and their London
//!    date ranges overlap. A single-day event never matches an exhibition that
//!    opened on another day.
//! 2. **Venue** ([`venues_compatible`]): the normalised venue keys
//!    (`normalise::normalise_venue_for_key`) are equal, or the word set of one
//!    contains the other's and the smaller has at least two words
//!    ("tate-modern" ⊂ "tate-modern-bankside"); or both events have
//!    coordinates at most [`VENUE_RADIUS_M`] apart.
//! 3. **Title** ([`title_score`]): titles are reduced to tokens by
//!    [`title_tokens`], then match when the token-set Jaccard index is at
//!    least `TITLE_JACCARD_NUM / TITLE_JACCARD_DEN` (word reordering, an extra
//!    word) or the Sørensen–Dice bigram similarity of the joined tokens is at
//!    least [`TITLE_DICE_MIN`] (spelling variants such as "Colour"/"Color").
//!
//! The thresholds are pinned by the table tests below.

use chrono::{DateTime, Utc};
use std::collections::HashSet;

use crate::model::NewEvent;
use crate::normalise::{TITLE_STOP_WORDS, london_date, normalise_venue_for_key, words};

/// Jaccard threshold as a fraction, compared in integers: a pair sitting
/// exactly on 4/5 must match regardless of float rounding.
pub const TITLE_JACCARD_NUM: usize = 4;
pub const TITLE_JACCARD_DEN: usize = 5;
pub const TITLE_DICE_MIN: f64 = 0.9;
pub const VENUE_RADIUS_M: f64 = 150.0;

const EARTH_RADIUS_M: f64 = 6_371_000.0;

/// Words that sources add to titles without changing what the event is.
const TITLE_NOISE_WORDS: &[&str] = &["exhibition", "exhibitions", "ticket", "tickets", "london"];

/// The fields matching looks at.
#[derive(Debug, Clone, Copy)]
pub struct MatchInput<'a> {
    pub title: &'a str,
    pub venue_name: Option<&'a str>,
    pub lat: Option<f64>,
    pub lng: Option<f64>,
    pub starts_at: DateTime<Utc>,
    pub ends_at: Option<DateTime<Utc>>,
}

impl<'a> From<&'a NewEvent> for MatchInput<'a> {
    fn from(e: &'a NewEvent) -> Self {
        MatchInput {
            title: &e.title,
            venue_name: e.venue_name.as_deref(),
            lat: e.lat,
            lng: e.lng,
            starts_at: e.starts_at,
            ends_at: e.ends_at,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TitleScore {
    pub jaccard: f64,
    pub dice: f64,
    pub is_match: bool,
}

/// Title words that identify the event: stop words, noise words, years and
/// the words of any of `venues` are dropped. Venue words are only dropped
/// when something else remains ("Barbican" at the Barbican stays).
pub fn title_tokens(title: &str, venues: &[Option<&str>]) -> Vec<String> {
    let kept: Vec<String> = words(title)
        .into_iter()
        .filter(|w| {
            !TITLE_STOP_WORDS.contains(&w.as_str())
                && !TITLE_NOISE_WORDS.contains(&w.as_str())
                && !is_year(w)
        })
        .collect();
    let venue_words: HashSet<String> = venues.iter().flatten().flat_map(|v| words(v)).collect();
    let without_venue: Vec<String> = kept
        .iter()
        .filter(|w| !venue_words.contains(*w))
        .cloned()
        .collect();
    if without_venue.is_empty() {
        kept
    } else {
        without_venue
    }
}

fn is_year(w: &str) -> bool {
    w.len() == 4 && w.parse::<u16>().is_ok_and(|y| (1900..=2099).contains(&y))
}

/// Title similarity. Symmetric: both titles drop the words of both venues.
pub fn title_score(a: &MatchInput, b: &MatchInput) -> TitleScore {
    let venues = [a.venue_name, b.venue_name];
    let ta = title_tokens(a.title, &venues);
    let tb = title_tokens(b.title, &venues);
    if ta.is_empty() || tb.is_empty() {
        return TitleScore {
            jaccard: 0.0,
            dice: 0.0,
            is_match: false,
        };
    }
    let sa: HashSet<&String> = ta.iter().collect();
    let sb: HashSet<&String> = tb.iter().collect();
    let inter = sa.intersection(&sb).count();
    let union = sa.union(&sb).count();
    let jaccard_ok = TITLE_JACCARD_DEN * inter >= TITLE_JACCARD_NUM * union;
    let dice = strsim::sorensen_dice(&ta.join(" "), &tb.join(" "));
    TitleScore {
        jaccard: inter as f64 / union as f64,
        dice,
        is_match: jaccard_ok || dice >= TITLE_DICE_MIN,
    }
}

pub fn dates_compatible(a: &MatchInput, b: &MatchInput) -> bool {
    let (a_start, a_end) = london_range(a);
    let (b_start, b_end) = london_range(b);
    let multi_day = |start, end| end > start;
    a_start == b_start
        || (multi_day(a_start, a_end)
            && multi_day(b_start, b_end)
            && a_start <= b_end
            && b_start <= a_end)
}

fn london_range(e: &MatchInput) -> (chrono::NaiveDate, chrono::NaiveDate) {
    (
        london_date(e.starts_at),
        london_date(e.ends_at.unwrap_or(e.starts_at)),
    )
}

pub fn venues_compatible(a: &MatchInput, b: &MatchInput) -> bool {
    venue_names_compatible(a.venue_name, b.venue_name) || coordinates_close(a, b)
}

fn venue_names_compatible(a: Option<&str>, b: Option<&str>) -> bool {
    let (ka, kb) = (normalise_venue_for_key(a), normalise_venue_for_key(b));
    if ka == "unknown" || kb == "unknown" {
        return false;
    }
    if ka == kb {
        return true;
    }
    let sa: HashSet<&str> = ka.split('-').collect();
    let sb: HashSet<&str> = kb.split('-').collect();
    let (small, large) = if sa.len() <= sb.len() {
        (sa, sb)
    } else {
        (sb, sa)
    };
    small.len() >= 2 && small.is_subset(&large)
}

fn coordinates_close(a: &MatchInput, b: &MatchInput) -> bool {
    match (a.lat, a.lng, b.lat, b.lng) {
        (Some(lat1), Some(lng1), Some(lat2), Some(lng2)) => {
            haversine_m(lat1, lng1, lat2, lng2) <= VENUE_RADIUS_M
        }
        _ => false,
    }
}

fn haversine_m(lat1: f64, lng1: f64, lat2: f64, lng2: f64) -> f64 {
    let (p1, p2) = (lat1.to_radians(), lat2.to_radians());
    let dp = p2 - p1;
    let dl = (lng2 - lng1).to_radians();
    let h = (dp / 2.0).sin().powi(2) + p1.cos() * p2.cos() * (dl / 2.0).sin().powi(2);
    2.0 * EARTH_RADIUS_M * h.sqrt().asin()
}

/// `Some(score)` iff dates, venue and title all match. Symmetric in (a, b).
pub fn match_score(a: &MatchInput, b: &MatchInput) -> Option<TitleScore> {
    if !dates_compatible(a, b) || !venues_compatible(a, b) {
        return None;
    }
    Some(title_score(a, b)).filter(|s| s.is_match)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(s: &str) -> DateTime<Utc> {
        s.parse().unwrap()
    }

    fn input<'a>(title: &'a str, venue: Option<&'a str>) -> MatchInput<'a> {
        MatchInput {
            title,
            venue_name: venue,
            lat: Some(51.5076),
            lng: Some(-0.0994),
            starts_at: t("2026-10-10T18:00:00Z"),
            ends_at: None,
        }
    }

    fn no_coords(mut m: MatchInput) -> MatchInput {
        m.lat = None;
        m.lng = None;
        m
    }

    #[test]
    fn match_table() {
        #[rustfmt::skip]
        let rows: &[(bool, &str, &str, &str, &str)] = &[
            (true, "Yayoi Kusama: Infinity Rooms – Tate Modern", "Tate Modern", "Yayoi Kusama: Infinity Mirror Rooms", "Tate Modern"),
            (true, "Saturday Talks: Liz Stumpf on LANZA Atelier’s 2026 Serpentine Pavilion", "Serpentine Pavilion", "Saturday talks: Liz Stumpf on Lanza Ateliers 2026 Serpentine Pavilion", "Serpentine Pavilion"),
            (true, "Noor: Light Installations", "Barbican Centre", "Noor: Light Installations Exhibition", "Barbican"),
            (true, "LAIKA presents Wildwood: The Exhibition", "Design Museum", "LAIKA Presents Wildwood", "Design Museum"),
            (true, "Es Devlin: Other Worlds", "Design Museum", "Es Devlin – Other Worlds tickets", "The Design Museum"),
            (true, "Jesús Rafael Soto: Pénétrable BBL Jaune", "Serpentine South Gallery", "Jesus Rafael Soto - Penetrable BBL Jaune", "Serpentine South"),
            (true, "Park Nights 2026: Ebun Sodipo and Rosa-Johan Uddoh", "Serpentine Pavilion", "Park Nights: Ebun Sodipo & Rosa Johan Uddoh", "Serpentine Pavilion"),
            (true, "London Art Fair 2027", "Business Design Centre", "London Art Fair", "Business Design Centre"),
            (true, "An Evening with Fran Lebowitz", "Barbican Centre", "Fran Lebowitz: An Evening With", "Barbican"),
            (true, "Christmas Lecture Preview: The Science of Colour", "Royal Institution", "Christmas Lecture Preview - The Science of Color", "The Royal Institution"),
            (true, "Cecilia Vicuña: Living Threads", "Whitechapel Gallery", "Cecilia Vicuna – Living Threads Exhibition", "Whitechapel Gallery"),
            (true, "The Film London Jarman Award 2026", "Whitechapel Gallery", "Film London Jarman Award", "Whitechapel"),
            (true, "Yayoi Kusama: Infinity Room", "Tate Modern", "Yayoi Kusama - Infinity Rooms", "Tate Modern"),
            (false, "Cecilia Vicuña: Living Threads", "Whitechapel Gallery", "Cecilia Vicuña: Foraging Quipu", "Whitechapel Gallery"),
            (false, "Saturday Talks: Liz Stumpf on LANZA Atelier’s 2026 Serpentine Pavilion", "Serpentine Pavilion", "Saturday Talks: Amar Kanwar in Conversation", "Serpentine Pavilion"),
            (false, "Justin Caguiat: Change Ringing", "Serpentine South Gallery", "Jesús Rafael Soto: Pénétrable BBL Jaune", "Serpentine South Gallery"),
            (false, "An Evening with Fran Lebowitz", "Barbican Centre", "An Evening with David Sedaris", "Barbican Centre"),
            (false, "Park Nights 2026: Ebun Sodipo and Rosa-Johan Uddoh", "Serpentine Pavilion", "Park Nights 2026: Last Yearz Interesting Negro", "Serpentine Pavilion"),
            (false, "Concrete and Clay", "Barbican Centre", "Curator's Tour: Concrete and Clay", "Barbican Centre"),
            (false, "Family Workshop: Clay Modelling", "Barbican Centre", "Adult Workshop: Clay Modelling", "Barbican Centre"),
            (false, "Noor: Light Installations", "Barbican Centre", "Noor", "Barbican Centre"),
            (false, "Two Gharanas, One Musician: Shahbaz Hussain Talk", "Barbican Centre", "Two Gharanas, One Musician: Shahbaz Hussain Concert", "Barbican Centre"),
            (false, "Walters and Cohen", "Barbican Centre", "Walters and Cohen: In Conversation", "Barbican Centre"),
            (false, "Late at Tate: October", "Tate Modern", "Late at Tate: November", "Tate Modern"),
        ];
        for &(expect, ta, va, tb, vb) in rows {
            let (a, b) = (input(ta, Some(va)), input(tb, Some(vb)));
            assert_eq!(
                match_score(&a, &b).is_some(),
                expect,
                "{ta:?} vs {tb:?}: {:?}",
                title_score(&a, &b)
            );
            assert_eq!(match_score(&b, &a).is_some(), expect, "{tb:?} vs {ta:?}");
        }
    }

    #[test]
    fn threshold_boundaries() {
        let v = Some("Tate Modern");
        let kusama = title_score(
            &input("Yayoi Kusama: Infinity Rooms – Tate Modern", v),
            &input("Yayoi Kusama: Infinity Mirror Rooms", v),
        );
        assert_eq!(kusama.jaccard, 4.0 / 5.0);
        assert!(kusama.dice < TITLE_DICE_MIN, "{kusama:?}");
        assert!(kusama.is_match);

        let v = Some("Barbican Centre");
        let gharanas = title_score(
            &input("Two Gharanas, One Musician: Shahbaz Hussain Talk", v),
            &input("Two Gharanas, One Musician: Shahbaz Hussain Concert", v),
        );
        assert_eq!(gharanas.jaccard, 6.0 / 8.0);
        assert!(gharanas.dice < TITLE_DICE_MIN, "{gharanas:?}");
        assert!(!gharanas.is_match);

        let colour = title_score(
            &input(
                "Christmas Lecture Preview: The Science of Colour",
                Some("Royal Institution"),
            ),
            &input(
                "Christmas Lecture Preview - The Science of Color",
                Some("The Royal Institution"),
            ),
        );
        assert_eq!(colour.jaccard, 5.0 / 7.0);
        assert!(colour.dice >= TITLE_DICE_MIN, "{colour:?}");
        assert!(colour.is_match);
    }

    #[test]
    fn date_and_venue_gates() {
        let title = "Cecilia Vicuña: Living Threads";
        let at = |starts: &str, ends: Option<&str>| MatchInput {
            starts_at: t(starts),
            ends_at: ends.map(t),
            ..input(title, Some("Whitechapel Gallery"))
        };
        let matches = |a: &MatchInput, b: &MatchInput| {
            let (ab, ba) = (match_score(a, b).is_some(), match_score(b, a).is_some());
            assert_eq!(ab, ba, "asymmetric");
            ab
        };

        // 23:30 BST is still 1 July in London.
        assert!(matches(
            &at("2026-07-01T22:30:00Z", None),
            &at("2026-07-01T18:00:00Z", None)
        ));
        // 00:30 BST is 2 July.
        assert!(!matches(
            &at("2026-07-01T23:30:00Z", None),
            &at("2026-07-01T18:00:00Z", None)
        ));
        let show = at("2026-10-01T10:00:00Z", Some("2027-02-01T18:00:00Z"));
        assert!(matches(
            &show,
            &at("2026-10-02T10:00:00Z", Some("2027-01-31T18:00:00Z"))
        ));
        assert!(!matches(
            &show,
            &at("2027-03-01T10:00:00Z", Some("2027-05-01T18:00:00Z"))
        ));
        assert!(!matches(&show, &at("2026-11-15T18:00:00Z", None)));

        let venue = |venue: Option<&'static str>, lat: Option<f64>, lng: Option<f64>| MatchInput {
            lat,
            lng,
            ..input(title, venue)
        };
        assert!(!matches(
            &venue(
                Some("Serpentine North Gallery"),
                Some(51.5055),
                Some(-0.1718)
            ),
            &venue(
                Some("Serpentine South Gallery"),
                Some(51.5045),
                Some(-0.1751)
            ),
        ));
        assert!(matches(
            &venue(None, Some(51.5201), Some(-0.0955)),
            &venue(None, Some(51.5202), Some(-0.0938)),
        ));
        assert!(!matches(&venue(None, None, None), &venue(None, None, None)));
        assert!(matches(
            &no_coords(input(title, Some("Tate Modern"))),
            &no_coords(input(title, Some("Tate Modern Bankside"))),
        ));
        assert!(!matches(
            &no_coords(input(title, Some("Design Museum"))),
            &no_coords(input(title, Some("Business Design Centre"))),
        ));
    }

    #[test]
    fn title_tokens_strip_noise() {
        assert_eq!(
            title_tokens(
                "The Film London Jarman Award 2026 Tickets",
                &[Some("Whitechapel Gallery")]
            ),
            ["film", "jarman", "award"]
        );
        assert_eq!(
            title_tokens("Barbican Centre", &[Some("Barbican Centre"), None]),
            ["barbican", "centre"]
        );
    }
}
