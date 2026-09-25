//! Helpers for schema.org JSON-LD `Event` markup (the preferred way to
//! scrape a site: structured, stable and intended for machines).

use scraper::{Html, Selector};
use serde_json::Value;

/// Extract every schema.org node whose `@type` is `Event` or a subtype
/// ending in `Event` (e.g. `ExhibitionEvent`, `TheaterEvent`) from all
/// `<script type="application/ld+json">` blocks. Invalid or `null` blocks
/// are ignored. Handles top-level arrays and `@graph`.
pub fn extract_events(html: &Html) -> Vec<Value> {
    let sel = Selector::parse(r#"script[type="application/ld+json"]"#).expect("valid selector");
    let mut out = Vec::new();
    for script in html.select(&sel) {
        let text: String = script.text().collect();
        let Ok(value) = serde_json::from_str::<Value>(text.trim()) else {
            continue;
        };
        collect(&value, &mut out);
    }
    out
}

fn is_event_type(t: &Value) -> bool {
    match t {
        Value::String(s) => s.ends_with("Event"),
        Value::Array(a) => a.iter().any(is_event_type),
        _ => false,
    }
}

fn collect(v: &Value, out: &mut Vec<Value>) {
    match v {
        Value::Array(items) => items.iter().for_each(|i| collect(i, out)),
        Value::Object(map) => {
            if map.get("@type").is_some_and(is_event_type) {
                out.push(v.clone());
            }
            if let Some(g) = map.get("@graph") {
                collect(g, out);
            }
        }
        _ => {}
    }
}

/// A string field, or the `name` of an object field (schema.org allows both).
pub fn str_or_name<'a>(v: &'a Value, key: &str) -> Option<&'a str> {
    match v.get(key)? {
        Value::String(s) => Some(s.as_str()),
        Value::Object(o) => o.get("name").and_then(Value::as_str),
        Value::Array(a) => a.first().and_then(|f| match f {
            Value::String(s) => Some(s.as_str()),
            Value::Object(o) => o.get("name").and_then(Value::as_str),
            _ => None,
        }),
        _ => None,
    }
}

/// The first image URL (`image` may be a string, an ImageObject or an array).
pub fn image_url(v: &Value) -> Option<String> {
    match v.get("image")? {
        Value::String(s) => Some(s.clone()),
        Value::Object(o) => o.get("url").and_then(Value::as_str).map(str::to_string),
        Value::Array(a) => a.iter().find_map(|i| match i {
            Value::String(s) => Some(s.clone()),
            Value::Object(o) => o.get("url").and_then(Value::as_str).map(str::to_string),
            _ => None,
        }),
        _ => None,
    }
}

/// The first `Offer` (offers may be an object or an array).
pub fn first_offer(v: &Value) -> Option<&Value> {
    match v.get("offers")? {
        Value::Array(a) => a.first(),
        o @ Value::Object(_) => Some(o),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_events_from_graph_arrays_and_ignores_null() {
        let html = Html::parse_document(
            r#"<html><head>
            <script type="application/ld+json">null</script>
            <script type="application/ld+json">{not json</script>
            <script type="application/ld+json">{"@graph":[{"@type":"WebPage"},{"@type":"Event","name":"A"}]}</script>
            <script type="application/ld+json">[{"@type":"ExhibitionEvent","name":"B"},{"@type":["Thing","MusicEvent"],"name":"C"}]</script>
            </head></html>"#,
        );
        let names: Vec<_> = extract_events(&html)
            .iter()
            .map(|e| e["name"].as_str().unwrap().to_string())
            .collect();
        assert_eq!(names, ["A", "B", "C"]);
    }

    #[test]
    fn field_helpers() {
        let v: Value = serde_json::json!({
            "location": {"@type":"Place","name":"Hall"},
            "image": [{"url":"https://i/1.jpg"}],
            "offers": [{"price":"5"}]
        });
        assert_eq!(str_or_name(&v, "location"), Some("Hall"));
        assert_eq!(image_url(&v).as_deref(), Some("https://i/1.jpg"));
        assert_eq!(first_offer(&v).unwrap()["price"], "5");
    }
}
