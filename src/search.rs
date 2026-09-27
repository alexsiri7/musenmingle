//! Full-text search (`q=`): query parsing, accent folding and typo
//! correction (pure), plus the one lookup the correction needs.
//!
//! Matching and ranking are SQL (`repo::LISTING_FILTER`, `repo::SEARCH_TSQUERY`)
//! over the generated `events.events.search` tsvector (migration
//! `20260927960001_event_search.sql`). A query matches an event when:
//!
//! - `websearch_to_tsquery` of the folded text matches (English stemming,
//!   quotes, `or`, `-word`); or
//! - the prefix query built here matches: the query's words reduced to
//!   `[a-z0-9]+`, ANDed, the last one as a prefix (`whitech` finds
//!   Whitechapel); or the same with typos corrected (below); or
//! - the query is only stop words (`the`), and the folded title or venue
//!   name contains it.
//!
//! Typo tolerance is done here rather than with `pg_trgm`, because
//! migrations may not create extensions: a query word that matches no
//! upcoming event is replaced by the nearest word (Damerau–Levenshtein, at
//! most 1 edit for 4–5 letters, 2 for longer words) from the titles and
//! venue names of upcoming events, and the corrected query is ORed in.

use std::collections::HashSet;

use sqlx::PgPool;

/// Longest accepted `q`, in characters.
pub const MAX_QUERY_CHARS: usize = 200;
/// Words of a query used for prefix matching and correction.
pub const MAX_TOKENS: usize = 12;
/// Shortest word that is ever corrected.
const MIN_CORRECTED_LEN: usize = 4;

/// Characters folded to ASCII, one for one; the same table as
/// `events.search_fold` (tests check the two agree).
const FOLD_FROM: &str = "áÁàÀâÂäÄãÃåÅāĀăĂąĄçÇćĆĉĈčČďĎđĐéÉèÈêÊëËēĒĕĔėĖęĘěĚĝĜğĞġĠģĢĥĤħĦíÍìÌîÎïÏĩĨīĪĭĬįĮıIĵĴķĶĺĹļĻľĽŀĿłŁñÑńŃņŅňŇóÓòÒôÔöÖõÕōŌŏŎőŐŕŔŗŖřŘśŚŝŜşŞšŠșȘţŢťŤțȚúÚùÙûÛüÜũŨūŪŭŬůŮűŰųŲŵŴýÝÿŸŷŶźŹżŻžŽʼ’‘";
const FOLD_TO: &str = "aaaaaaaaaaaaaaaaaaccccccccddddeeeeeeeeeeeeeeeeeegggggggghhhhiiiiiiiiiiiiiiiiiijjkkllllllllllnnnnnnnnoooooooooooooooorrrrrrssssssssssttttttuuuuuuuuuuuuuuuuuuuuwwyyyyyyzzzzzz'''";
/// Characters folded to two letters.
const FOLD_PAIRS: [(char, &str); 12] = [
    ('ß', "ss"),
    ('ẞ', "ss"),
    ('æ', "ae"),
    ('Æ', "ae"),
    ('œ', "oe"),
    ('Œ', "oe"),
    ('ø', "o"),
    ('Ø', "o"),
    ('þ', "th"),
    ('Þ', "th"),
    ('ð', "d"),
    ('Ð', "d"),
];

/// Lower-case `s` and fold accented Latin letters to ASCII (`Sámi` →
/// `sami`), like `events.search_fold`.
pub fn fold(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        if let Some((_, pair)) = FOLD_PAIRS.iter().find(|(p, _)| *p == c) {
            out.push_str(pair);
        } else if let Some(i) = FOLD_FROM.chars().position(|f| f == c) {
            out.push(FOLD_TO.chars().nth(i).unwrap_or(c));
        } else {
            out.extend(c.to_lowercase());
        }
    }
    out
}

/// The folded `[a-z0-9]+` words of `q` (at most [`MAX_TOKENS`]).
pub fn tokens(q: &str) -> Vec<String> {
    fold(q)
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|w| !w.is_empty())
        .take(MAX_TOKENS)
        .map(str::to_string)
        .collect()
}

/// Whether `q` uses web-search syntax (quotes, `or`, `-word`), in which case
/// only `websearch_to_tsquery` interprets it.
fn has_operators(q: &str) -> bool {
    q.contains('"')
        || q.split_whitespace()
            .any(|w| w.eq_ignore_ascii_case("or") || (w.starts_with('-') && w.len() > 1))
}

/// `to_tsquery` text ANDing `words`, the last one as a prefix. Safe to
/// pass to `to_tsquery`: every word is `[a-z0-9]+`.
pub fn prefix_tsquery(words: &[String]) -> Option<String> {
    let last = words.len().checked_sub(1)?;
    Some(
        words
            .iter()
            .enumerate()
            .map(|(i, w)| {
                debug_assert!(w.bytes().all(|b| b.is_ascii_alphanumeric()));
                if i == last {
                    format!("{w}:*")
                } else {
                    w.clone()
                }
            })
            .collect::<Vec<_>>()
            .join(" & "),
    )
}

/// A search request (`q=`), as parsed. `alternatives` is the extra
/// `to_tsquery` text ORed with `websearch_to_tsquery(text)`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Search {
    /// The query as typed (trimmed).
    pub text: String,
    /// The prefix query, and the corrected one once [`resolve`] ran.
    pub alternatives: Option<String>,
    /// `text` with typos corrected, when a word was replaced.
    pub corrected: Option<String>,
}

impl Search {
    /// Parse `q`: `Ok(None)` when blank; too long is an error.
    pub fn parse(q: &str) -> Result<Option<Search>, String> {
        let text = q.trim();
        if text.is_empty() {
            return Ok(None);
        }
        if text.chars().count() > MAX_QUERY_CHARS {
            return Err(format!("q must be at most {MAX_QUERY_CHARS} characters"));
        }
        let alternatives = if has_operators(text) {
            None
        } else {
            prefix_tsquery(&tokens(text))
        };
        Ok(Some(Search {
            text: text.to_string(),
            alternatives,
            corrected: None,
        }))
    }

    /// Add the typo-corrected query (from [`correct`]) to the alternatives.
    pub fn with_correction(mut self, corrected: Vec<String>) -> Search {
        if let Some(extra) = prefix_tsquery(&corrected) {
            self.alternatives = Some(match self.alternatives.take() {
                Some(a) => format!("({a}) | ({extra})"),
                None => extra,
            });
        }
        self.corrected = Some(corrected.join(" "));
        self
    }
}

/// Most edits allowed when correcting a word of `len` letters.
fn max_edits(len: usize) -> usize {
    match len {
        0..MIN_CORRECTED_LEN => 0,
        4..=5 => 1,
        _ => 2,
    }
}

/// The closest word of `lexicon` to `word`, within [`max_edits`].
pub fn nearest<'a>(word: &str, lexicon: &'a [String]) -> Option<&'a str> {
    let limit = max_edits(word.len());
    if limit == 0 || word.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    lexicon
        .iter()
        .filter(|w| w.len().abs_diff(word.len()) <= limit && w.as_str() != word)
        .map(|w| (strsim::damerau_levenshtein(word, w), w))
        .filter(|(d, _)| *d <= limit)
        .min_by(|(da, a), (db, b)| {
            da.cmp(db)
                .then_with(|| {
                    strsim::jaro_winkler(word, b)
                        .partial_cmp(&strsim::jaro_winkler(word, a))
                        .unwrap_or(std::cmp::Ordering::Equal)
                })
                .then_with(|| a.cmp(b))
        })
        .map(|(_, w)| w.as_str())
}

/// `words` with each word in `unmatched` replaced by its nearest lexicon
/// word, or `None` when nothing changed.
pub fn correct(
    words: &[String],
    unmatched: &HashSet<String>,
    lexicon: &[String],
) -> Option<Vec<String>> {
    let mut changed = false;
    let out = words
        .iter()
        .map(|w| {
            if unmatched.contains(w) {
                if let Some(n) = nearest(w, lexicon) {
                    changed = true;
                    return n.to_string();
                }
            }
            w.clone()
        })
        .collect();
    changed.then_some(out)
}

/// Correct typos in `search` against the upcoming events in the database
/// (see the module docs). Queries with web-search syntax are left alone.
pub async fn resolve(pool: &PgPool, search: Search) -> sqlx::Result<Search> {
    if has_operators(&search.text) {
        return Ok(search);
    }
    let words = tokens(&search.text);
    let candidates: Vec<String> = words
        .iter()
        .filter(|w| max_edits(w.len()) > 0)
        .cloned()
        .collect();
    if candidates.is_empty() {
        return Ok(search);
    }
    let unmatched: HashSet<String> = crate::repo::search_unmatched_words(pool, &candidates)
        .await?
        .into_iter()
        .collect();
    if unmatched.is_empty() {
        return Ok(search);
    }
    let lexicon = crate::repo::search_lexicon(pool).await?;
    Ok(match correct(&words, &unmatched, &lexicon) {
        Some(c) => search.with_correction(c),
        None => search,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(words: &[&str]) -> Vec<String> {
        words.iter().map(|w| w.to_string()).collect()
    }

    #[test]
    fn fold_strips_accents_and_lowercases() {
        assert_eq!(fold("Sámi ÉLAN Łódź"), "sami elan lodz");
        assert_eq!(
            fold("Straße Æsop Ørsted O’Brien"),
            "strasse aesop orsted o'brien"
        );
        assert_eq!(FOLD_FROM.chars().count(), FOLD_TO.chars().count());
    }

    #[test]
    fn tokens_are_folded_ascii_words() {
        assert_eq!(
            tokens("  Sámi — art's & 'paintings'! "),
            s(&["sami", "art", "s", "paintings"])
        );
        assert_eq!(tokens("a:* | b & !c <-> d"), s(&["a", "b", "c", "d"]));
        assert_eq!(tokens(&"word ".repeat(40)).len(), MAX_TOKENS);
    }

    #[test]
    fn prefix_query_marks_the_last_word() {
        assert_eq!(
            prefix_tsquery(&s(&["whitech"])).as_deref(),
            Some("whitech:*")
        );
        assert_eq!(
            prefix_tsquery(&s(&["life", "draw"])).as_deref(),
            Some("life & draw:*")
        );
        assert_eq!(prefix_tsquery(&[]), None);
    }

    #[test]
    fn parse_trims_caps_and_skips_operator_queries() {
        assert_eq!(Search::parse("   ").unwrap(), None);
        let q = Search::parse(" Barbican  talks ").unwrap().unwrap();
        assert_eq!(q.text, "Barbican  talks");
        assert_eq!(q.alternatives.as_deref(), Some("barbican & talks:*"));
        assert!(
            Search::parse(&"x".repeat(MAX_QUERY_CHARS))
                .unwrap()
                .is_some()
        );
        assert!(Search::parse(&"x".repeat(MAX_QUERY_CHARS + 1)).is_err());
        for q in ["\"life drawing\"", "painting -oil", "print or photo"] {
            assert_eq!(Search::parse(q).unwrap().unwrap().alternatives, None, "{q}");
        }
        // Only punctuation: nothing for the prefix query.
        assert_eq!(Search::parse("&&").unwrap().unwrap().alternatives, None);
    }

    #[test]
    fn nearest_allows_few_edits_by_length() {
        let lexicon = s(&[
            "whitechapel",
            "gallery",
            "barbican",
            "tate",
            "modern",
            "print",
        ]);
        assert_eq!(nearest("whitechaple", &lexicon), Some("whitechapel"));
        assert_eq!(nearest("galery", &lexicon), Some("gallery"));
        assert_eq!(nearest("barbicn", &lexicon), Some("barbican"));
        assert_eq!(nearest("pint", &lexicon), Some("print"));
        assert_eq!(nearest("tat", &lexicon), None, "too short to correct");
        assert_eq!(nearest("xyzzyplugh", &lexicon), None);
        assert_eq!(
            nearest("2026", &s(&["2025"])),
            None,
            "numbers are never corrected"
        );
    }

    #[test]
    fn correct_replaces_only_unmatched_words() {
        let lexicon = s(&["whitechapel", "gallery"]);
        let unmatched: HashSet<String> = ["whitechaple".to_string()].into();
        assert_eq!(
            correct(&s(&["whitechaple", "galery"]), &unmatched, &lexicon),
            Some(s(&["whitechapel", "galery"]))
        );
        assert_eq!(correct(&s(&["gallery"]), &unmatched, &lexicon), None);
        let q = Search::parse("Whitechaple").unwrap().unwrap();
        let q = q.with_correction(s(&["whitechapel"]));
        assert_eq!(
            q.alternatives.as_deref(),
            Some("(whitechaple:*) | (whitechapel:*)")
        );
        assert_eq!(q.corrected.as_deref(), Some("whitechapel"));
    }
}
