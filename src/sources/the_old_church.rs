//! The Old Church — a converted chapel on Stoke Newington Church Street
//! hosting gigs, album launches, early-music concerts and sound baths
//! (issue #224). Read from the data its Wix site embeds in the What's On
//! page.
//!
//! * robots.txt (checked 2026-11-09, saved as a fixture): `User-agent: *`
//!   allows everything except `*?lightbox=`.
//! * `/whats-on` is the Wix Events list. It has no JSON-LD (the event pages
//!   do, with nothing more than the list), but it embeds the list's data as
//!   JSON in `<script id="wix-warmup-data">`: under the Wix Events app
//!   ([`WIX_EVENTS_APP`]), each list widget's `events.events`. One page, one
//!   request a run: the list holds the next 18 or so events, and the rest
//!   load only with the page's "Load more" script, so later events come in
//!   as they reach the top of the list.
//! * Each event's `scheduling.config` has a UTC `startDate`/`endDate`
//!   (`2026-11-11T19:30:00.000Z` for the 19:30 the list prints, in GMT; the
//!   event pages' JSON-LD agrees) and a `timeZoneId` of Europe/London; the
//!   stray milliseconds Wix adds to ends (`23:30:00.756Z`) are dropped.
//!   Every event is timed, so none is `all_day`; the end is dropped when
//!   the site hides it (`endDateHidden`). Events with no date yet
//!   (`scheduleTbd`) and cancelled ones (`status` 3) are skipped.
//! * The location names only "London", so the venue name is fixed; the
//!   address and coordinates come from the event. Its page is
//!   `/event-details/<slug>` (served even when the list links only the
//!   external ticket site). Prices are only on those ticket sites, so price
//!   is unknown.
//! * Category: music, the venue's programme, unless the title reads as a
//!   workshop or talk (`map_category`). Wellness sessions (gong baths, sound
//!   journeys, yoga: [`WELLNESS`] in the title or summary) are skipped. The
//!   site has no genre labels, so the music subtags come from the summary's
//!   words through `crate::music` (the title's are added by the sync), and a
//!   gig whose summary names no genre has none.

use async_trait::async_trait;
use chrono::{DateTime, SubsecRound, Utc};
use scraper::{Html, Selector};
use serde_json::Value;
use url::Url;

use super::{Source, SourceError};
use crate::fetch::FetchContext;
use crate::model::{Category, NewEvent, Price, RawEvent};
use crate::normalise::{
    clean_description, clean_text, dedupe_key, map_category, padded_words, parse_datetime,
};

pub const KEY: &str = "the-old-church";
const LISTING_PATH: &str = "/whats-on";
const EVENT_PATH: &str = "/event-details/";
const VENUE: &str = "The Old Church";
/// Wix's app definition id for Wix Events, the key of its widgets' data.
const WIX_EVENTS_APP: &str = "140603ad-af8d-84a5-2c80-a0f60cb47351";
/// Wix's `status` of a cancelled event.
const CANCELED: i64 = 3;
/// Words (lowercase, on word boundaries) marking a wellness session.
const WELLNESS: &[&str] = &[
    "gong",
    "sound bath",
    "sound journey",
    "sound meditation",
    "meditation",
    "yoga",
    "breathwork",
    "reiki",
    "cacao",
];

pub struct TheOldChurch {
    base_url: Url,
}

impl TheOldChurch {
    pub fn new(base_url: Url) -> Self {
        Self { base_url }
    }
}

/// The events in the What's On page's embedded Wix Events data, in list
/// order, each kept as Wix gives it. `None` when the page carries no Wix
/// Events list at all (not the same as an empty one).
pub fn parse_listing(html: &str) -> Option<Vec<Value>> {
    let doc = Html::parse_document(html);
    let script = doc
        .select(&Selector::parse("script#wix-warmup-data").expect("valid selector"))
        .next()?;
    let data: Value = serde_json::from_str(&script.text().collect::<String>()).ok()?;
    let widgets = data
        .get("appsWarmupData")?
        .get(WIX_EVENTS_APP)?
        .as_object()?;
    let lists: Vec<&Vec<Value>> = widgets
        .values()
        .filter_map(|w| w.get("events")?.get("events")?.as_array())
        .collect();
    if lists.is_empty() {
        return None;
    }
    let mut out: Vec<Value> = Vec::new();
    for ev in lists.into_iter().flatten() {
        if !out.iter().any(|o| o.get("id") == ev.get("id")) {
            out.push(ev.clone());
        }
    }
    Some(out)
}

/// A listed event as a [`RawEvent`] (its page under `base_url`); `None`
/// without an id or slug.
pub fn raw_event(event: Value, base_url: &Url) -> Option<RawEvent> {
    let id = event.get("id")?.as_str()?.to_string();
    let slug = event.get("slug")?.as_str().filter(|s| !s.is_empty())?;
    let url = base_url.join(&format!("{EVENT_PATH}{slug}")).ok()?;
    Some(RawEvent {
        source_event_id: id,
        source_url: Some(url.to_string()),
        payload: serde_json::json!({ "url": url.as_str(), "event": event }),
    })
}

fn is_wellness(title: &str, summary: &str) -> bool {
    let hay = format!("{}{}", padded_words(title), padded_words(summary));
    WELLNESS.iter().any(|w| hay.contains(&format!(" {w} ")))
}

/// The title without the "(SOLD OUT)" the venue appends to it.
fn clean_title(title: &str) -> String {
    const SOLD_OUT: &str = "(sold out)";
    let t = clean_text(title);
    let cut = t.len().saturating_sub(SOLD_OUT.len());
    match t.get(cut..) {
        Some(tail) if cut > 0 && tail.eq_ignore_ascii_case(SOLD_OUT) => {
            t[..cut].trim_end().to_string()
        }
        _ => t,
    }
}

/// Normalise a The Old Church [`RawEvent`] payload.
pub fn normalise_payload(payload: &Value) -> Result<Option<NewEvent>, SourceError> {
    let ev = payload
        .get("event")
        .ok_or_else(|| SourceError::Parse("payload without event".into()))?;
    let text = |v: &Value, key: &str| v.get(key).and_then(Value::as_str).map(str::to_string);
    let title = text(ev, "title")
        .map(|t| clean_title(&t))
        .filter(|t| !t.is_empty())
        .ok_or_else(|| SourceError::Parse("event without a title".into()))?;
    if ev.get("status").and_then(Value::as_i64) == Some(CANCELED) {
        return Ok(None);
    }
    let schedule = &ev["scheduling"]["config"];
    if schedule["scheduleTbd"].as_bool() == Some(true) {
        return Ok(None);
    }
    let date = |key: &str| -> Option<DateTime<Utc>> {
        schedule
            .get(key)
            .and_then(Value::as_str)
            .and_then(parse_datetime)
            .map(|t| t.trunc_subsecs(0))
    };
    let starts_at =
        date("startDate").ok_or_else(|| SourceError::Parse(format!("{title:?}: no startDate")))?;
    let ends_at = date("endDate")
        .filter(|_| schedule["endDateHidden"].as_bool() != Some(true))
        .filter(|e| *e > starts_at);

    let summary = text(ev, "description").unwrap_or_default();
    if is_wellness(&title, &summary) {
        return Ok(None);
    }
    let category = match map_category(&[&title]) {
        Some(c @ (Category::Workshop | Category::Talk)) => c,
        _ => Category::Music,
    };
    let about = text(ev, "about").unwrap_or_default();
    let tags = crate::music::tags_for(
        category.as_str(),
        &[clean_text(&summary), clean_text(&about)],
        "",
    )
    .into_iter()
    .map(str::to_string)
    .collect();
    let description = [summary.as_str(), about.as_str()]
        .into_iter()
        .filter(|s| !s.trim().is_empty())
        .collect::<Vec<_>>()
        .join("\n");

    let location = &ev["location"];
    let address = text(location, "address")
        .map(|a| clean_text(&a))
        .filter(|a| !a.is_empty());
    let coordinate = |key: &str| location["coordinates"][key].as_f64();

    Ok(Some(NewEvent {
        sessions: Vec::new(),
        dedupe_key: dedupe_key(&title, starts_at, Some(VENUE)),
        description: clean_description(Some(&description)),
        title,
        venue_name: Some(VENUE.to_string()),
        address,
        lat: coordinate("lat"),
        lng: coordinate("lng"),
        starts_at,
        ends_at,
        all_day: false,
        price: Price::default(),
        url: text(payload, "url"),
        image_url: text(&ev["mainImage"], "url"),
        category,
        tags,
    }))
}

#[async_trait]
impl Source for TheOldChurch {
    fn key(&self) -> &str {
        KEY
    }

    async fn fetch(&self, ctx: &FetchContext) -> Result<Vec<RawEvent>, SourceError> {
        let listing_url = self
            .base_url
            .join(LISTING_PATH)
            .map_err(|e| SourceError::Config(e.to_string()))?;
        let html = ctx.get_text(&listing_url).await?;
        let events = parse_listing(&html)
            .ok_or_else(|| SourceError::Parse("no Wix Events data on the What's On page".into()))?;
        let mut out = Vec::new();
        for event in events {
            match raw_event(event, &self.base_url) {
                Some(raw) => out.push(raw),
                None => ctx.report_error("listed event without an id or slug".to_string()),
            }
        }
        Ok(out)
    }

    fn normalise(&self, raw: &RawEvent) -> Result<Option<NewEvent>, SourceError> {
        normalise_payload(&raw.payload)
    }

    fn qa_scope(&self) -> Option<&'static str> {
        Some(
            "Only the events the What's On page shows before \"Load more\"; wellness \
             sessions (gong baths, sound journeys, sound meditations, yoga) and cancelled \
             events are left out on purpose.",
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn payload(title: &str, summary: &str, config: Value) -> Value {
        json!({
            "url": "https://www.theoldchurch.org.uk/event-details/x",
            "event": {
                "id": "x",
                "title": title,
                "description": summary,
                "about": "",
                "status": 0,
                "scheduling": {"config": config},
                "location": {"name": "London", "address": "Stoke Newington Church St, London N16 9ES, UK"},
            },
        })
    }

    fn summer() -> Value {
        json!({
            "scheduleTbd": false,
            "startDate": "2026-07-02T18:30:00.000Z",
            "endDate": "2026-07-02T21:00:00.756Z",
            "timeZoneId": "Europe/London",
            "endDateHidden": false,
        })
    }

    fn norm(p: &Value) -> Option<NewEvent> {
        normalise_payload(p).unwrap()
    }

    #[test]
    fn utc_times_are_kept_in_summer() {
        let ev = norm(&payload("A GIG", "Jazz and more.", summer())).unwrap();
        assert_eq!(ev.starts_at.to_rfc3339(), "2026-07-02T18:30:00+00:00");
        assert_eq!(
            ev.ends_at.map(|t| t.to_rfc3339()).as_deref(),
            Some("2026-07-02T21:00:00+00:00")
        );
        assert!(!ev.all_day);
        assert_eq!(ev.category, Category::Music);
        assert_eq!(ev.tags, ["jazz"]);
        assert_eq!(ev.venue_name.as_deref(), Some(VENUE));
    }

    #[test]
    fn a_hidden_end_is_dropped() {
        let mut config = summer();
        config["endDateHidden"] = json!(true);
        assert_eq!(norm(&payload("A GIG", "", config)).unwrap().ends_at, None);
    }

    #[test]
    fn undated_cancelled_and_wellness_events_are_skipped() {
        let mut tbd = summer();
        tbd["scheduleTbd"] = json!(true);
        assert!(norm(&payload("A GIG", "", tbd)).is_none());
        let mut cancelled = payload("A GIG", "", summer());
        cancelled["event"]["status"] = json!(CANCELED);
        assert!(norm(&cancelled).is_none());
        for (title, summary) in [
            ("FULL MOON GONG IMMERSION", ""),
            ("SOUND JOURNEY - SETTLING INTO AUTUMN", ""),
            ("RESTORE", "A beautifully immersive Sound Meditation."),
            ("HATHA YOGA WITH EISHA KAROL", ""),
        ] {
            assert!(
                norm(&payload(title, summary, summer())).is_none(),
                "{title}"
            );
        }
    }

    #[test]
    fn workshops_and_talks_keep_their_category() {
        let ev = norm(&payload("FAMILY ART WORKSHOP", "", summer())).unwrap();
        assert_eq!(ev.category, Category::Workshop);
        assert!(ev.tags.is_empty());
        let ev = norm(&payload("IN CONVERSATION WITH A COMPOSER", "", summer())).unwrap();
        assert_eq!(ev.category, Category::Talk);
    }

    #[test]
    fn the_sold_out_suffix_is_dropped() {
        assert_eq!(clean_title("RIDGECLUB (SOLD OUT)"), "RIDGECLUB");
        assert_eq!(clean_title("RIDGECLUB (Sold Out)"), "RIDGECLUB");
        assert_eq!(clean_title("(SOLD OUT) AGAIN"), "(SOLD OUT) AGAIN");
    }

    #[test]
    fn a_missing_start_is_a_parse_error() {
        let err = normalise_payload(&payload("A GIG", "", json!({"scheduleTbd": false})))
            .unwrap_err()
            .to_string();
        assert!(err.contains("no startDate"), "{err}");
    }
}
