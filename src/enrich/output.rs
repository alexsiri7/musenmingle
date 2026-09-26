//! The enrichment output contract: fixed vocabularies, the JSON schema sent
//! as `response_format`, and strict validation of what the model returns.
//!
//! Nothing the model says is trusted: every result is checked against the
//! vocabularies, the length limits, the grounding rules and the input text
//! (artists and opening evidence must appear verbatim in what we sent).

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

/// Bump when the prompt, schema or validation changes meaningfully: every
/// event is then re-enriched (gradually, within the spend caps).
pub const PROMPT_VERSION: i32 = 1;

/// The instructions (static, so the provider can cache them).
pub const SYSTEM_PROMPT: &str = include_str!("prompt.txt");

pub const MEDIUM_TAGS: &[&str] = &[
    "photography",
    "painting",
    "drawing",
    "sculpture",
    "installation",
    "design",
    "architecture",
    "illustration",
    "textiles_craft",
    "ceramics",
    "film_video",
    "performance",
    "sound_music",
    "writing_poetry",
    "digital_new_media",
    "printmaking",
];
pub const FORMAT_TAGS: &[&str] = &[
    "hands_on",
    "talk",
    "social",
    "opening",
    "late",
    "family_friendly",
    "course",
    "tour",
    "screening",
    "fair_market",
];
pub const GOOD_FOR: &[&str] = &[
    "solo",
    "date",
    "friends",
    "kids",
    "first_timers",
    "deep_dive",
];
pub const VIBE_TAGS: &[&str] = &[
    "contemplative",
    "playful",
    "provocative",
    "immersive",
    "lively",
    "intimate",
    "experimental",
    "crafty",
];
pub const GROUNDING: &[&str] = &["listing", "listing_plus_general_knowledge", "insufficient"];

pub const MAX_MEDIUM: usize = 3;
pub const MAX_FORMAT: usize = 3;
pub const MAX_GOOD_FOR: usize = 3;
pub const MAX_VIBE: usize = 2;
pub const MAX_ARTISTS: usize = 6;
pub const MAX_WHATS_COOL_CHARS: usize = 220;
pub const MAX_ONE_LINER_CHARS: usize = 90;
pub const MAX_EVIDENCE_CHARS: usize = 120;
pub const MAX_ARTIST_CHARS: usize = 80;

/// Marketing words the notes must not use (matched case-insensitively on
/// word boundaries).
pub const HYPE_WORDS: &[&str] = &[
    "stunning",
    "must-see",
    "must see",
    "unmissable",
    "breathtaking",
    "spectacular",
    "unforgettable",
    "world-class",
    "iconic",
    "amazing",
    "incredible",
    "mesmerising",
    "mesmerizing",
    "jaw-dropping",
    "don't miss",
    "not to be missed",
];

/// Human label for a vocabulary tag ("textiles_craft" -> "textiles & craft").
pub fn label(tag: &str) -> &str {
    match tag {
        "textiles_craft" => "textiles & craft",
        "film_video" => "film & video",
        "sound_music" => "sound & music",
        "writing_poetry" => "writing & poetry",
        "digital_new_media" => "digital & new media",
        "hands_on" => "hands-on",
        "family_friendly" => "family-friendly",
        "fair_market" => "fair or market",
        "first_timers" => "first-timers",
        "deep_dive" => "a deep dive",
        other => other,
    }
}

/// The field names every result must carry, exactly.
const FIELDS: &[&str] = &[
    "id",
    "medium_tags",
    "format_tags",
    "good_for",
    "vibe_tags",
    "artists",
    "is_opening",
    "opening_evidence",
    "grounding",
    "whats_cool",
    "one_liner",
    "confidence",
];

/// A validated enrichment of one event (stored as `events.enrichments.output`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Enrichment {
    pub medium_tags: Vec<String>,
    pub format_tags: Vec<String>,
    pub good_for: Vec<String>,
    pub vibe_tags: Vec<String>,
    pub artists: Vec<String>,
    pub is_opening: bool,
    pub opening_evidence: Option<String>,
    pub grounding: String,
    pub whats_cool: Option<String>,
    pub one_liner: Option<String>,
    pub confidence: f64,
}

#[derive(Deserialize)]
struct RawResult {
    id: String,
    medium_tags: Vec<String>,
    format_tags: Vec<String>,
    good_for: Vec<String>,
    vibe_tags: Vec<String>,
    artists: Vec<String>,
    is_opening: bool,
    opening_evidence: Option<String>,
    grounding: String,
    whats_cool: Option<String>,
    one_liner: Option<String>,
    confidence: f64,
}

/// The `response_format` for chat completions: `{"results": [...]}` with
/// every field required and tags constrained to the vocabularies. (Length
/// limits are enforced by [`validate`], not all providers support them.)
pub fn response_format() -> Value {
    let tags = |vocab: &[&str], max: usize| json!({ "type": "array", "items": { "type": "string", "enum": vocab }, "maxItems": max });
    let nullable = json!({ "type": ["string", "null"] });
    let item = json!({
        "type": "object",
        "additionalProperties": false,
        "required": FIELDS,
        "properties": {
            "id": { "type": "string" },
            "medium_tags": tags(MEDIUM_TAGS, MAX_MEDIUM),
            "format_tags": tags(FORMAT_TAGS, MAX_FORMAT),
            "good_for": tags(GOOD_FOR, MAX_GOOD_FOR),
            "vibe_tags": tags(VIBE_TAGS, MAX_VIBE),
            "artists": { "type": "array", "items": { "type": "string" }, "maxItems": MAX_ARTISTS },
            "is_opening": { "type": "boolean" },
            "opening_evidence": nullable,
            "grounding": { "type": "string", "enum": GROUNDING },
            "whats_cool": nullable,
            "one_liner": nullable,
            "confidence": { "type": "number" },
        }
    });
    json!({
        "type": "json_schema",
        "json_schema": {
            "name": "event_enrichment",
            "strict": true,
            "schema": {
                "type": "object",
                "additionalProperties": false,
                "required": ["results"],
                "properties": { "results": { "type": "array", "items": item } }
            }
        }
    })
}

/// Lower-case, fold typographic quotes/dashes, collapse whitespace.
pub fn normalise_for_match(s: &str) -> String {
    let mapped: String = s
        .chars()
        .map(|c| match c {
            '\u{2018}' | '\u{2019}' | '\u{02bc}' => '\'',
            '\u{201c}' | '\u{201d}' => '"',
            '\u{2013}' | '\u{2014}' => '-',
            c if c.is_whitespace() => ' ',
            c => c,
        })
        .collect::<String>()
        .to_lowercase();
    mapped.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Whether `text` uses a hype word (word-boundary, case-insensitive).
pub fn hype_word(text: &str) -> Option<&'static str> {
    let words: String = normalise_for_match(text)
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '-' || c == '\'' {
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
    HYPE_WORDS
        .iter()
        .copied()
        .find(|w| padded.contains(&format!(" {w} ")))
}

fn check_tags(name: &str, tags: &[String], vocab: &[&str], max: usize) -> Result<(), String> {
    if tags.len() > max {
        return Err(format!("{name}: at most {max} tags, got {}", tags.len()));
    }
    for (i, t) in tags.iter().enumerate() {
        if !vocab.contains(&t.as_str()) {
            return Err(format!("{name}: {t:?} is not in the vocabulary"));
        }
        if tags[..i].contains(t) {
            return Err(format!("{name}: duplicate {t:?}"));
        }
    }
    Ok(())
}

fn check_text(name: &str, text: &str, max_chars: usize) -> Result<(), String> {
    if text.trim().is_empty() {
        return Err(format!("{name}: empty (use null)"));
    }
    let n = text.chars().count();
    if n > max_chars {
        return Err(format!("{name}: {n} characters, max {max_chars}"));
    }
    if let Some(w) = hype_word(text) {
        return Err(format!("{name}: uses the hype word {w:?}"));
    }
    if text.contains(['{', '}', '[', ']', '<', '>']) {
        return Err(format!("{name}: contains stray markup characters"));
    }
    Ok(())
}

/// Validate one result against its event's input text (the facts we sent,
/// see [`crate::enrich::input::EventFacts::grounding_text`]).
pub fn validate(value: &Value, input_text: &str) -> Result<(String, Enrichment), String> {
    let obj = value.as_object().ok_or("result is not an object")?;
    for f in FIELDS {
        if !obj.contains_key(*f) {
            return Err(format!("missing field {f:?}"));
        }
    }
    if let Some(extra) = obj.keys().find(|k| !FIELDS.contains(&k.as_str())) {
        return Err(format!("unexpected field {extra:?}"));
    }
    let raw: RawResult =
        serde_json::from_value(value.clone()).map_err(|e| format!("wrong field type: {e}"))?;
    let id = raw.id.clone();
    let e = Enrichment {
        medium_tags: raw.medium_tags,
        format_tags: raw.format_tags,
        good_for: raw.good_for,
        vibe_tags: raw.vibe_tags,
        artists: raw
            .artists
            .into_iter()
            .map(|a| a.trim().to_string())
            .collect(),
        is_opening: raw.is_opening,
        opening_evidence: raw.opening_evidence.map(|s| s.trim().to_string()),
        grounding: raw.grounding,
        whats_cool: raw.whats_cool.map(|s| s.trim().to_string()),
        one_liner: raw.one_liner.map(|s| s.trim().to_string()),
        confidence: raw.confidence,
    };
    check_tags("medium_tags", &e.medium_tags, MEDIUM_TAGS, MAX_MEDIUM)?;
    check_tags("format_tags", &e.format_tags, FORMAT_TAGS, MAX_FORMAT)?;
    check_tags("good_for", &e.good_for, GOOD_FOR, MAX_GOOD_FOR)?;
    check_tags("vibe_tags", &e.vibe_tags, VIBE_TAGS, MAX_VIBE)?;
    if !GROUNDING.contains(&e.grounding.as_str()) {
        return Err(format!("grounding: {:?} is not allowed", e.grounding));
    }
    if !(e.confidence.is_finite() && (0.0..=1.0).contains(&e.confidence)) {
        return Err(format!(
            "confidence {} is not between 0 and 1",
            e.confidence
        ));
    }

    let haystack = normalise_for_match(input_text);
    if e.artists.len() > MAX_ARTISTS {
        return Err(format!("artists: at most {MAX_ARTISTS}"));
    }
    for (i, a) in e.artists.iter().enumerate() {
        if a.is_empty() || a.chars().count() > MAX_ARTIST_CHARS {
            return Err("artists: empty or over-long name".into());
        }
        if !haystack.contains(&normalise_for_match(a)) {
            return Err(format!("artists: {a:?} does not appear in the input"));
        }
        if e.artists[..i].contains(a) {
            return Err(format!("artists: duplicate {a:?}"));
        }
    }
    match (e.is_opening, e.opening_evidence.as_deref()) {
        (true, Some(ev)) => {
            if ev.is_empty() || ev.chars().count() > MAX_EVIDENCE_CHARS {
                return Err("opening_evidence: empty or over-long".into());
            }
            if !haystack.contains(&normalise_for_match(ev)) {
                return Err(format!(
                    "opening_evidence: {ev:?} is not quoted from the input"
                ));
            }
        }
        (true, None) => return Err("is_opening is true without opening_evidence".into()),
        (false, Some(_)) => {
            return Err("opening_evidence must be null when is_opening is false".into());
        }
        (false, None) => {}
    }

    if e.grounding == "insufficient" {
        if e.whats_cool.is_some() || e.one_liner.is_some() {
            return Err(
                "grounding is insufficient: whats_cool and one_liner must both be null".into(),
            );
        }
    } else {
        let Some(w) = e.whats_cool.as_deref() else {
            return Err("whats_cool is null but grounding is not insufficient".into());
        };
        check_text("whats_cool", w, MAX_WHATS_COOL_CHARS)?;
        if !w.ends_with(['.', '!', '?', ')', '\u{2019}', '\u{201d}', '"', '\'']) {
            return Err("whats_cool: must end with a full sentence".into());
        }
    }
    if let Some(o) = e.one_liner.as_deref() {
        check_text("one_liner", o, MAX_ONE_LINER_CHARS)?;
    }
    Ok((id, e))
}

/// Outcome of one batch response: validated results and failures, both by
/// input index.
#[derive(Debug, Default)]
pub struct BatchOutcome {
    pub ok: Vec<(usize, Enrichment)>,
    pub failed: Vec<(usize, String)>,
}

/// Map a batch response (`{"results": [...]}`) back to the inputs by id.
/// `ids[i]` is the batch id sent for input `i`, `texts[i]` its grounding
/// text. Missing, duplicated and invalid results fail their input; results
/// with unknown ids are ignored. An unparsable response fails every input.
pub fn parse_batch(content: &str, ids: &[String], texts: &[String]) -> BatchOutcome {
    let mut out = BatchOutcome::default();
    let fail_all = |reason: String| BatchOutcome {
        ok: Vec::new(),
        failed: (0..ids.len()).map(|i| (i, reason.clone())).collect(),
    };
    // JSON mode may still wrap the object in a Markdown fence.
    let body = content.trim();
    let body = body
        .strip_prefix("```json")
        .or_else(|| body.strip_prefix("```"))
        .and_then(|b| b.strip_suffix("```"))
        .unwrap_or(body);
    let parsed: Value = match serde_json::from_str(body.trim()) {
        Ok(v) => v,
        Err(e) => return fail_all(format!("response is not JSON: {e}")),
    };
    let Some(results) = parsed.get("results").and_then(Value::as_array) else {
        return fail_all("response has no results array".into());
    };
    let mut seen: Vec<Option<Result<Enrichment, String>>> = vec![None; ids.len()];
    for r in results {
        let Some(id) = r.get("id").and_then(Value::as_str) else {
            continue;
        };
        let Some(i) = ids.iter().position(|x| x == id) else {
            tracing::warn!(id, "enrichment result for an unknown id ignored");
            continue;
        };
        seen[i] = Some(match seen[i] {
            Some(_) => Err(format!("duplicate result for id {id}")),
            None => validate(r, &texts[i]).map(|(_, e)| e),
        });
    }
    for (i, s) in seen.into_iter().enumerate() {
        match s {
            None => out.failed.push((i, "no result for this id".into())),
            Some(Ok(e)) => out.ok.push((i, e)),
            Some(Err(reason)) => out.failed.push((i, reason)),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const INPUT: &str =
        "Magdalene Odundo | Barbican | Private view: Thursday 6pm | Hand-built vessels";

    fn good() -> Value {
        json!({
            "id": "e1",
            "medium_tags": ["ceramics", "sculpture"],
            "format_tags": ["opening"],
            "good_for": ["solo"],
            "vibe_tags": ["contemplative"],
            "artists": ["Magdalene Odundo"],
            "is_opening": true,
            "opening_evidence": "Private view",
            "grounding": "listing",
            "whats_cool": "Coiled, burnished vessels from a maker who has spent decades refining a few forms.",
            "one_liner": "Hand-built ceramic vessels by Magdalene Odundo",
            "confidence": 0.9
        })
    }

    fn with(k: &str, v: Value) -> Value {
        let mut g = good();
        g[k] = v;
        g
    }

    #[test]
    fn accepts_a_good_result() {
        let (id, e) = validate(&good(), INPUT).unwrap();
        assert_eq!(id, "e1");
        assert_eq!(e.medium_tags, ["ceramics", "sculpture"]);
        assert!(e.is_opening);
    }

    #[test]
    fn rejects_off_vocabulary_and_too_many_tags() {
        assert!(validate(&with("medium_tags", json!(["pottery"])), INPUT).is_err());
        assert!(validate(&with("format_tags", json!(["party"])), INPUT).is_err());
        assert!(validate(&with("good_for", json!(["everyone"])), INPUT).is_err());
        assert!(validate(&with("vibe_tags", json!(["cosy"])), INPUT).is_err());
        assert!(
            validate(
                &with(
                    "medium_tags",
                    json!(["painting", "drawing", "sculpture", "design"])
                ),
                INPUT
            )
            .is_err()
        );
        assert!(
            validate(
                &with("vibe_tags", json!(["playful", "lively", "intimate"])),
                INPUT
            )
            .is_err()
        );
        assert!(validate(&with("medium_tags", json!(["ceramics", "ceramics"])), INPUT).is_err());
        assert!(validate(&with("grounding", json!("vibes")), INPUT).is_err());
    }

    #[test]
    fn rejects_over_long_text_counting_characters() {
        let long: String = "é".repeat(215) + " end.";
        assert_eq!(long.chars().count(), 220);
        assert!(validate(&with("whats_cool", json!(long)), INPUT).is_ok());
        let longer = "é".repeat(216) + " end.";
        assert!(validate(&with("whats_cool", json!(longer)), INPUT).is_err());
        assert!(validate(&with("one_liner", json!("x".repeat(91))), INPUT).is_err());
        assert!(validate(&with("one_liner", json!("x".repeat(90))), INPUT).is_ok());
    }

    #[test]
    fn insufficient_grounding_means_no_notes() {
        let mut v = with("grounding", json!("insufficient"));
        assert!(validate(&v, INPUT).is_err(), "whats_cool present");
        v["whats_cool"] = Value::Null;
        assert!(validate(&v, INPUT).is_err(), "one_liner present");
        v["one_liner"] = Value::Null;
        assert!(validate(&v, INPUT).is_ok());
        let v = with("whats_cool", Value::Null);
        assert!(
            validate(&v, INPUT).is_err(),
            "listing grounding needs a note"
        );
    }

    #[test]
    fn rejects_hype_words_on_word_boundaries() {
        assert!(validate(&with("whats_cool", json!("A Stunning show.")), INPUT).is_err());
        assert!(validate(&with("whats_cool", json!("Truly a must-see.")), INPUT).is_err());
        assert!(validate(&with("one_liner", json!("Unmissable pots")), INPUT).is_err());
        // "iconically" is not "iconic"; "amazingly"-style substrings don't trip.
        assert!(hype_word("iconically plain").is_none());
        assert_eq!(hype_word("Don\u{2019}t miss it"), Some("don't miss"));
    }

    #[test]
    fn artists_and_evidence_must_come_from_the_input() {
        assert!(validate(&with("artists", json!(["Grayson Perry"])), INPUT).is_err());
        assert!(validate(&with("artists", json!(["magdalene  odundo"])), INPUT).is_ok());
        assert!(validate(&with("opening_evidence", json!("Launch party")), INPUT).is_err());
        let v = with("is_opening", json!(false));
        assert!(validate(&v, INPUT).is_err(), "evidence without opening");
        let mut v = v;
        v["opening_evidence"] = Value::Null;
        assert!(validate(&v, INPUT).is_ok());
        assert!(validate(&with("opening_evidence", Value::Null), INPUT).is_err());
    }

    #[test]
    fn rejects_missing_extra_and_mistyped_fields() {
        let mut v = good();
        v.as_object_mut().unwrap().remove("one_liner");
        assert!(
            validate(&v, INPUT).is_err(),
            "a missing nullable field is still an error"
        );
        assert!(validate(&with("colour", json!("red")), INPUT).is_err());
        assert!(validate(&with("confidence", json!(1.5)), INPUT).is_err());
        assert!(validate(&with("confidence", json!("high")), INPUT).is_err());
        assert!(validate(&with("whats_cool", json!("Ends badly.}")), INPUT).is_err());
        assert!(validate(&with("whats_cool", json!("No full stop")), INPUT).is_err());
    }

    #[test]
    fn batches_map_back_by_id() {
        let ids = vec!["e1".to_string(), "e2".to_string(), "e3".to_string()];
        let texts = vec![INPUT.to_string(); 3];
        let r2 = with("id", json!("e2"));
        let r1 = good();
        let bad3 = {
            let mut v = with("id", json!("e3"));
            v["medium_tags"] = json!(["pottery"]);
            v
        };
        let unknown = with("id", json!("e9"));
        let content = json!({ "results": [r2, unknown, r1, bad3] }).to_string();
        let out = parse_batch(&content, &ids, &texts);
        let ok: Vec<usize> = out.ok.iter().map(|(i, _)| *i).collect();
        assert_eq!(ok, [0, 1]);
        assert_eq!(out.failed.len(), 1);
        assert_eq!(out.failed[0].0, 2);

        // Missing and duplicated ids fail those inputs only.
        let content = json!({ "results": [good(), good()] }).to_string();
        let out = parse_batch(&content, &ids, &texts);
        assert!(out.ok.is_empty());
        assert_eq!(out.failed.len(), 3);

        let out = parse_batch("{\"results\": [", &ids, &texts);
        assert_eq!(out.failed.len(), 3);
    }

    #[test]
    fn schema_lists_every_field_and_vocabulary() {
        let f = response_format();
        let item = &f["json_schema"]["schema"]["properties"]["results"]["items"];
        assert_eq!(item["required"].as_array().unwrap().len(), FIELDS.len());
        assert_eq!(
            item["properties"]["medium_tags"]["items"]["enum"]
                .as_array()
                .unwrap()
                .len(),
            MEDIUM_TAGS.len()
        );
    }
}
