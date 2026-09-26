//! Prompt/model evaluation for AI enrichment, without a database.
//!
//! Reads events exported as JSON (the fields of `enrich::input::EventFacts`,
//! e.g. from a read-only `psql ... json_agg(...)` export), sends them in
//! batches through exactly the production request builder, validator and
//! retry rule, and prints one JSON object with every result, failure, token
//! count and cost.
//!
//! ```bash
//! REQUESTY_API_KEY=... cargo run --example enrich_eval -- events.json \
//!     anthropic/claude-opus-5-5 4 20 0.2 5 > out.json
//! #   args: <events.json> <model> <in $/M> <out $/M> <cache-read $/M> <cache-write $/M> [batch]
//! ```
//!
//! Prices are passed explicitly (from the Requesty catalogue) so the numbers
//! match what `events.model_prices` would charge. Add `EMBED=1` to also
//! embed each result with `openai/text-embedding-3-small` and print the
//! nearest neighbours of every event.

use std::time::Duration;

use musenmingle::enrich::input::EventFacts;
use musenmingle::enrich::output::{self, Enrichment};
use musenmingle::enrich::requesty::Requesty;
use musenmingle::enrich::store::ModelPrice;
use musenmingle::enrich::{EnrichConfig, call_cost, chat_body, embed};
use rust_decimal::Decimal;
use serde_json::{Value, json};

fn facts(v: &Value) -> EventFacts {
    let s = |k: &str| v.get(k).and_then(Value::as_str).map(str::to_string);
    let dec = |k: &str| {
        v.get(k)
            .and_then(|x| {
                x.as_f64()
                    .map(|f| f.to_string())
                    .or(x.as_str().map(str::to_string))
            })
            .and_then(|t| t.parse::<Decimal>().ok())
    };
    let strs = |k: &str| {
        v.get(k)
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(|x| x.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default()
    };
    let mut listed_by: Vec<String> = strs("sources");
    listed_by.sort();
    EventFacts {
        id: s("id").and_then(|i| i.parse().ok()).unwrap_or_default(),
        title: s("title").unwrap_or_default(),
        venue_name: s("venue_name"),
        starts_at: s("starts_at")
            .and_then(|t| t.parse().ok())
            .expect("starts_at"),
        ends_at: s("ends_at").and_then(|t| t.parse().ok()),
        category: s("category").unwrap_or_default(),
        tags: strs("tags"),
        is_free: v.get("is_free").and_then(Value::as_bool).unwrap_or(false),
        price_min: dec("price_min"),
        price_max: dec("price_max"),
        currency: s("currency"),
        description: s("description"),
        listed_by,
    }
}

fn cosine(a: &[f32], b: &[f32]) -> f64 {
    let (mut d, mut na, mut nb) = (0f64, 0f64, 0f64);
    for (x, y) in a.iter().zip(b) {
        d += f64::from(*x) * f64::from(*y);
        na += f64::from(*x) * f64::from(*x);
        nb += f64::from(*y) * f64::from(*y);
    }
    d / (na.sqrt() * nb.sqrt())
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let events: Vec<Value> = serde_json::from_str(&std::fs::read_to_string(&args[1])?)?;
    let model = args[2].clone();
    let p = |i: usize| args[i].parse::<Decimal>().expect("price");
    let price = ModelPrice {
        model: model.clone(),
        input_usd_per_mtok: p(3),
        output_usd_per_mtok: p(4),
        cache_read_usd_per_mtok: p(5),
        cache_write_usd_per_mtok: p(6),
        retention_days: None,
    };
    let batch_size: usize = args.get(7).map_or(10, |b| b.parse().expect("batch"));
    let key = std::env::var("REQUESTY_API_KEY")?;
    let client = Requesty::new(musenmingle::enrich::requesty::DEFAULT_BASE_URL, &key)?;
    let cfg = EnrichConfig {
        model: model.clone(),
        ..Default::default()
    };
    let all: Vec<EventFacts> = events.iter().map(facts).collect();
    let mut results: Vec<Option<Enrichment>> = vec![None; all.len()];
    let mut failures: Vec<Value> = Vec::new();
    let mut calls: Vec<Value> = Vec::new();
    let mut total = Decimal::ZERO;
    let chunks: Vec<Vec<usize>> = (0..all.len())
        .collect::<Vec<_>>()
        .chunks(batch_size)
        .map(<[usize]>::to_vec)
        .collect();
    for (k, idx) in chunks.iter().enumerate() {
        let mut todo = idx.clone();
        let mut reminder: Option<String> = None;
        for attempt in 0..2 {
            let ids: Vec<String> = (1..=todo.len()).map(|i| format!("e{i}")).collect();
            let pairs: Vec<(String, &EventFacts)> = ids
                .iter()
                .cloned()
                .zip(todo.iter().map(|i| &all[*i]))
                .collect();
            let body = chat_body(
                &cfg,
                &pairs,
                reminder.as_deref(),
                k + 1 < chunks.len() || attempt > 0,
            );
            let started = std::time::Instant::now();
            let c = client.chat(&body, Duration::from_secs(240)).await?;
            let cost = call_cost(&c.usage, &price);
            total += cost;
            calls.push(json!({
                "batch": k, "attempt": attempt, "events": todo.len(),
                "secs": started.elapsed().as_secs_f64(), "finish": c.finish_reason,
                "prompt_tokens": c.usage.prompt_tokens, "cached": c.usage.cached_tokens,
                "cache_write": c.usage.cache_write_tokens, "completion": c.usage.completion_tokens,
                "cost": cost.round_dp(6).to_string(), "provider_cost": c.usage.provider_cost_usd,
            }));
            let texts: Vec<String> = todo.iter().map(|i| all[*i].grounding_text()).collect();
            let out = output::parse_batch(&c.content, &ids, &texts);
            for (j, e) in out.ok {
                results[todo[j]] = Some(e);
            }
            if out.failed.is_empty() {
                break;
            }
            reminder = Some(
                out.failed
                    .iter()
                    .enumerate()
                    .map(|(n, (_, why))| format!("- e{}: {why}", n + 1))
                    .collect::<Vec<_>>()
                    .join("\n"),
            );
            for (j, why) in &out.failed {
                failures.push(
                    json!({ "title": all[todo[*j]].title, "attempt": attempt, "reason": why }),
                );
            }
            todo = out.failed.iter().map(|(j, _)| todo[*j]).collect();
        }
    }
    let mut neighbours = Value::Null;
    let mut embed_cost = Decimal::ZERO;
    if std::env::var("EMBED").is_ok_and(|v| v == "1") {
        let texts: Vec<String> = all
            .iter()
            .zip(&results)
            .map(|(f, e)| embed::embed_text(f, e.as_ref()))
            .collect();
        let (vecs, usage) = client
            .embed(
                "openai/text-embedding-3-small",
                &texts,
                Duration::from_secs(60),
            )
            .await?;
        embed_cost =
            Decimal::from(usage.prompt_tokens) * Decimal::new(2, 2) / Decimal::from(1_000_000);
        let mut rows = Vec::new();
        for (i, v) in vecs.iter().enumerate() {
            let mut sims: Vec<(f64, usize)> = vecs
                .iter()
                .enumerate()
                .filter(|(j, _)| *j != i)
                .map(|(j, w)| (cosine(v, w), j))
                .collect();
            sims.sort_by(|a, b| b.0.total_cmp(&a.0));
            rows.push(json!({
                "title": all[i].title,
                "nearest": sims.iter().take(3).map(|(s, j)| json!({"title": all[*j].title, "sim": s})).collect::<Vec<_>>(),
            }));
        }
        neighbours =
            json!({ "tokens": usage.prompt_tokens, "cost": embed_cost.to_string(), "rows": rows });
    }
    let out: Vec<Value> = all
        .iter()
        .zip(&results)
        .map(|(f, e)| {
            json!({ "title": f.title, "venue": f.venue_name, "listed_by": f.listed_by,
                               "has_excerpt": f.description.is_some(), "excerpt": f.description,
                               "result": e })
        })
        .collect();
    println!(
        "{}",
        serde_json::to_string_pretty(&json!({
            "model": model, "batch_size": batch_size, "events": all.len(),
            "enriched": results.iter().filter(|r| r.is_some()).count(),
            "total_cost": total.round_dp(6).to_string(),
            "cost_per_event": (total / Decimal::from(all.len().max(1))).round_dp(6).to_string(),
            "calls": calls, "failures": failures, "results": out, "embeddings": neighbours,
            "embed_cost": embed_cost.to_string(),
        }))?
    );
    Ok(())
}
