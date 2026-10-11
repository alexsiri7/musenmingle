//! Memberships (spec change #313, issue #315): the National Art Pass and
//! the like, which get their holders into some venues free or for less.
//! Pure and deterministic, no AI. An event matches a membership when its
//! venue is on the membership's list (`events.venue_memberships`, kept by
//! hand in migrations, never scraped from the provider), carrying that
//! venue's offer; or when its source tags or stored excerpt name the
//! membership, on word boundaries, with no offer.
//!
//! `repo::sync_memberships` applies it to every event after each ingest run
//! (`events.event_memberships`).

use std::sync::LazyLock;

use crate::normalise::padded_words;

/// A membership we know about.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Membership {
    /// Stable key, as stored and as the `membership=` filter value.
    pub key: &'static str,
    /// What pages call it.
    pub name: &'static str,
    /// The provider's page about it.
    pub url: &'static str,
    /// How listings name it (lowercase, hyphens as spaces), matched on word
    /// boundaries.
    names: &'static [&'static str],
}

/// The memberships, in the order the filter shows them. A new one also
/// needs a migration widening the `membership` CHECKs.
pub const MEMBERSHIPS: &[Membership] = &[Membership {
    key: "art_pass",
    name: "National Art Pass",
    url: "https://www.artfund.org/national-art-pass",
    names: &["national art pass", "art pass"],
}];

/// The keys of [`MEMBERSHIPS`], in order: the `membership=` vocabulary.
pub static KEYS: LazyLock<Vec<&'static str>> =
    LazyLock::new(|| MEMBERSHIPS.iter().map(|m| m.key).collect());

/// The membership with this key.
pub fn get(key: &str) -> Option<&'static Membership> {
    MEMBERSHIPS.iter().find(|m| m.key == key)
}

/// A membership's display name; unknown keys come back as given.
pub fn label(key: &str) -> &str {
    get(key).map_or(key, |m| m.name)
}

/// An event's match with a membership.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Match {
    pub membership: &'static str,
    /// The venue's offer; `None` when only the event's text names it.
    pub offer: Option<String>,
}

/// The memberships an event matches (in [`MEMBERSHIPS`] order), from its
/// venue's `(membership, offer)` entries, its source tags and its stored
/// excerpt. Entries for unknown memberships are ignored.
pub fn matches(
    venue_offers: &[(String, String)],
    source_tags: &[String],
    excerpt: Option<&str>,
) -> Vec<Match> {
    let hay: Vec<String> = source_tags
        .iter()
        .map(String::as_str)
        .chain(excerpt)
        .map(padded_words)
        .collect();
    MEMBERSHIPS
        .iter()
        .filter_map(|m| {
            if let Some((_, offer)) = venue_offers.iter().find(|(k, _)| k == m.key) {
                return Some(Match {
                    membership: m.key,
                    offer: Some(offer.clone()),
                });
            }
            let named = m.names.iter().any(|n| {
                let needle = format!(" {n} ");
                hay.iter().any(|h| h.contains(&needle))
            });
            named.then_some(Match {
                membership: m.key,
                offer: None,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(source_tags: &[&str], excerpt: Option<&str>) -> Vec<Match> {
        let st: Vec<String> = source_tags.iter().map(|s| s.to_string()).collect();
        matches(&[], &st, excerpt)
    }

    fn art_pass(offer: Option<&str>) -> Vec<Match> {
        vec![Match {
            membership: "art_pass",
            offer: offer.map(str::to_string),
        }]
    }

    #[test]
    fn the_national_art_pass_is_known() {
        let m = get("art_pass").unwrap();
        assert_eq!(m.name, "National Art Pass");
        assert_eq!(m.url, "https://www.artfund.org/national-art-pass");
        assert!(get("unknown").is_none());
        assert_eq!(*KEYS, ["art_pass"]);
        assert_eq!(label("art_pass"), "National Art Pass");
    }

    #[test]
    fn a_participating_venue_gives_its_offer() {
        let venue = [("art_pass".to_string(), "50% off exhibitions".to_string())];
        assert_eq!(
            matches(&venue, &[], Some("Free for National Art Pass holders.")),
            art_pass(Some("50% off exhibitions"))
        );
        assert_eq!(
            matches(&venue, &[], None),
            art_pass(Some("50% off exhibitions"))
        );
    }

    #[test]
    fn the_events_text_names_it_on_word_boundaries() {
        assert_eq!(
            text(&[], Some("Free for National Art Pass holders.")),
            art_pass(None)
        );
        assert_eq!(text(&["Art-Pass"], None), art_pass(None));
        assert_eq!(text(&[], Some("½ price with an Art Pass")), art_pass(None));
        assert!(text(&["craft"], Some("A smart passage through prints.")).is_empty());
        assert!(text(&[], Some("Art passes through many hands.")).is_empty());
        assert!(text(&[], None).is_empty());
    }

    #[test]
    fn unknown_memberships_are_ignored() {
        let venue = [("museum_pass".to_string(), "Free entry".to_string())];
        assert!(matches(&venue, &[], None).is_empty());
    }

    #[test]
    fn every_membership_has_names_and_a_link() {
        for m in MEMBERSHIPS {
            assert!(!m.names.is_empty(), "{}", m.key);
            assert!(m.url.starts_with("https://"), "{}", m.key);
            for n in m.names {
                assert_eq!(padded_words(n), format!(" {n} "), "{}", m.key);
            }
        }
    }
}
