//! Sinfonia Smith Square (formerly St John's Smith Square, Westminster,
//! SW1P) — concerts from the site's own "What's On" feed (issue #217).
//!
//! * robots.txt (checked 2026-11-08, saved as a fixture): the `User-agent: *`
//!   group disallows nothing.
//! * The `/whats-on/` page renders its list with JavaScript from a static
//!   JSON file, `/wp-content/uploads/whats-on.json` (all upcoming events,
//!   about 40), which carries everything the list shows: one request a run,
//!   no detail pages. The pages have no JSON-LD `Event`s.
//! * Times: each item's `timestamp` lists its performances as London wall
//!   clock (`"2026-11-29 19:00"` for "Sun 29 Nov 2026 7.00pm"); two or more
//!   are sessions (#207). `duration` (minutes, or `false`) gives each
//!   session's end.
//! * Venues: the hall and the crypt are both at the church; an item whose
//!   venues include `elsewhere` (Westminster Abbey, Wimbledon, the Anghiari
//!   festival in Tuscany) gives no address, so it is skipped.
//! * Category: everything is music except an item whose types are only
//!   talks (→ talk), community events (→ community) or dance and ballet
//!   (skipped); family and immersive concerts are music. The venue's types
//!   are the tags, so `crate::music` reads `orchestral`, `choral`, `jazz`,
//!   `contemporary`…; `classical` is added unless the types or title say
//!   jazz, folk or gospel ([`NOT_CLASSICAL`]).
//! * Description: the item's excerpt, else its repertoire (works and
//!   performers). Price: only `free`; the feed has no amounts.
//! * Ids: the WordPress slug; the link must be an `/event/` page on the site.

use async_trait::async_trait;
use chrono::Duration;
use serde_json::Value;
use url::Url;

use super::{Source, SourceError};
use crate::fetch::FetchContext;
use crate::model::{Category, NewEvent, Price, RawEvent, Session};
use crate::normalise::{
    clean_description, clean_text, dedupe_key, padded_words, parse_london_wall_clock,
};

pub const KEY: &str = "sinfonia-smith-square";
pub const FEED_PATH: &str = "/wp-content/uploads/whats-on.json";
const EVENT_PREFIX: &str = "/event/";
const VENUE_NAME: &str = "Sinfonia Smith Square";
const VENUE_ADDRESS: &str = "Smith Square, Westminster, London SW1P 3HA";
const ELSEWHERE: &str = "elsewhere";
/// "Visiting": a hire by a visiting promoter, not a genre.
const VISITING: &str = "external";
/// Types that make an item something other than a concert when no other
/// type does.
const TALK_TYPES: &[&str] = &["talks-and-events"];
const COMMUNITY_TYPES: &[&str] = &["community-event"];
const DANCE_TYPES: &[&str] = &["dance", "ballet"];
/// Types that say nothing about the kind of event.
const NEUTRAL_TYPES: &[&str] = &[VISITING, "relaxed-performances"];
/// Words (as [`padded_words`]) in the types or title of a concert that is
/// not classical.
const NOT_CLASSICAL: &[&str] = &["jazz", "folk", "gospel"];

pub struct SinfoniaSmithSquare {
    base_url: Url,
}

impl SinfoniaSmithSquare {
    pub fn new(base_url: Url) -> Self {
        Self { base_url }
    }
}

/// Parse the feed fetched from `feed_url` into one [`RawEvent`] per item, in
/// feed order, the item itself as the payload with its `link` resolved. An
/// item without a slug or an `/event/` link on the feed's host is an error.
pub fn parse_feed(
    feed: &Value,
    feed_url: &Url,
) -> Result<Vec<Result<RawEvent, SourceError>>, SourceError> {
    let items = feed
        .get("items")
        .and_then(Value::as_array)
        .ok_or_else(|| SourceError::Parse("no items in the What's On feed".into()))?;
    Ok(items
        .iter()
        .map(|item| {
            let s = |k: &str| item.get(k).and_then(Value::as_str);
            let bad = |what: &str| SourceError::Parse(format!("{:?}: {what}", s("title")));
            let slug = s("slug")
                .filter(|slug| !slug.is_empty())
                .ok_or_else(|| bad("no slug"))?;
            let url = s("link")
                .and_then(|u| feed_url.join(u).ok())
                .filter(|u| {
                    u.host_str() == feed_url.host_str()
                        && u.path()
                            .strip_prefix(EVENT_PREFIX)
                            .is_some_and(|slug| !slug.trim_matches('/').is_empty())
                })
                .ok_or_else(|| bad("no /event/ link"))?;
            let mut payload = item.clone();
            payload["link"] = Value::from(url.as_str());
            Ok(RawEvent {
                source_event_id: slug.to_string(),
                source_url: Some(url.to_string()),
                payload,
            })
        })
        .collect())
}

/// The non-empty `slug`s of the item's `key` list (`types`, `venues`).
fn slugs<'a>(payload: &'a Value, key: &str) -> Vec<&'a str> {
    payload
        .get(key)
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|t| t.get("slug")?.as_str())
        .filter(|s| !s.is_empty())
        .collect()
}

/// The category of an item with these type slugs; `None` to skip it.
pub fn category(types: &[&str]) -> Option<Category> {
    let has = |list: &[&str]| types.iter().any(|t| list.contains(t));
    let not_music = [TALK_TYPES, COMMUNITY_TYPES, DANCE_TYPES, NEUTRAL_TYPES].concat();
    let concert = types.iter().any(|t| !not_music.contains(t));
    if concert || types.iter().all(|t| NEUTRAL_TYPES.contains(t)) {
        Some(Category::Music)
    } else if has(TALK_TYPES) {
        Some(Category::Talk)
    } else if has(COMMUNITY_TYPES) {
        Some(Category::Community)
    } else {
        None
    }
}

/// The item's tags: its type names but "Visiting", plus `classical` unless
/// a type or the title names a genre in [`NOT_CLASSICAL`].
fn tags(payload: &Value, title: &str) -> Vec<String> {
    let mut tags: Vec<String> = payload
        .get("types")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|t| t.get("slug").and_then(Value::as_str) != Some(VISITING))
        .filter_map(|t| t.get("name")?.as_str())
        .map(|name| clean_text(name).to_lowercase())
        .filter(|name| !name.is_empty())
        .collect();
    let hay = format!("{}{}", padded_words(&tags.join(" ")), padded_words(title));
    if !NOT_CLASSICAL
        .iter()
        .any(|w| hay.contains(&format!(" {w} ")))
    {
        tags.push("classical".to_string());
    }
    tags
}

/// Normalise a Sinfonia Smith Square feed item.
pub fn normalise_payload(payload: &Value) -> Result<Option<NewEvent>, SourceError> {
    let s = |k: &str| payload.get(k).and_then(Value::as_str);
    let title = s("title")
        .map(clean_text)
        .filter(|t| !t.is_empty())
        .ok_or_else(|| SourceError::Parse("event without a title".into()))?;
    if slugs(payload, "venues").contains(&ELSEWHERE) {
        return Ok(None);
    }
    let Some(category) = category(&slugs(payload, "types")) else {
        return Ok(None);
    };
    let bad = |k: &str| SourceError::Parse(format!("{title:?}: bad {k} {:?}", payload.get(k)));
    let starts = payload
        .get("timestamp")
        .and_then(Value::as_array)
        .filter(|t| !t.is_empty())
        .ok_or_else(|| bad("timestamp"))?
        .iter()
        .map(|t| t.as_str().and_then(parse_london_wall_clock))
        .collect::<Option<Vec<_>>>()
        .ok_or_else(|| bad("timestamp"))?;
    let duration = s("duration")
        .and_then(|d| d.trim().parse::<i64>().ok())
        .filter(|d| *d > 0)
        .map(Duration::minutes);
    let description = s("excerpt")
        .and_then(|e| clean_description(Some(e)))
        .or_else(|| clean_description(s("repertoire")));

    let mut event = NewEvent {
        title,
        description,
        venue_name: Some(VENUE_NAME.to_string()),
        address: Some(VENUE_ADDRESS.to_string()),
        lat: None,
        lng: None,
        starts_at: starts[0],
        ends_at: None,
        all_day: false,
        sessions: Vec::new(),
        price: Price {
            is_free: payload.get("free").and_then(Value::as_bool) == Some(true),
            ..Price::default()
        },
        url: s("link").map(str::to_string),
        image_url: s("image").filter(|i| !i.is_empty()).map(str::to_string),
        category,
        tags: Vec::new(),
        dedupe_key: String::new(),
    };
    event.tags = tags(payload, &event.title);
    event.set_sessions(
        starts
            .into_iter()
            .map(|starts_at| Session {
                starts_at,
                ends_at: duration.map(|d| starts_at + d),
            })
            .collect(),
    );
    event.dedupe_key = dedupe_key(&event.title, event.starts_at, Some(VENUE_NAME));
    Ok(Some(event))
}

#[async_trait]
impl Source for SinfoniaSmithSquare {
    fn key(&self) -> &str {
        KEY
    }

    async fn fetch(&self, ctx: &FetchContext) -> Result<Vec<RawEvent>, SourceError> {
        let url = self
            .base_url
            .join(FEED_PATH)
            .map_err(|e| SourceError::Config(e.to_string()))?;
        let items = parse_feed(&ctx.get_json(&url).await?, &url)?;
        if items.is_empty() {
            return Err(SourceError::Parse("no events in the What's On feed".into()));
        }
        let mut out = Vec::new();
        for item in items {
            match item {
                Ok(raw) => out.push(raw),
                Err(e) => ctx.report_error(e.to_string()),
            }
        }
        Ok(out)
    }

    fn normalise(&self, raw: &RawEvent) -> Result<Option<NewEvent>, SourceError> {
        normalise_payload(&raw.payload)
    }

    fn qa_scope(&self) -> Option<&'static str> {
        Some(
            "Only events at Smith Square (the Hall and the Crypt). Events the site lists \
             under the venue \"Elsewhere\" (e.g. at Westminster Abbey or abroad) are left \
             out on purpose, as are dance and ballet without music.",
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn item(types: &[&str], venue: &str, timestamps: &[&str], duration: Value) -> Value {
        json!({
            "title": "Hail Mary!",
            "slug": "hail-mary",
            "link": "https://www.sinfoniasmithsq.org.uk/event/hail-mary/",
            "excerpt": "",
            "repertoire": "<p><strong>Byrd</strong> Mass</p>",
            "free": false,
            "duration": duration,
            "timestamp": timestamps,
            "types": types.iter().map(|t| json!({"name": t, "slug": t})).collect::<Vec<_>>(),
            "venues": [{"name": venue, "slug": venue}],
        })
    }

    #[test]
    fn categories() {
        assert_eq!(category(&["choral"]), Some(Category::Music));
        assert_eq!(category(&[]), Some(Category::Music));
        assert_eq!(category(&["family"]), Some(Category::Music));
        assert_eq!(category(&["external"]), Some(Category::Music));
        assert_eq!(category(&["talks-and-events"]), Some(Category::Talk));
        assert_eq!(
            category(&["talks-and-events", "external"]),
            Some(Category::Talk)
        );
        assert_eq!(
            category(&["community-event", "talks-and-events"]),
            Some(Category::Talk)
        );
        assert_eq!(category(&["community-event"]), Some(Category::Community));
        assert_eq!(category(&["dance"]), None);
        assert_eq!(category(&["ballet", "orchestral"]), Some(Category::Music));
    }

    #[test]
    fn times_are_london_wall_clock_with_the_duration_as_the_end() {
        let bst = normalise_payload(&item(
            &["choral"],
            "smith-square-hall",
            &["2026-10-15 19:00"],
            json!("90"),
        ))
        .unwrap()
        .unwrap();
        assert_eq!(bst.starts_at.to_rfc3339(), "2026-10-15T18:00:00+00:00");
        assert_eq!(
            bst.ends_at.map(|e| e.to_rfc3339()).as_deref(),
            Some("2026-10-15T19:30:00+00:00")
        );
        assert!(!bst.all_day && bst.sessions.is_empty());
        let open = normalise_payload(&item(
            &["choral"],
            "smith-square-hall",
            &["2026-12-18 19:30"],
            json!(false),
        ))
        .unwrap()
        .unwrap();
        assert_eq!(open.starts_at.to_rfc3339(), "2026-12-18T19:30:00+00:00");
        assert!(open.ends_at.is_none());
    }

    #[test]
    fn several_timestamps_are_sessions() {
        let e = normalise_payload(&item(
            &["family"],
            "smith-square-hall",
            &["2026-12-20 11:00", "2026-12-19 13:30"],
            json!("60"),
        ))
        .unwrap()
        .unwrap();
        assert_eq!(e.sessions.len(), 2);
        assert_eq!(e.starts_at.to_rfc3339(), "2026-12-19T13:30:00+00:00");
        assert_eq!(
            e.ends_at.map(|e| e.to_rfc3339()).as_deref(),
            Some("2026-12-20T12:00:00+00:00")
        );
    }

    #[test]
    fn events_elsewhere_and_dance_are_skipped() {
        let elsewhere = json!({
            "title": "Messiah",
            "timestamp": ["2026-11-27 19:00"],
            "types": [{"name": "Choral", "slug": "choral"}],
            "venues": [
                {"name": "Westminster Abbey", "slug": "westminster-abbey"},
                {"name": "Elsewhere", "slug": "elsewhere"}
            ],
        });
        assert!(normalise_payload(&elsewhere).unwrap().is_none());
        let dance = item(
            &["dance"],
            "smith-square-hall",
            &["2026-11-27 19:00"],
            json!("60"),
        );
        assert!(normalise_payload(&dance).unwrap().is_none());
    }

    #[test]
    fn a_missing_or_bad_timestamp_is_a_parse_error() {
        for timestamps in [&[][..], &["soon"][..]] {
            let err = normalise_payload(&item(
                &["choral"],
                "smith-square-hall",
                timestamps,
                json!(false),
            ))
            .unwrap_err()
            .to_string();
            assert!(err.contains("bad timestamp"), "{err}");
        }
    }

    #[test]
    fn classical_unless_the_types_or_title_name_another_genre() {
        let tags = |types: &[&str], title: &str| {
            let mut p = item(
                types,
                "smith-square-hall",
                &["2026-11-27 19:00"],
                json!("60"),
            );
            p["title"] = json!(title);
            normalise_payload(&p).unwrap().unwrap().tags
        };
        assert_eq!(tags(&["choral"], "Hail Mary!"), ["choral", "classical"]);
        assert_eq!(tags(&[], "Earth, Wind, Fire & Water Music"), ["classical"]);
        assert_eq!(tags(&["jazz"], "Ellington's Nutcracker"), ["jazz"]);
        assert_eq!(
            tags(&["vocal"], "Joy! A Seasonal Gospel Celebration"),
            ["vocal"]
        );
        assert_eq!(
            tags(&["choral", "external"], "Messiah"),
            ["choral", "classical"]
        );
    }

    #[test]
    fn the_description_is_the_excerpt_else_the_repertoire() {
        let mut p = item(
            &["choral"],
            "smith-square-hall",
            &["2026-11-27 19:00"],
            json!("60"),
        );
        let e = normalise_payload(&p).unwrap().unwrap();
        assert_eq!(e.description.as_deref(), Some("Byrd Mass"));
        p["excerpt"] = json!("The Tallis Scholars sing Byrd.");
        let e = normalise_payload(&p).unwrap().unwrap();
        assert_eq!(
            e.description.as_deref(),
            Some("The Tallis Scholars sing Byrd.")
        );
    }

    #[test]
    fn an_item_without_a_slug_or_an_event_link_is_an_error() {
        let feed_url =
            Url::parse("https://www.sinfoniasmithsq.org.uk/wp-content/uploads/whats-on.json")
                .unwrap();
        let feed = json!({"items": [
            {"title": "A", "slug": "a", "link": "https://elsewhere.example/event/a/"},
            {"title": "B", "slug": "", "link": "https://www.sinfoniasmithsq.org.uk/event/b/"},
            {"title": "C", "slug": "c", "link": "https://www.sinfoniasmithsq.org.uk/festival/c/"},
            {"title": "D", "slug": "d", "link": "https://www.sinfoniasmithsq.org.uk/event/d/"},
        ]});
        let items = parse_feed(&feed, &feed_url).unwrap();
        assert!(items[0].as_ref().unwrap_err().to_string().contains("link"));
        assert!(items[1].as_ref().unwrap_err().to_string().contains("slug"));
        assert!(items[2].as_ref().unwrap_err().to_string().contains("link"));
        assert_eq!(items[3].as_ref().unwrap().source_event_id, "d");
        assert!(parse_feed(&json!({}), &feed_url).is_err());
    }

    #[test]
    fn a_relative_link_is_resolved_for_the_event_url() {
        let feed_url =
            Url::parse("https://www.sinfoniasmithsq.org.uk/wp-content/uploads/whats-on.json")
                .unwrap();
        let mut p = item(&["choral"], "hall", &["2026-11-29 19:00"], json!("90"));
        p["link"] = json!("/event/hail-mary/");
        let raw = parse_feed(&json!({ "items": [p] }), &feed_url)
            .unwrap()
            .remove(0)
            .unwrap();
        let e = normalise_payload(&raw.payload).unwrap().unwrap();
        let resolved = "https://www.sinfoniasmithsq.org.uk/event/hail-mary/";
        assert_eq!(raw.source_url.as_deref(), Some(resolved));
        assert_eq!(e.url.as_deref(), Some(resolved));
    }

    #[test]
    fn the_free_flag_is_the_price() {
        let mut p = item(&["choral"], "hall", &["2026-11-29 19:00"], json!("90"));
        assert!(!normalise_payload(&p).unwrap().unwrap().price.is_free);
        p["free"] = json!(true);
        assert!(normalise_payload(&p).unwrap().unwrap().price.is_free);
    }

    #[test]
    fn the_qa_scope_names_what_is_left_out() {
        let scope = SinfoniaSmithSquare::new(Url::parse("https://x.test/").unwrap())
            .qa_scope()
            .unwrap();
        for word in ["Elsewhere", "dance", "ballet"] {
            assert!(scope.contains(word), "{word}");
        }
    }
}
