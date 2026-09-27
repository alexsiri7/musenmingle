//! Venues as first-class objects (issue #204), the pure parts: which venue
//! a listing's venue name belongs to ([`Resolver`]: the normalised name
//! (`normalise::normalise_venue_for_key`) through `events.venue_aliases`),
//! URL slugs, UK postcodes in addresses, and picking a venue's
//! representative name/address/point from its events. `repo::sync_venues`
//! applies them after each ingest run.

use std::collections::HashMap;

use crate::normalise::normalise_venue_for_key;

/// Where an event's venue name leads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Resolved {
    /// The venue with this normalised name.
    Venue(String),
    /// No venue: no name, a placeholder, or an area (an alias with no venue).
    None,
}

/// Resolves venue names to venue keys through the aliases.
#[derive(Debug, Default, Clone)]
pub struct Resolver {
    aliases: HashMap<String, Option<String>>,
}

impl Resolver {
    /// `aliases`: `(name, canonical venue name or None for "not a venue")`.
    pub fn new<'a>(aliases: impl IntoIterator<Item = (&'a str, Option<&'a str>)>) -> Self {
        let aliases = aliases
            .into_iter()
            .map(|(name, to)| {
                (
                    normalise_venue_for_key(Some(name)),
                    to.map(|t| normalise_venue_for_key(Some(t))),
                )
            })
            .collect();
        Self { aliases }
    }

    pub fn resolve(&self, venue_name: Option<&str>) -> Resolved {
        let key = normalise_venue_for_key(venue_name);
        if key == "unknown" {
            return Resolved::None;
        }
        match self.aliases.get(&key) {
            Some(Some(to)) if to != "unknown" => Resolved::Venue(to.clone()),
            Some(_) => Resolved::None,
            None => Resolved::Venue(key),
        }
    }
}

/// URL slug of a venue name: folded to ASCII, lower case, `&` as "and",
/// apostrophes dropped, other runs of non-alphanumerics as one `-`
/// (`The Photographers' Gallery` → `the-photographers-gallery`). Never
/// empty (`venue`).
pub fn slugify(name: &str) -> String {
    let folded = crate::search::fold(name).replace('&', " and ");
    let mut out = String::with_capacity(folded.len());
    let mut dash = false;
    for c in folded.chars() {
        if c.is_ascii_alphanumeric() {
            if dash && !out.is_empty() {
                out.push('-');
            }
            dash = false;
            out.push(c);
        } else if !matches!(c, '\'' | '’' | '‘') {
            dash = true;
        }
    }
    if out.is_empty() { "venue".into() } else { out }
}

/// `base`, or `base-2`, `base-3`… whichever `taken` doesn't hold.
pub fn unique_slug(base: &str, taken: impl Fn(&str) -> bool) -> String {
    if !taken(base) {
        return base.to_string();
    }
    (2..)
        .map(|n| format!("{base}-{n}"))
        .find(|s| !taken(s))
        .unwrap_or_default()
}

/// The last full UK postcode in `address`, formatted `SE5 8UH`. Only the
/// spaced form counts (outward code: 1–2 letters, a digit, then an optional
/// letter or digit; inward code: a digit and two letters); a bare district
/// such as `EC1A` is not a postcode.
pub fn postcode(address: &str) -> Option<String> {
    let words: Vec<String> = address
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|w| !w.is_empty())
        .map(str::to_ascii_uppercase)
        .collect();
    words
        .windows(2)
        .rev()
        .find(|w| is_outward(&w[0]) && is_inward(&w[1]))
        .map(|w| format!("{} {}", w[0], w[1]))
}

fn is_outward(w: &str) -> bool {
    let b = w.as_bytes();
    let letters = b.iter().take_while(|c| c.is_ascii_alphabetic()).count();
    if !(1..=2).contains(&letters) || b.len() <= letters {
        return false;
    }
    let rest = &b[letters..];
    rest[0].is_ascii_digit()
        && match rest.len() {
            1 => true,
            2 => rest[1].is_ascii_alphanumeric(),
            _ => false,
        }
}

fn is_inward(w: &str) -> bool {
    let b = w.as_bytes();
    b.len() == 3 && b[0].is_ascii_digit() && b[1..].iter().all(u8::is_ascii_alphabetic)
}

/// The most common value (ties: the smallest), ignoring blanks.
pub fn most_common<'a>(values: impl IntoIterator<Item = &'a str>) -> Option<&'a str> {
    let mut counts: HashMap<&str, usize> = HashMap::new();
    for v in values {
        let v = v.trim();
        if !v.is_empty() {
            *counts.entry(v).or_default() += 1;
        }
    }
    counts
        .into_iter()
        .max_by(|(a, n), (b, m)| n.cmp(m).then_with(|| b.cmp(a)))
        .map(|(v, _)| v)
}

/// The most common point among `points` (compared at ~1 m, 5 decimals;
/// ties: the smallest), so one odd listing doesn't move a venue.
pub fn most_common_point(points: impl IntoIterator<Item = (f64, f64)>) -> Option<(f64, f64)> {
    let mut counts: HashMap<(i64, i64), (usize, (f64, f64))> = HashMap::new();
    for (lat, lng) in points {
        let k = ((lat * 1e5).round() as i64, (lng * 1e5).round() as i64);
        counts.entry(k).or_insert((0, (lat, lng))).0 += 1;
    }
    counts
        .into_iter()
        .max_by(|(ka, (n, _)), (kb, (m, _))| n.cmp(m).then_with(|| kb.cmp(ka)))
        .map(|(_, (_, p))| p)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_through_aliases() {
        let r = Resolver::new([
            ("Institute of Contemporary Arts", Some("ICA")),
            ("Clerkenwell", None),
        ]);
        assert_eq!(r.resolve(Some("ICA")), Resolved::Venue("ica".into()));
        assert_eq!(
            r.resolve(Some("The Institute of Contemporary Arts")),
            Resolved::Venue("ica".into())
        );
        assert_eq!(
            r.resolve(Some("Barbican Centre")),
            Resolved::Venue("barbican".into())
        );
        assert_eq!(r.resolve(Some("Clerkenwell")), Resolved::None);
        assert_eq!(r.resolve(Some("-")), Resolved::None);
        assert_eq!(r.resolve(Some("  ")), Resolved::None);
        assert_eq!(r.resolve(None), Resolved::None);
    }

    #[test]
    fn slugs() {
        assert_eq!(
            slugify("The Photographers' Gallery"),
            "the-photographers-gallery"
        );
        assert_eq!(slugify("V&A South Kensington"), "v-and-a-south-kensington");
        assert_eq!(slugify("Sir John Soane’s Museum"), "sir-john-soanes-museum");
        assert_eq!(
            slugify("Thaddaeus Ropac, Ely House"),
            "thaddaeus-ropac-ely-house"
        );
        assert_eq!(slugify("Galería Élan"), "galeria-elan");
        assert_eq!(slugify("—"), "venue");
        let taken = ["ica", "ica-2"];
        assert_eq!(unique_slug("ica", |s| taken.contains(&s)), "ica-3");
        assert_eq!(unique_slug("tate", |s| taken.contains(&s)), "tate");
    }

    #[test]
    fn postcodes() {
        assert_eq!(
            postcode("65-67 Peckham Road, London SE5 8UH").as_deref(),
            Some("SE5 8UH")
        );
        assert_eq!(
            postcode("39a Canonbury Square, London N1 2AN").as_deref(),
            Some("N1 2AN")
        );
        assert_eq!(
            postcode("1st Floor, 47 Farringdon Road, London EC1M 3JB").as_deref(),
            Some("EC1M 3JB")
        );
        assert_eq!(
            postcode("20 Maresfield Gardens, London, nw3 5sx").as_deref(),
            Some("NW3 5SX")
        );
        assert_eq!(
            postcode("222 Brixton Rd, London SW9 6AH, UK").as_deref(),
            Some("SW9 6AH")
        );
        assert_eq!(postcode("London, London, EC1A, United Kingdom"), None);
        assert_eq!(postcode("11 Bury Street, St James's, London"), None);
        assert_eq!(
            postcode("The Mall, London SW1Y 5AH").as_deref(),
            Some("SW1Y 5AH")
        );
    }

    #[test]
    fn most_common_values() {
        assert_eq!(most_common(["b", "a", "b", " "]), Some("b"));
        assert_eq!(most_common(["b", "a"]), Some("a"));
        assert_eq!(most_common([""]), None);
        assert_eq!(
            most_common_point([(51.5, -0.1), (51.6, -0.2), (51.500001, -0.1)]),
            Some((51.5, -0.1))
        );
        assert_eq!(most_common_point([]), None);
    }
}
