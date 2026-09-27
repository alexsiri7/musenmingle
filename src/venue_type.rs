//! Venue type (issue #78): what kind of place an event is at, one of
//! [`VENUE_TYPES`]. Deterministic, no AI. For an event, in order:
//!
//! 1. a manual override: `events.venues.venue_type` for the venue, matched
//!    on the normalised venue name (`normalise::normalise_venue_for_key`,
//!    like the venue coordinates);
//! 2. the default of the event's sources ([`source_default`]: museum
//!    sources are museums, Artlogic galleries commercial galleries, Luma
//!    calendars community); when an event's sources disagree, the first
//!    by source key wins;
//! 3. a reviewed keyword list on the venue name ([`from_keywords`]);
//! 4. otherwise `other`.
//!
//! `repo::sync_venue_types` applies it to every event after each ingest
//! run; the listing filters on `venue_type=` (`crate::listing`).

use std::collections::HashMap;

use crate::normalise::normalise_venue_for_key;

/// The venue types, in the order the filter shows them.
pub const VENUE_TYPES: &[&str] = &[
    "museum",
    "commercial_gallery",
    "artist_run",
    "community",
    "other",
];

/// What the filter calls a venue type.
pub fn label(venue_type: &str) -> &str {
    match venue_type {
        "museum" => "museum or public gallery",
        "commercial_gallery" => "commercial gallery",
        "artist_run" => "artist-run space",
        "community" => "community venue",
        "other" => "other venue",
        other => other,
    }
}

/// Sources whose every event is at one kind of venue. Sources not listed
/// (Ticketmaster, festivals, bookshops, Lux…) have no default.
const SOURCE_DEFAULTS: &[(&str, &str)] = &[
    // Museums and public (non-commercial) galleries and arts centres.
    ("barbican", "museum"),
    ("camden-art-centre", "museum"),
    ("chisenhale-gallery", "museum"),
    ("courtauld", "museum"),
    ("design-museum", "museum"),
    ("foundling-museum", "museum"),
    ("garden-museum", "museum"),
    ("goldsmiths-cca", "museum"),
    ("handel-hendrix", "museum"),
    ("headstone-manor", "museum"),
    ("hunterian-museum", "museum"),
    ("ibraaz", "museum"),
    ("ica", "museum"),
    ("mall-galleries", "museum"),
    ("old-royal-naval-college", "museum"),
    ("photographers-gallery", "museum"),
    ("royal-museums-greenwich", "museum"),
    ("serpentine-galleries", "museum"),
    ("soane-museum", "museum"),
    ("somerset-house", "museum"),
    ("south-london-gallery", "museum"),
    ("tec-cinema-museum", "museum"),
    ("tec-freud-museum", "museum"),
    ("tec-slbi", "museum"),
    ("the-showroom", "museum"),
    ("two-temple-place", "museum"),
    ("vam", "museum"),
    ("wellcome-collection", "museum"),
    ("whitechapel-gallery", "museum"),
    ("william-morris-gallery", "museum"),
    // Commercial galleries (the Artlogic ones are matched by prefix).
    ("lisson-gallery", "commercial_gallery"),
    ("october-gallery", "commercial_gallery"),
    // Artist-run and artist-led spaces.
    ("horse-hospital", "artist_run"),
    ("tec-bow-arts", "artist_run"),
    // Community venues and groups (the Luma calendars by prefix).
    ("conway-hall", "community"),
    ("four-corners", "community"),
    ("tec-chats-palace", "community"),
    ("william-morris-society", "community"),
];

/// The default venue type of a source's events, if it has one.
pub fn source_default(source_key: &str) -> Option<&'static str> {
    if source_key.starts_with("artlogic-") {
        return Some("commercial_gallery");
    }
    if source_key.starts_with("luma-") {
        return Some("community");
    }
    SOURCE_DEFAULTS
        .iter()
        .find(|(k, _)| *k == source_key)
        .map(|(_, t)| *t)
}

/// Words in a venue name that say what it is (matched on the lowercased
/// name, as whole words or phrases), checked in this order.
const KEYWORDS: &[(&str, &str)] = &[
    ("project space", "artist_run"),
    ("artist-run", "artist_run"),
    ("artist run", "artist_run"),
    ("artist-led", "artist_run"),
    ("studios", "artist_run"),
    ("museum", "museum"),
    ("community", "community"),
    ("library", "community"),
    ("arts centre", "community"),
];

/// The venue type a venue's name alone says, if any.
pub fn from_keywords(venue_name: &str) -> Option<&'static str> {
    let words: String = venue_name
        .to_lowercase()
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '-' {
                c
            } else {
                ' '
            }
        })
        .collect();
    let padded = format!(
        " {} ",
        words.split_whitespace().collect::<Vec<_>>().join(" ")
    );
    KEYWORDS
        .iter()
        .find(|(k, _)| padded.contains(&format!(" {k} ")))
        .map(|(_, t)| *t)
}

/// Manual overrides, keyed by normalised venue name.
#[derive(Debug, Clone, Default)]
pub struct Overrides(HashMap<String, &'static str>);

impl Overrides {
    /// From `(venue name, venue type)` rows; unknown types are ignored.
    pub fn new<'a>(rows: impl IntoIterator<Item = (&'a str, &'a str)>) -> Self {
        let mut map = HashMap::new();
        for (name, t) in rows {
            let key = normalise_venue_for_key(Some(name));
            if let (false, Some(t)) = (key == "unknown", VENUE_TYPES.iter().find(|v| **v == t)) {
                map.entry(key).or_insert(*t);
            }
        }
        Overrides(map)
    }

    fn get(&self, venue_name: &str) -> Option<&'static str> {
        let key = normalise_venue_for_key(Some(venue_name));
        self.0.get(&key).copied()
    }
}

/// An event's venue type from its venue name and its sources' keys.
pub fn classify(
    venue_name: Option<&str>,
    source_keys: &[&str],
    overrides: &Overrides,
) -> &'static str {
    let venue = venue_name.map(str::trim).filter(|v| !v.is_empty());
    if let Some(t) = venue.and_then(|v| overrides.get(v)) {
        return t;
    }
    let mut keys = source_keys.to_vec();
    keys.sort_unstable();
    if let Some(t) = keys.iter().find_map(|k| source_default(k)) {
        return t;
    }
    venue.and_then(from_keywords).unwrap_or("other")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn none() -> Overrides {
        Overrides::default()
    }

    #[test]
    fn source_defaults() {
        assert_eq!(
            classify(Some("V&A South Kensington"), &["vam"], &none()),
            "museum"
        );
        assert_eq!(
            classify(Some("Victoria Miro"), &["artlogic-victoria-miro"], &none()),
            "commercial_gallery"
        );
        assert_eq!(
            classify(Some("Mason & Fifth"), &["luma-mason-and-fifth"], &none()),
            "community"
        );
        assert_eq!(classify(None, &["luma-for-writers"], &none()), "community");
        assert_eq!(
            classify(Some("Conway Hall"), &["conway-hall"], &none()),
            "community"
        );
        assert_eq!(
            classify(Some("Lakeside Centre"), &["tec-bow-arts"], &none()),
            "artist_run"
        );
        // Every default is in the vocabulary.
        for (_, t) in SOURCE_DEFAULTS {
            assert!(VENUE_TYPES.contains(t), "{t}");
        }
    }

    #[test]
    fn source_default_beats_keywords() {
        // "Studios" would say artist-run, but the source is a commercial gallery.
        assert_eq!(
            classify(Some("Flowers Studios"), &["artlogic-flowers"], &none()),
            "commercial_gallery"
        );
    }

    #[test]
    fn keywords_for_sources_without_a_default() {
        let tm = &["ticketmaster"][..];
        assert_eq!(
            classify(Some("Acme Project Space"), tm, &none()),
            "artist_run"
        );
        assert_eq!(
            classify(Some("Bow Arts Studios"), tm, &none()),
            "artist_run"
        );
        assert_eq!(
            classify(Some("An artist-run space"), tm, &none()),
            "artist_run"
        );
        assert_eq!(
            classify(Some("Natural History Museum"), tm, &none()),
            "museum"
        );
        assert_eq!(
            classify(Some("Chats Palace Arts Centre"), tm, &none()),
            "community"
        );
        assert_eq!(
            classify(Some("Bethnal Green Library"), tm, &none()),
            "community"
        );
        assert_eq!(
            classify(Some("Neon at Battersea Power Station"), tm, &none()),
            "other"
        );
        assert_eq!(classify(Some("Playhouse Theatre"), tm, &none()), "other");
        // Whole words only.
        assert_eq!(classify(Some("Museumsquartier Bar"), tm, &none()), "other");
        assert_eq!(classify(Some("Studiostore"), tm, &none()), "other");
        assert_eq!(classify(None, tm, &none()), "other");
        assert_eq!(classify(Some("  "), &[], &none()), "other");
    }

    #[test]
    fn overrides_win_and_match_the_normalised_name() {
        let o = Overrides::new([
            ("Royal Academy of Arts", "museum"),
            ("Neon at Battersea Power Station", "other"),
            ("Somewhere", "not-a-type"),
        ]);
        // Beats the source default (Ticketmaster has none) and keywords.
        assert_eq!(
            classify(Some("The Royal Academy of Arts"), &["ticketmaster"], &o),
            "museum"
        );
        assert_eq!(
            classify(Some("royal academy of arts"), &["luma-for-writers"], &o),
            "museum"
        );
        // Unknown types in the seed are ignored.
        assert_eq!(classify(Some("Somewhere"), &["ticketmaster"], &o), "other");
        assert_eq!(classify(Some("Somewhere"), &["vam"], &o), "museum");
    }

    #[test]
    fn disagreeing_sources_pick_the_first_by_key() {
        assert_eq!(
            classify(Some("X"), &["vam", "artlogic-x"], &none()),
            "commercial_gallery"
        );
        assert_eq!(
            classify(Some("X"), &["ticketmaster", "barbican"], &none()),
            "museum"
        );
    }

    #[test]
    fn labels() {
        for t in VENUE_TYPES {
            assert_ne!(label(t), *t, "{t} needs a label");
        }
    }
}
