//! Music subtags (issue #209, option (b): the 'arty' end of music). Pure and
//! deterministic, no AI. An event in the `music` category gets the subtags
//! of [`MUSIC_TAGS`] whose keywords appear, on word boundaries, in its
//! source tags or its title. A scraper can set a subtag directly by putting
//! the tag itself (`jazz`, `sound art`…) in the event's `tags`; the keyword
//! lists catch the rest. Events in other categories never get one.
//!
//! `repo::sync_music_tags` applies it to every event after each ingest run;
//! the listing filters on `music=` (`crate::listing`).

/// The music subtags, in the order the filter shows them.
pub const MUSIC_TAGS: &[&str] = &[
    "classical",
    "contemporary",
    "experimental",
    "jazz",
    "sound_art",
];

/// What pages call a music subtag.
pub fn label(tag: &str) -> &str {
    match tag {
        "sound_art" => "sound art",
        other => other,
    }
}

/// Keywords per subtag (lowercase, hyphens as spaces), matched on word
/// boundaries. Kept to words that name the genre, not moods.
const KEYWORDS: &[(&str, &[&str])] = &[
    (
        "classical",
        &[
            "classical",
            "orchestra",
            "orchestral",
            "symphony",
            "symphonic",
            "philharmonic",
            "sinfonia",
            "concerto",
            "string quartet",
            "piano trio",
            "chamber music",
            "chamber orchestra",
            "recital",
            "baroque",
            "opera",
            "choral",
            "choir",
            "oratorio",
            "requiem",
            "sonata",
            "organ recital",
            "early music",
        ],
    ),
    (
        "contemporary",
        &[
            "contemporary",
            "contemporary music",
            "new music",
            "contemporary classical",
            "modern composition",
            "world premiere",
            "uk premiere",
            "london premiere",
        ],
    ),
    (
        "experimental",
        &[
            "experimental",
            "avant garde",
            "noise",
            "drone",
            "electroacoustic",
            "musique concrete",
            "free improvisation",
            "improvised music",
            "modular synth",
        ],
    ),
    (
        "jazz",
        &["jazz", "bebop", "big band", "swing", "improv", "free jazz"],
    ),
    (
        "sound_art",
        &[
            "sound art",
            "sound_art",
            "sound installation",
            "sound walk",
            "soundwalk",
            "sonic art",
            "field recording",
            "field recordings",
            "deep listening",
            "listening session",
        ],
    ),
];

/// Lowercase `s` with every run of non-alphanumerics (except `_`) as one
/// space, padded, so `contains(" word ")` is a word-boundary match.
fn padded(s: &str) -> String {
    let mut out = String::from(" ");
    let mut space = true;
    for c in s.chars().flat_map(char::to_lowercase) {
        let c = match c {
            'é' | 'è' | 'ê' => 'e',
            c => c,
        };
        if c.is_alphanumeric() || c == '_' {
            out.push(c);
            space = false;
        } else if !space {
            out.push(' ');
            space = true;
        }
    }
    if !space {
        out.push(' ');
    }
    out
}

/// The music subtags of an event (in [`MUSIC_TAGS`] order), from its
/// category, source tags and title. Empty unless `category` is `music`.
pub fn tags_for(category: &str, source_tags: &[String], title: &str) -> Vec<&'static str> {
    if category != "music" {
        return Vec::new();
    }
    let hay: Vec<String> = source_tags
        .iter()
        .map(|t| padded(t))
        .chain(std::iter::once(padded(title)))
        .collect();
    MUSIC_TAGS
        .iter()
        .copied()
        .filter(|tag| {
            let words = KEYWORDS
                .iter()
                .find(|(t, _)| t == tag)
                .map_or(&[][..], |(_, w)| *w);
            words.iter().any(|w| {
                let needle = format!(" {w} ");
                hay.iter().any(|h| h.contains(&needle))
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tags(category: &str, source_tags: &[&str], title: &str) -> Vec<&'static str> {
        let st: Vec<String> = source_tags.iter().map(|s| s.to_string()).collect();
        tags_for(category, &st, title)
    }

    #[test]
    fn only_music_events_get_subtags() {
        assert!(tags("talk", &["jazz"], "Jazz and the city").is_empty());
        assert!(tags("exhibition", &[], "Sound art now").is_empty());
        assert_eq!(tags("music", &["jazz"], "Julian Siegel Quartet"), ["jazz"]);
    }

    #[test]
    fn keywords_match_on_word_boundaries() {
        assert_eq!(
            tags("music", &[], "Bruch and Beethoven: Violin Concerto No. 1"),
            ["classical"]
        );
        // "noise" inside another word, "swing" inside "swinging"… don't match.
        assert!(tags("music", &[], "Noisette plays the Swingingest tunes").is_empty());
        assert_eq!(
            tags("music", &["Sound Art", "Avant-Garde"], "Hainbach live"),
            ["experimental", "sound_art"]
        );
        assert_eq!(tags("music", &["sound_art"], "x"), ["sound_art"]);
    }

    #[test]
    fn several_subtags_keep_the_vocabulary_order() {
        assert_eq!(
            tags(
                "music",
                &["contemporary-music"],
                "World premiere for string quartet and free improvisation"
            ),
            ["classical", "contemporary", "experimental"]
        );
    }

    #[test]
    fn every_subtag_has_keywords_and_a_label() {
        for t in MUSIC_TAGS {
            assert!(KEYWORDS.iter().any(|(k, w)| k == t && !w.is_empty()), "{t}");
            assert!(!label(t).contains('_'), "{t}");
        }
        assert_eq!(KEYWORDS.len(), MUSIC_TAGS.len());
    }
}
