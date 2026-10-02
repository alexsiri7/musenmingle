//! What the QA judge sees: the pages a run fetched (main-content text and
//! JSON-LD) and the records we extracted from them, in one JSON message
//! kept under [`MAX_INPUT_CHARS`].

use std::collections::HashMap;

use chrono::{DateTime, NaiveDate, Utc};
use chrono_tz::Europe::London;
use dom_smoothie::{Readability, TextMode};
use scraper::{Html, Node, Selector};
use serde_json::{Value, json};

use crate::enrich::output::normalise_for_match;
use crate::model::{NewEvent, Price};
use crate::normalise::normalise_title_for_key;
use crate::runner::RunEvent;

/// Upper bound on the user message (~12k tokens at the 3 characters per
/// token [`crate::enrich::estimate_cost`] assumes).
pub const MAX_INPUT_CHARS: usize = 36_000;
/// Readability output shorter than this is not the page's content.
pub const MIN_MAIN_CHARS: usize = 500;
/// On a listing, Readability (built for articles) often keeps one event of
/// many: its text is used only when it keeps at least this share of the
/// visible text, else the judge would report "missed" events it never saw.
pub const LISTING_MIN_MAIN_RATIO: f64 = 0.5;
const MAX_JSON_LD_CHARS: usize = 4_000;
const MAX_LISTING_RECORDS_CHARS: usize = 6_000;
const TRUNCATED: &str = " […truncated]";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PageKind {
    Listing,
    Detail,
    /// A JSON API response.
    Api,
}

impl PageKind {
    pub fn as_str(self) -> &'static str {
        match self {
            PageKind::Listing => "listing",
            PageKind::Detail => "detail",
            PageKind::Api => "api",
        }
    }
}

/// A fetched page to show the judge.
#[derive(Debug, Clone, Copy)]
pub struct PageIn<'a> {
    pub kind: PageKind,
    /// Already redacted (no query string).
    pub url: &'a str,
    pub body: &'a str,
    pub json: bool,
}

/// Text of every text node outside `head`, `script`, `style`, `noscript`,
/// `template` and `svg`, whitespace-collapsed.
pub fn visible_text(html: &str) -> String {
    const HIDDEN: &[&str] = &["head", "script", "style", "noscript", "template", "svg"];
    let doc = Html::parse_document(html);
    let mut parts: Vec<&str> = Vec::new();
    for node in doc.tree.root().descendants() {
        let Node::Text(text) = node.value() else {
            continue;
        };
        let hidden = node.ancestors().any(|a| {
            a.value()
                .as_element()
                .is_some_and(|e| HIDDEN.contains(&e.name()))
        });
        if !hidden {
            parts.push(text);
        }
    }
    parts
        .join(" ")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// Readability's main-content text ("" when it finds none).
pub fn main_text(html: &str) -> String {
    let cfg = dom_smoothie::Config {
        text_mode: TextMode::Formatted,
        ..Default::default()
    };
    Readability::new(html, None, Some(cfg))
        .and_then(|mut r| r.parse())
        .map(|a| a.text_content.trim().to_string())
        .unwrap_or_default()
}

/// Every parseable `application/ld+json` block, whatever its type.
pub fn json_ld_blocks(html: &str) -> Vec<Value> {
    let doc = Html::parse_document(html);
    let sel = Selector::parse(r#"script[type="application/ld+json"]"#).expect("valid selector");
    doc.select(&sel)
        .filter_map(|s| serde_json::from_str(&s.text().collect::<String>()).ok())
        .collect()
}

/// The text to show for a page and where it came from (`readability`,
/// `visible` or `json`).
pub fn page_text(body: &str, kind: PageKind, json: bool) -> (String, &'static str) {
    if json {
        let compact = serde_json::from_str::<Value>(body)
            .map(|v| v.to_string())
            .unwrap_or_else(|_| body.to_string());
        return (compact, "json");
    }
    let main = main_text(body);
    let main_chars = main.chars().count();
    let use_main = match kind {
        PageKind::Detail | PageKind::Api => main_chars >= MIN_MAIN_CHARS,
        PageKind::Listing => {
            main_chars >= MIN_MAIN_CHARS
                && main_chars as f64
                    >= LISTING_MIN_MAIN_RATIO * visible_text(body).chars().count() as f64
        }
    };
    if use_main {
        (main, "readability")
    } else {
        (visible_text(body), "visible")
    }
}

/// A price as a page would show it: `free`, `£5`, `£5–£10`, `5 EUR`.
pub fn price_text(p: &Price) -> Option<String> {
    if p.is_free {
        return Some("free".into());
    }
    let amount = |d: rust_decimal::Decimal| match p.currency.as_deref() {
        Some("GBP") => format!("£{}", d.normalize()),
        Some(c) => format!("{} {c}", d.normalize()),
        None => d.normalize().to_string(),
    };
    match (p.min, p.max) {
        (Some(a), Some(b)) if a != b => Some(format!("{}–{}", amount(a), amount(b))),
        (Some(a), _) | (None, Some(a)) => Some(amount(a)),
        (None, None) => None,
    }
}

fn when(t: DateTime<Utc>, all_day: bool) -> String {
    let local = t.with_timezone(&London);
    if all_day {
        local.format("%Y-%m-%d").to_string()
    } else {
        local.format("%Y-%m-%d %H:%M").to_string()
    }
}

/// Our values for the fields the judge checks, as it sees them.
pub fn record_fields(e: &NewEvent) -> Value {
    json!({
        "title": e.title,
        "starts_at": when(e.starts_at, e.all_day),
        "ends_at": e.ends_at.map(|t| when(t, e.all_day)),
        "all_day": e.all_day,
        "venue_name": e.venue_name,
        "address": e.address,
        "price": price_text(&e.price),
        "category": e.category.as_str(),
    })
}

/// The judge's message and what validation needs to check its answer.
#[derive(Debug, Clone)]
pub struct JudgeInput {
    pub message: String,
    /// [`normalise_for_match`] of every page text and JSON-LD sent.
    pub grounding_all: String,
    /// The same for the listing page only.
    pub grounding_listing: String,
    /// `r1`, `r2`: records whose detail page was sent.
    pub record_ids: Vec<String>,
    /// `l1`, ...: listing rows sent.
    pub listing_ids: Vec<String>,
    /// Our fields of every record sent, by id.
    pub records_by_id: HashMap<String, Value>,
    /// `[{url, kind, text_from, chars}]` (stored in `events.qa_checks.pages`).
    pub pages_meta: Value,
}

/// Length of `s` once written as a JSON string (without the quotes).
fn json_len(s: &str) -> usize {
    s.chars()
        .map(|c| match c {
            '"' | '\\' | '\n' | '\r' | '\t' | '\u{08}' | '\u{0c}' => 2,
            c if (c as u32) < 0x20 => 6,
            c => c.len_utf8(),
        })
        .sum()
}

/// `s` cut so that it (with the truncation marker) takes at most `budget`
/// characters of JSON.
fn fit(s: &str, budget: usize) -> String {
    if json_len(s) <= budget {
        return s.to_string();
    }
    let room = budget.saturating_sub(json_len(TRUNCATED));
    let mut used = 0;
    let mut out = String::new();
    for c in s.chars() {
        let n = json_len(c.encode_utf8(&mut [0; 4]));
        if used + n > room {
            break;
        }
        used += n;
        out.push(c);
    }
    out.push_str(TRUNCATED);
    out
}

/// Whole blocks up to [`MAX_JSON_LD_CHARS`]; a first block over it is cut
/// as a string (the bool says so).
fn cap_json_ld(blocks: Vec<Value>) -> (Vec<Value>, bool) {
    let mut out = Vec::new();
    let mut used = 2;
    for b in blocks {
        let text = b.to_string();
        if used + text.len() + 1 > MAX_JSON_LD_CHARS {
            if out.is_empty() {
                let cut = fit(&text, MAX_JSON_LD_CHARS - 4);
                return (vec![Value::String(cut)], true);
            }
            break;
        }
        used += text.len() + 1;
        out.push(b);
    }
    (out, false)
}

fn leaf_strings(v: &Value, out: &mut Vec<String>) {
    match v {
        Value::String(s) => out.push(s.clone()),
        Value::Array(a) => a.iter().for_each(|x| leaf_strings(x, out)),
        Value::Object(o) => o.values().for_each(|x| leaf_strings(x, out)),
        other => out.push(other.to_string()),
    }
}

fn grounding(texts: &[&str], json_ld: &[&Value]) -> String {
    let mut parts: Vec<String> = texts.iter().map(|t| t.to_string()).collect();
    for v in json_ld {
        parts.push(v.to_string());
        leaf_strings(v, &mut parts);
    }
    normalise_for_match(&parts.join("\n"))
}

struct Page {
    kind: PageKind,
    url: String,
    text: String,
    text_from: &'static str,
    json_ld: Vec<Value>,
    json_ld_truncated: bool,
}

fn page_json(id: usize, p: &Page, text: &str) -> Value {
    let mut v = json!({
        "id": format!("p{}", id + 1),
        "kind": p.kind.as_str(),
        "url": p.url,
        "text_from": p.text_from,
        "text": text,
        "json_ld": p.json_ld,
    });
    if p.json_ld_truncated {
        v["json_ld_truncated"] = json!(true);
    }
    v
}

/// Build the judge's input. `pages[0]` is the listing (or API response)
/// unless it is a detail page; `detail_records` pairs each checked record with the index of its page.
/// `scope` is the source's scope note: shown to the judge, never grounding.
pub fn build(
    source_key: &str,
    scope: Option<&str>,
    today_london: NaiveDate,
    pages: &[PageIn<'_>],
    detail_records: &[(&RunEvent, usize)],
    all_records: &[RunEvent],
) -> JudgeInput {
    let pages: Vec<Page> = pages
        .iter()
        .map(|p| {
            let (text, text_from) = page_text(p.body, p.kind, p.json);
            let (json_ld, json_ld_truncated) = if p.json {
                (Vec::new(), false)
            } else {
                cap_json_ld(json_ld_blocks(p.body))
            };
            Page {
                kind: p.kind,
                url: p.url.to_string(),
                text,
                text_from,
                json_ld,
                json_ld_truncated,
            }
        })
        .collect();

    let mut records_by_id = HashMap::new();
    let mut record_ids = Vec::new();
    let mut records = Vec::new();
    for (i, (r, page)) in detail_records.iter().enumerate() {
        let id = format!("r{}", i + 1);
        let fields = record_fields(&r.event);
        let mut v = fields.clone();
        v["id"] = json!(id);
        v["page"] = json!(format!("p{}", page + 1));
        v["url"] = json!(r.event.url);
        records.push(v);
        records_by_id.insert(id.clone(), fields);
        record_ids.push(id);
    }

    // Without a captured listing (a source that fetches it outside
    // `get_text`/`get_json`), only detail pages are shown.
    let has_listing = pages.first().is_some_and(|p| p.kind != PageKind::Detail);
    let listing_text = pages
        .first()
        .filter(|_| has_listing)
        .map(|p| normalise_for_match(&p.text))
        .unwrap_or_default();
    let detail_ids: Vec<&str> = detail_records
        .iter()
        .map(|(r, _)| r.source_event_id.as_str())
        .collect();
    let visible: Vec<&RunEvent> = all_records
        .iter()
        .filter(|r| r.kept && !detail_ids.contains(&r.source_event_id.as_str()))
        .filter(|r| {
            let t = normalise_for_match(&r.event.title);
            !t.is_empty() && listing_text.contains(&t)
        })
        .collect();
    let mut listing_records = Vec::new();
    let mut listing_ids = Vec::new();
    let mut used = 0;
    for (i, r) in visible.iter().enumerate() {
        let id = format!("l{}", i + 1);
        let e = &r.event;
        let row = json!({
            "id": id,
            "title": e.title,
            "starts_at": when(e.starts_at, e.all_day),
            "ends_at": e.ends_at.map(|t| when(t, e.all_day)),
        });
        let n = row.to_string().len() + 1;
        if used + n > MAX_LISTING_RECORDS_CHARS {
            break;
        }
        used += n;
        records_by_id.insert(id.clone(), record_fields(e));
        listing_ids.push(id);
        listing_records.push(row);
    }
    let not_shown = visible.len() - listing_records.len();

    let skeleton = |texts: &[String]| {
        let mut v = json!({
            "today_london": today_london.format("%Y-%m-%d").to_string(),
            "source": source_key,
            "pages": pages.iter().zip(texts).enumerate()
                .map(|(i, (p, t))| page_json(i, p, t)).collect::<Vec<_>>(),
            "records": records,
            "listing_records": listing_records,
            "listing_records_not_shown": not_shown,
        });
        if let Some(s) = scope {
            v["source_scope"] = json!(s);
        }
        v.to_string()
    };
    let empty = vec![String::new(); pages.len()];
    let room = MAX_INPUT_CHARS.saturating_sub(skeleton(&empty).len());
    let per_page = room / pages.len().max(1);
    let texts: Vec<String> = pages.iter().map(|p| fit(&p.text, per_page)).collect();
    let message = skeleton(&texts);

    let all_ld: Vec<&Value> = pages.iter().flat_map(|p| &p.json_ld).collect();
    let text_refs: Vec<&str> = texts.iter().map(String::as_str).collect();
    let grounding_all = grounding(&text_refs, &all_ld);
    let grounding_listing = match (pages.first(), texts.first()) {
        (Some(p), Some(t)) if has_listing => grounding(&[t], &p.json_ld.iter().collect::<Vec<_>>()),
        _ => String::new(),
    };
    let pages_meta = pages
        .iter()
        .map(|p| {
            json!({
                "url": p.url,
                "kind": p.kind.as_str(),
                "text_from": p.text_from,
                "chars": p.text.chars().count(),
            })
        })
        .collect();

    JudgeInput {
        message,
        grounding_all,
        grounding_listing,
        record_ids,
        listing_ids,
        records_by_id,
        pages_meta,
    }
}

/// Whether a title the judge calls "missed" is one of the run's records
/// (equal, or one key containing the other).
pub fn is_run_record(title: &str, all_records: &[RunEvent]) -> bool {
    let key = normalise_title_for_key(title);
    !key.is_empty()
        && all_records.iter().any(|r| {
            let k = normalise_title_for_key(&r.event.title);
            !k.is_empty() && (k == key || k.contains(&key) || key.contains(&k))
        })
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::model::Category;
    use chrono::TimeZone;
    use rust_decimal::Decimal;

    const BARBICAN_DETAIL: &str = include_str!(
        "../../tests/fixtures/scrapers/barbican/detail/2026/ryman-in-rhythm-sarathy-korwar.html"
    );
    const SOMERSET_LISTING: &str =
        include_str!("../../tests/fixtures/scrapers/somerset-house/whats-on-page-1.html");
    const HUNTERIAN_LISTING: &str =
        include_str!("../../tests/fixtures/scrapers/hunterian-museum/whats-on.html");

    pub(crate) fn run_event(id: &str, title: &str, starts_at: DateTime<Utc>) -> RunEvent {
        RunEvent {
            source_event_id: id.into(),
            source_url: Some(format!("https://venue.test/{id}")),
            event: NewEvent {
                sessions: Vec::new(),
                title: title.into(),
                description: None,
                venue_name: Some("Hall".into()),
                address: None,
                lat: None,
                lng: None,
                starts_at,
                ends_at: None,
                all_day: false,
                price: Price::default(),
                url: Some(format!("https://venue.test/{id}")),
                image_url: None,
                category: Category::Talk,
                tags: vec![],
                dedupe_key: id.into(),
            },
            kept: true,
        }
    }

    fn today() -> NaiveDate {
        NaiveDate::from_ymd_opt(2026, 10, 1).unwrap()
    }

    #[test]
    fn detail_pages_use_readability() {
        let (text, from) = page_text(BARBICAN_DETAIL, PageKind::Detail, false);
        assert_eq!(from, "readability");
        assert!(text.chars().count() >= MIN_MAIN_CHARS);
        assert!(text.contains("Sarathy Korwar"), "{text}");
    }

    #[test]
    fn listings_fall_back_to_visible_text() {
        // Readability keeps a few hundred characters of this listing.
        let (text, from) = page_text(SOMERSET_LISTING, PageKind::Listing, false);
        assert_eq!(from, "visible");
        assert!(text.chars().count() > 1000);
        // Enough main text for a detail page, too little of this listing.
        assert_eq!(
            page_text(HUNTERIAN_LISTING, PageKind::Listing, false).1,
            "visible"
        );
        assert_eq!(
            page_text(HUNTERIAN_LISTING, PageKind::Detail, false).1,
            "readability"
        );
    }

    #[test]
    fn visible_text_skips_scripts_and_styles() {
        let html = "<html><head><title>T</title><style>p{}</style></head><body>\
                    <p>One\n  two</p><script>var x;</script><noscript>n</noscript><p>three</p></body></html>";
        assert_eq!(visible_text(html), "One two three");
    }

    #[test]
    fn json_ld_keeps_every_block() {
        let html = r#"<html><head>
            <script type="application/ld+json">{"@type":"Event","name":"A"}</script>
            <script type="application/ld+json">{"@type":"Organization","name":"B"}</script>
            <script type="application/ld+json">{not json</script>
            </head><body></body></html>"#;
        let blocks = json_ld_blocks(html);
        assert_eq!(blocks.len(), 2);
        assert_eq!(blocks[1]["@type"], "Organization");
    }

    #[test]
    fn prices_read_like_a_page() {
        let gbp = |min: i64, max: i64| Price {
            is_free: false,
            min: Some(Decimal::from(min)),
            max: Some(Decimal::from(max)),
            currency: Some("GBP".into()),
        };
        assert_eq!(price_text(&gbp(5, 10)).as_deref(), Some("£5–£10"));
        assert_eq!(price_text(&gbp(5, 5)).as_deref(), Some("£5"));
        let eur = Price {
            currency: Some("EUR".into()),
            ..gbp(5, 5)
        };
        assert_eq!(price_text(&eur).as_deref(), Some("5 EUR"));
        let free = Price {
            is_free: true,
            ..Price::default()
        };
        assert_eq!(price_text(&free).as_deref(), Some("free"));
        assert_eq!(price_text(&Price::default()), None);
    }

    #[test]
    fn dates_are_london_and_all_day_is_a_date() {
        // 19:00 UTC in October is 20:00 BST.
        let mut e = run_event(
            "a",
            "A",
            Utc.with_ymd_and_hms(2026, 10, 12, 19, 0, 0).unwrap(),
        );
        assert_eq!(record_fields(&e.event)["starts_at"], "2026-10-12 20:00");
        e.event.starts_at = Utc.with_ymd_and_hms(2026, 10, 11, 23, 0, 0).unwrap();
        e.event.all_day = true;
        assert_eq!(record_fields(&e.event)["starts_at"], "2026-10-12");
    }

    #[test]
    fn oversized_input_is_cut_to_the_budget() {
        let ld: String = (0..50)
            .map(|i| {
                format!(
                    r#"<script type="application/ld+json">{{"@type":"Event","name":"Block {i} {}"}}</script>"#,
                    "x".repeat(200)
                )
            })
            .collect();
        let words: String = (0..40_000).map(|i| format!("w{i} ")).collect();
        let body = format!("<html><head>{ld}</head><body><p>{words}</p></body></html>");
        assert!(body.len() > 200_000);
        let start = Utc.with_ymd_and_hms(2026, 10, 12, 19, 0, 0).unwrap();
        let all: Vec<RunEvent> = (0..500)
            .map(|i| run_event(&format!("e{i}"), &format!("w{i}"), start))
            .collect();
        let pages = [
            PageIn {
                kind: PageKind::Listing,
                url: "https://venue.test/whats-on",
                body: &body,
                json: false,
            },
            PageIn {
                kind: PageKind::Detail,
                url: "https://venue.test/e0",
                body: &body,
                json: false,
            },
            PageIn {
                kind: PageKind::Detail,
                url: "https://venue.test/e1",
                body: &body,
                json: false,
            },
        ];
        let input = build(
            "fake",
            None,
            today(),
            &pages,
            &[(&all[0], 1), (&all[1], 2)],
            &all,
        );
        assert!(
            input.message.len() <= MAX_INPUT_CHARS,
            "{}",
            input.message.len()
        );
        let v: Value = serde_json::from_str(&input.message).unwrap();
        for p in v["pages"].as_array().unwrap() {
            assert!(p["text"].as_str().unwrap().ends_with("[…truncated]"));
            assert!(p["json_ld"].to_string().len() <= MAX_JSON_LD_CHARS);
        }
        assert_eq!(v["records"].as_array().unwrap().len(), 2);
        assert_eq!(v["records"][0]["title"], "w0");
        assert_eq!(v["records"][1]["page"], "p3");
        assert!(v["listing_records_not_shown"].as_u64().unwrap() > 0);
        assert_eq!(input.record_ids, ["r1", "r2"]);
        assert_eq!(
            input.listing_ids.len(),
            v["listing_records"].as_array().unwrap().len()
        );
        // Grounding covers what was sent, not what was cut.
        assert!(input.grounding_all.contains("w5 w6"));
        assert!(!input.grounding_all.contains("w39999"));
        assert!(input.grounding_listing.contains("block 0"));
    }

    #[test]
    fn missed_titles_that_match_a_record_are_ours() {
        let t = Utc.with_ymd_and_hms(2026, 10, 12, 19, 0, 0).unwrap();
        let all = [run_event("a", "The Big Show: Late", t)];
        assert!(is_run_record("Big Show", &all));
        assert!(is_run_record("The Big Show: Late (sold out)", &all));
        assert!(!is_run_record("Another thing", &all));
    }

    #[test]
    fn the_source_scope_reaches_the_judge_but_not_grounding() {
        let pages = [PageIn {
            kind: PageKind::Listing,
            url: "https://venue.test/whats-on",
            body: "<html><body><p>Music: A Gig. Talk: A Talk.</p></body></html>",
            json: false,
        }];
        let scope = "Only talks. Gigs are left out on purpose.";
        let input = build("fake", Some(scope), today(), &pages, &[], &[]);
        let v: Value = serde_json::from_str(&input.message).unwrap();
        assert_eq!(v["source_scope"], scope);
        assert!(!input.grounding_all.contains("left out on purpose"));

        let input = build("fake", None, today(), &pages, &[], &[]);
        let v: Value = serde_json::from_str(&input.message).unwrap();
        assert!(v.get("source_scope").is_none());
    }
}
