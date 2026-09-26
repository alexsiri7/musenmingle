//! AI enrichment and embeddings, run by the ingest job after the sources.
//!
//! Scrapers never use a language model: extraction stays deterministic. This
//! pass runs afterwards, only on data we already store (see [`input`]): it
//! asks a model (via Requesty, [`requesty`]) for tags from fixed
//! vocabularies and a short "What's cool" note (see [`output`]), validates
//! the answer strictly, and stores it in `events.enrichments`, materialised
//! onto `events.events` for filters and pages. The pages label the note as
//! AI-written. Then each enriched event gets an embedding ([`embed`]).
//!
//! Cost control: events are sent in batches behind one long static system
//! prompt (Requesty `auto_cache` when a run makes several calls), every call
//! is recorded in `events.enrichment_calls` with its cost from the
//! catalogue prices in `events.model_prices`, and a call is only made when
//! its pessimistic estimate fits under both the daily cap (London day) and
//! the per-run cap. An event is re-enriched only when its input hash or the
//! prompt version changes; a give-up is remembered per input hash.
//!
//! When Requesty reports that credits are exhausted, the pass stops for the
//! run, `events.alert_state` remembers it and the owner gets one ntfy per
//! day until a later call succeeds (then one "OK again" message).

pub mod embed;
pub mod input;
pub mod output;
pub mod requesty;
pub mod store;

use std::collections::HashMap;
use std::time::{Duration, Instant};

use chrono::{DateTime, NaiveTime, Utc};
use chrono_tz::Europe::London;
use rust_decimal::Decimal;
use rust_decimal::prelude::{FromPrimitive, ToPrimitive};
use serde_json::{Value, json};
use sqlx::PgPool;

use crate::notify::Notifier;
use input::EventFacts;
use output::{BatchOutcome, Enrichment, PROMPT_VERSION, SYSTEM_PROMPT};
use requesty::{CallError, Requesty, Usage};
use store::{Candidate, ModelPrice};

pub const DEFAULT_MODEL: &str = "anthropic/claude-opus-5-5";
pub const DEFAULT_EMBED_MODEL: &str = "openai/text-embedding-3-small";
/// `events.alert_state` key for exhausted Requesty credits.
pub const CREDITS_ALERT: &str = "requesty_credits";
pub const CREDITS_TITLE: &str = "Muse & Mingle: Requesty credits exhausted";
pub const CREDITS_OK_TITLE: &str = "Muse & Mingle: Requesty credits OK again";
/// Events re-checked this long after they ended (matches the ingest grace).
const PAST_GRACE: chrono::Duration = chrono::Duration::days(1);

/// How the answer's JSON is requested. Either way [`output::validate`]
/// checks every field; the schema is also spelled out in the prompt.
///
/// Evaluated 2026-09-26 on 10-event batches with `anthropic/claude-opus-5-5`
/// through Requesty: strict `json_schema` came back as a degenerate
/// one-item placeholder (`"id": "__skip__"`) in 4 of 10 calls (with and
/// without `reasoning_effort`), while `json_object` with `reasoning_effort:
/// low` returned all 10 results in 6 of 6 calls using ~1,600 output tokens
/// instead of ~3,200.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutputMode {
    /// `response_format: {"type": "json_object"}`.
    JsonObject,
    /// `response_format: {"type": "json_schema", ...}` (strict).
    JsonSchema,
}

/// Enrichment settings (`ENRICH_*`, `EMBED_MODEL`; see README).
#[derive(Debug, Clone, PartialEq)]
pub struct EnrichConfig {
    pub model: String,
    /// `None` = embeddings off.
    pub embed_model: Option<String>,
    pub daily_cap_usd: Decimal,
    pub run_cap_usd: Decimal,
    pub batch_size: usize,
    pub max_events_per_run: usize,
    pub embed_batch_size: usize,
    pub call_timeout: Duration,
    /// Wall-clock budget for the whole pass (keeps the ingest lock short).
    pub run_budget: Duration,
    pub output_mode: OutputMode,
    /// `reasoning_effort` sent with chat calls (`None` = provider default).
    pub reasoning_effort: Option<String>,
    pub temperature: f64,
}

impl Default for EnrichConfig {
    fn default() -> Self {
        Self {
            model: DEFAULT_MODEL.into(),
            embed_model: Some(DEFAULT_EMBED_MODEL.into()),
            daily_cap_usd: Decimal::ONE,
            run_cap_usd: Decimal::new(40, 2),
            batch_size: 10,
            max_events_per_run: 120,
            embed_batch_size: 100,
            call_timeout: Duration::from_secs(150),
            run_budget: Duration::from_secs(300),
            // See `OutputMode`: JSON mode with low reasoning was reliable and
            // about half the output tokens of strict json_schema.
            output_mode: OutputMode::JsonObject,
            reasoning_effort: Some("low".into()),
            temperature: 0.2,
        }
    }
}

/// What one pass did (logged by the runner).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct EnrichReport {
    pub queued: usize,
    pub enriched: usize,
    pub failed: usize,
    pub calls: usize,
    pub tokens_in: i64,
    pub tokens_out: i64,
    pub cost_usd: Decimal,
    pub embedded: usize,
    pub spent_today_usd: Decimal,
    /// Why the pass stopped early, if it did.
    pub stopped: Option<String>,
}

/// What [`sync`] changed.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SyncReport {
    pub default_tags_applied: u64,
    pub stale_cleared: usize,
}

/// Bookkeeping that runs every tick, with or without an API key: sources'
/// default medium tags on events without AI fields, and removal of AI fields
/// whose input changed (so a note never outlives the facts it was written
/// from, e.g. after a description is removed on a venue's request).
pub async fn sync(pool: &PgPool, now: DateTime<Utc>) -> sqlx::Result<SyncReport> {
    let mut r = SyncReport {
        default_tags_applied: store::apply_default_tags(pool).await?,
        ..Default::default()
    };
    for c in store::candidates(pool, now - PAST_GRACE).await? {
        if c.materialised && c.enriched_hash.as_deref() != Some(&c.facts.input_hash()) {
            store::clear_materialised(pool, c.facts.id).await?;
            r.stale_cleared += 1;
        }
    }
    Ok(r)
}

/// Start of the London day containing `now`.
pub fn london_midnight(now: DateTime<Utc>) -> DateTime<Utc> {
    crate::normalise::london_to_utc(
        now.with_timezone(&London)
            .date_naive()
            .and_time(NaiveTime::MIN),
    )
}

fn per_mtok(tokens: i64, price: Decimal) -> Decimal {
    Decimal::from(tokens.max(0)) * price / Decimal::from(1_000_000)
}

/// Cost of a call from its usage and the catalogue prices.
pub fn call_cost(u: &Usage, p: &ModelPrice) -> Decimal {
    let uncached = u.prompt_tokens - u.cached_tokens - u.cache_write_tokens;
    per_mtok(uncached, p.input_usd_per_mtok)
        + per_mtok(u.cached_tokens, p.cache_read_usd_per_mtok)
        + per_mtok(u.cache_write_tokens, p.cache_write_usd_per_mtok)
        + per_mtok(u.completion_tokens, p.output_usd_per_mtok)
}

/// Output tokens allowed for a batch of `n`: ~150 per result plus the
/// model's own reasoning (observed ~1,500-1,700 in total for 10 events).
pub fn max_tokens(n: usize) -> i64 {
    300 * n as i64 + 2000
}

/// A pessimistic cost estimate: every prompt token at the dearer of the
/// input and cache-write prices (~3 characters per token), and the whole
/// output budget used.
pub fn estimate_cost(prompt_chars: usize, max_out: i64, p: &ModelPrice) -> Decimal {
    let tokens_in = (prompt_chars / 3 + 1) as i64;
    per_mtok(
        tokens_in,
        p.input_usd_per_mtok.max(p.cache_write_usd_per_mtok),
    ) + per_mtok(max_out, p.output_usd_per_mtok)
}

/// The chat request for a batch. `reminder` (the retry) lists what was
/// wrong with the previous answer.
pub fn chat_body(
    cfg: &EnrichConfig,
    batch: &[(String, &EventFacts)],
    reminder: Option<&str>,
    cache: bool,
) -> Value {
    let events: Vec<_> = batch.iter().map(|(id, f)| f.prompt_event(id)).collect();
    let mut messages = vec![
        json!({ "role": "system", "content": SYSTEM_PROMPT }),
        json!({ "role": "user", "content": json!({ "events": events }).to_string() }),
    ];
    if let Some(r) = reminder {
        messages.push(json!({ "role": "user", "content": format!(
            "Your previous answer for these events was rejected by our validator:\n{r}\n\
             Answer again for ALL the events above. Follow every rule exactly: only the listed \
             vocabulary values, at most 3 medium/format/good_for tags and 2 vibe tags, whats_cool \
             at most 220 characters ending with a full stop and one_liner at most 90, no hype \
             words, artists and opening_evidence copied verbatim from the input, and whats_cool \
             and one_liner both null when grounding is \"insufficient\"."
        ) }));
    }
    let mut body = json!({
        "model": cfg.model,
        "messages": messages,
        "response_format": match cfg.output_mode {
            OutputMode::JsonObject => json!({ "type": "json_object" }),
            OutputMode::JsonSchema => output::response_format(),
        },
        "max_tokens": max_tokens(batch.len()),
        "temperature": cfg.temperature,
        "requesty": { "auto_cache": cache },
    });
    if let Some(effort) = &cfg.reasoning_effort {
        body["reasoning_effort"] = json!(effort);
    }
    body
}

fn needs_enrichment(c: &Candidate, hash: &str) -> bool {
    let current =
        c.enriched_hash.as_deref() == Some(hash) && c.enriched_version == Some(PROMPT_VERSION);
    let gave_up =
        c.failed_hash.as_deref() == Some(hash) && c.failed_version == Some(PROMPT_VERSION);
    !current && !gave_up
}

/// The enrichment pass with its client and notifier.
pub struct Enricher {
    pub client: Requesty,
    pub notifier: Box<dyn Notifier>,
    pub config: EnrichConfig,
}

/// A spend check before a call.
enum Budget {
    Ok,
    Stop(String),
}

struct Pass<'a> {
    e: &'a Enricher,
    pool: &'a PgPool,
    now: DateTime<Utc>,
    deadline: Instant,
    spent_before: Decimal,
    report: EnrichReport,
}

impl Pass<'_> {
    fn budget(&self, estimate: Decimal) -> Budget {
        let cfg = &self.e.config;
        if Instant::now() >= self.deadline {
            return Budget::Stop("time budget used".into());
        }
        if self.spent_before + self.report.cost_usd + estimate > cfg.daily_cap_usd {
            return Budget::Stop(format!(
                "daily cap ${} reached (spent ${} today)",
                cfg.daily_cap_usd,
                (self.spent_before + self.report.cost_usd).round_dp(4)
            ));
        }
        if self.report.cost_usd + estimate > cfg.run_cap_usd {
            return Budget::Stop(format!("run cap ${} reached", cfg.run_cap_usd));
        }
        Budget::Ok
    }

    async fn ledger(&mut self, record: store::CallRecord) {
        self.report.calls += 1;
        self.report.tokens_in += record.tokens_in;
        self.report.tokens_out += record.tokens_out;
        self.report.cost_usd += record.cost_usd;
        if let Err(e) = store::record_call(self.pool, &record, Utc::now()).await {
            tracing::error!(error = %e, "recording an enrichment call failed");
        }
    }

    /// Handle a call error: credits → alert; everything stops the pass.
    async fn call_failed(&mut self, model: &str, n: usize, err: &CallError) -> String {
        self.ledger(store::CallRecord {
            model: model.to_string(),
            prompt_version: PROMPT_VERSION,
            events_requested: n as i32,
            ok: false,
            error: Some(err.to_string()),
            ..Default::default()
        })
        .await;
        if let CallError::CreditsExhausted { message, .. } = err {
            self.e.credits_exhausted(self.pool, self.now, message).await;
            return "Requesty credits exhausted".into();
        }
        format!("call failed: {err}")
    }

    /// One chat call for `batch`. `Ok(None)` = stop the pass (reason set).
    async fn call(
        &mut self,
        batch: &[&Candidate],
        reminder: Option<&str>,
        cache: bool,
        price: &ModelPrice,
    ) -> Option<(BatchOutcome, Decimal, Usage)> {
        let ids: Vec<String> = (1..=batch.len()).map(|i| format!("e{i}")).collect();
        let pairs: Vec<(String, &EventFacts)> = ids
            .iter()
            .cloned()
            .zip(batch.iter().map(|c| &c.facts))
            .collect();
        let body = chat_body(&self.e.config, &pairs, reminder, cache);
        let chars = body["messages"].to_string().len() + body["response_format"].to_string().len();
        if let Budget::Stop(why) = self.budget(estimate_cost(chars, max_tokens(batch.len()), price))
        {
            self.report.stopped = Some(why);
            return None;
        }
        let model = self.e.config.model.clone();
        let completion = match self.e.client.chat(&body, self.e.config.call_timeout).await {
            Ok(c) => c,
            Err(err) => {
                let why = self.call_failed(&model, batch.len(), &err).await;
                tracing::warn!(error = %err, "enrichment call failed; stopping this pass");
                self.report.stopped = Some(why);
                return None;
            }
        };
        self.e.credits_ok(self.pool, self.now).await;
        let texts: Vec<String> = batch.iter().map(|c| c.facts.grounding_text()).collect();
        let outcome = if completion.finish_reason == "length" {
            BatchOutcome {
                ok: Vec::new(),
                failed: (0..batch.len())
                    .map(|i| (i, "output was cut off (max_tokens)".to_string()))
                    .collect(),
            }
        } else {
            output::parse_batch(&completion.content, &ids, &texts)
        };
        let cost = call_cost(&completion.usage, price);
        let u = completion.usage;
        self.ledger(store::CallRecord {
            model,
            prompt_version: PROMPT_VERSION,
            events_requested: batch.len() as i32,
            events_ok: outcome.ok.len() as i32,
            tokens_in: u.prompt_tokens,
            tokens_cached: u.cached_tokens,
            tokens_cache_write: u.cache_write_tokens,
            tokens_out: u.completion_tokens,
            cost_usd: cost,
            provider_cost_usd: u.provider_cost_usd.and_then(Decimal::from_f64),
            ok: true,
            error: None,
        })
        .await;
        Some((outcome, cost, u))
    }

    async fn save(
        &mut self,
        c: &Candidate,
        e: &Enrichment,
        n: usize,
        cost: Decimal,
        u: &Usage,
    ) -> sqlx::Result<()> {
        let n = n.max(1);
        store::save_enrichment(
            self.pool,
            &store::NewEnrichment {
                event_id: c.facts.id,
                model: &self.e.config.model,
                prompt_version: PROMPT_VERSION,
                input_hash: &c.facts.input_hash(),
                output: e,
                tokens_in: (u.prompt_tokens / n as i64) as i32,
                tokens_out: (u.completion_tokens / n as i64) as i32,
                cost_usd: (cost / Decimal::from(n)).round_dp(6),
            },
        )
        .await?;
        self.report.enriched += 1;
        Ok(())
    }

    async fn give_up(&mut self, c: &Candidate, reason: &str) -> sqlx::Result<()> {
        tracing::warn!(event = %c.facts.id, title = %c.facts.title, reason, "enrichment given up");
        store::record_failure(
            self.pool,
            c.facts.id,
            &c.facts.input_hash(),
            PROMPT_VERSION,
            reason,
        )
        .await?;
        self.report.failed += 1;
        Ok(())
    }

    /// Enrich one batch, retrying its invalid results once. `Ok(false)` =
    /// stop the pass.
    async fn batch(
        &mut self,
        batch: &[&Candidate],
        cache: bool,
        price: &ModelPrice,
    ) -> sqlx::Result<bool> {
        let Some((outcome, cost, usage)) = self.call(batch, None, cache, price).await else {
            return Ok(false);
        };
        for (i, e) in &outcome.ok {
            self.save(batch[*i], e, batch.len(), cost, &usage).await?;
        }
        if outcome.failed.is_empty() {
            return Ok(true);
        }
        let retry: Vec<&Candidate> = outcome.failed.iter().map(|(i, _)| batch[*i]).collect();
        let reminder: String = outcome
            .failed
            .iter()
            .enumerate()
            .map(|(k, (_, why))| format!("- e{}: {why}", k + 1))
            .collect::<Vec<_>>()
            .join("\n");
        tracing::info!(count = retry.len(), %reminder, "retrying invalid enrichments once");
        let Some((second, cost2, usage2)) = self.call(&retry, Some(&reminder), true, price).await
        else {
            // Not retried (cap, credits, error): try again on a later pass.
            return Ok(false);
        };
        for (i, e) in &second.ok {
            self.save(retry[*i], e, retry.len(), cost2, &usage2).await?;
        }
        for (i, why) in &second.failed {
            self.give_up(retry[*i], why).await?;
        }
        Ok(true)
    }

    async fn embeddings(&mut self, embed_model: &str) -> anyhow::Result<()> {
        if !store::embeddings_available(self.pool).await? {
            tracing::info!(
                "embeddings off: events.event_embeddings does not exist (pgvector not usable)"
            );
            return Ok(());
        }
        let Some(price) = store::model_price(self.pool, embed_model).await? else {
            tracing::warn!(
                model = embed_model,
                "no events.model_prices row for EMBED_MODEL; embeddings skipped"
            );
            return Ok(());
        };
        let cands = store::candidates(self.pool, self.now - PAST_GRACE).await?;
        let ids: Vec<uuid::Uuid> = cands.iter().map(|c| c.facts.id).collect();
        let existing: HashMap<uuid::Uuid, store::EmbeddingRow> =
            store::embedding_rows(self.pool, &ids)
                .await?
                .into_iter()
                .map(|r| (r.event_id, r))
                .collect();
        let mut todo: Vec<(&Candidate, String, String, bool)> = Vec::new();
        for c in &cands {
            let hash = c.facts.input_hash();
            let current = c.enriched_hash.as_deref() == Some(&hash)
                && c.enriched_version == Some(PROMPT_VERSION);
            let gave_up =
                c.failed_hash.as_deref() == Some(&hash) && c.failed_version == Some(PROMPT_VERSION);
            let (text, facts_only) = match (&c.enriched_output, current, gave_up) {
                (Some(out), true, _) => (embed::embed_text(&c.facts, Some(&out.0)), false),
                (_, false, true) => (embed::embed_text(&c.facts, None), true),
                // Wait for the enrichment.
                _ => continue,
            };
            let th = embed::text_hash(&text);
            if existing.get(&c.facts.id).is_some_and(|r| {
                r.text_hash == th
                    && r.embed_version == embed::EMBED_VERSION
                    && r.model == embed_model
            }) {
                continue;
            }
            todo.push((c, text, th, facts_only));
        }
        for chunk in todo.chunks(self.e.config.embed_batch_size.max(1)) {
            let texts: Vec<String> = chunk.iter().map(|(_, t, _, _)| t.clone()).collect();
            let chars: usize = texts.iter().map(String::len).sum();
            let estimate = estimate_cost(chars, 0, &price);
            if let Budget::Stop(why) = self.budget(estimate) {
                self.report.stopped.get_or_insert(why);
                break;
            }
            match self
                .e
                .client
                .embed(embed_model, &texts, self.e.config.call_timeout)
                .await
            {
                Err(err) => {
                    let why = self.call_failed(embed_model, chunk.len(), &err).await;
                    tracing::warn!(error = %err, "embedding call failed");
                    self.report.stopped.get_or_insert(why);
                    break;
                }
                Ok((vectors, usage)) => {
                    self.e.credits_ok(self.pool, self.now).await;
                    let cost = call_cost(&usage, &price);
                    let bad = vectors.iter().any(|v| v.len() != embed::EMBED_DIMS);
                    self.ledger(store::CallRecord {
                        model: embed_model.to_string(),
                        prompt_version: embed::EMBED_VERSION,
                        events_requested: chunk.len() as i32,
                        events_ok: if bad { 0 } else { chunk.len() as i32 },
                        tokens_in: usage.prompt_tokens,
                        cost_usd: cost,
                        provider_cost_usd: usage.provider_cost_usd.and_then(Decimal::from_f64),
                        ok: !bad,
                        error: bad.then(|| format!("expected {} dimensions", embed::EMBED_DIMS)),
                        ..Default::default()
                    })
                    .await;
                    if bad {
                        self.report.stopped.get_or_insert(format!(
                            "{embed_model} does not return {} dimensions",
                            embed::EMBED_DIMS
                        ));
                        break;
                    }
                    for ((c, _, th, facts_only), v) in chunk.iter().zip(&vectors) {
                        store::save_embedding(
                            self.pool,
                            &store::NewEmbedding {
                                event_id: c.facts.id,
                                model: embed_model,
                                embed_version: embed::EMBED_VERSION,
                                text_hash: th,
                                facts_only: *facts_only,
                                embedding: v,
                            },
                        )
                        .await?;
                        self.report.embedded += 1;
                    }
                }
            }
        }
        Ok(())
    }
}

impl Enricher {
    /// Enrich due events, then embed, within the caps. Errors are only for
    /// the database; provider failures stop the pass and are reported.
    pub async fn run(&self, pool: &PgPool, now: DateTime<Utc>) -> anyhow::Result<EnrichReport> {
        let mut pass = Pass {
            e: self,
            pool,
            now,
            deadline: Instant::now() + self.config.run_budget,
            spent_before: store::spent_since(pool, london_midnight(now)).await?,
            report: EnrichReport::default(),
        };
        match store::model_price(pool, &self.config.model).await? {
            None => {
                tracing::warn!(
                    model = %self.config.model,
                    "no events.model_prices row for ENRICH_MODEL; add one (with catalogue prices) to enable enrichment"
                );
                pass.report.stopped = Some("no price for the model".into());
            }
            // /about promises that the model seeing excerpts keeps nothing.
            Some(price) if price.retention_days != Some(0) => {
                tracing::warn!(
                    model = %self.config.model,
                    retention_days = ?price.retention_days,
                    "ENRICH_MODEL is not zero-retention in events.model_prices; /about promises zero retention, so enrichment is off"
                );
                pass.report.stopped = Some("model is not zero-retention".into());
            }
            Some(price) => {
                let cands = store::candidates(pool, now - PAST_GRACE).await?;
                let queue: Vec<&Candidate> = cands
                    .iter()
                    .filter(|c| needs_enrichment(c, &c.facts.input_hash()))
                    .take(self.config.max_events_per_run)
                    .collect();
                pass.report.queued = queue.len();
                let size = self.config.batch_size.max(1);
                let chunks: Vec<&[&Candidate]> = queue.chunks(size).collect();
                // Cache the static prefix only when this pass makes several
                // calls: a cache write costs more than plain input.
                let cache = chunks.len() > 1;
                for chunk in &chunks {
                    if !pass.batch(chunk, cache, &price).await? {
                        break;
                    }
                }
            }
        }
        let credits_out = pass.report.stopped.as_deref() == Some("Requesty credits exhausted");
        if let (Some(m), false) = (self.config.embed_model.clone(), credits_out) {
            pass.embeddings(&m).await?;
        }
        pass.report.spent_today_usd = pass.spent_before + pass.report.cost_usd;
        Ok(pass.report)
    }

    async fn credits_exhausted(&self, pool: &PgPool, now: DateTime<Utc>, message: &str) {
        let state = match store::alert_raise(pool, CREDITS_ALERT, message, now).await {
            Ok(s) => s,
            Err(e) => {
                tracing::error!(error = %e, "recording the credits alert failed");
                return;
            }
        };
        let today = crate::normalise::london_date(now);
        let due = state
            .last_notified_at
            .is_none_or(|t| crate::normalise::london_date(t) != today);
        tracing::error!(%message, notify = due, "Requesty credits exhausted; enrichment paused");
        if !due {
            return;
        }
        let since = state
            .active_since
            .unwrap_or(now)
            .with_timezone(&London)
            .format("%-d %b %Y %H:%M %Z");
        let body = format!(
            "AI enrichment paused since {since}; top up at https://app.requesty.ai — it resumes automatically."
        );
        match self.notifier.notify(CREDITS_TITLE, &body, "high").await {
            Ok(()) => {
                if let Err(e) = store::alert_notified(pool, CREDITS_ALERT, now).await {
                    tracing::error!(error = %e, "recording the credits notification failed");
                }
            }
            Err(e) => tracing::error!(error = %e, "credits notification failed"),
        }
    }

    async fn credits_ok(&self, pool: &PgPool, now: DateTime<Utc>) {
        match store::alert_clear(pool, CREDITS_ALERT, now).await {
            Ok(Some(since)) => {
                tracing::info!(%since, "Requesty credits OK again; enrichment resumed");
                if let Err(e) = self
                    .notifier
                    .notify(
                        CREDITS_OK_TITLE,
                        "Requesty credits OK again — enrichment resumed.",
                        "default",
                    )
                    .await
                {
                    tracing::error!(error = %e, "credits-OK notification failed");
                }
            }
            Ok(None) => {}
            Err(e) => tracing::error!(error = %e, "clearing the credits alert failed"),
        }
    }
}

/// `f64` view of a spend figure, for logs.
pub fn usd(d: Decimal) -> f64 {
    d.round_dp(4).to_f64().unwrap_or(0.0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn opus() -> ModelPrice {
        ModelPrice {
            model: DEFAULT_MODEL.into(),
            input_usd_per_mtok: Decimal::from(4),
            output_usd_per_mtok: Decimal::from(20),
            cache_read_usd_per_mtok: Decimal::new(2, 1),
            cache_write_usd_per_mtok: Decimal::from(5),
            retention_days: Some(0),
        }
    }

    #[test]
    fn cost_from_usage_matches_requesty() {
        // Observed 2026-09-26: Requesty reported cost 0.043416 for this usage.
        let u = Usage {
            prompt_tokens: 3684,
            completion_tokens: 1250,
            cached_tokens: 0,
            cache_write_tokens: 3680,
            provider_cost_usd: Some(0.043416),
        };
        assert_eq!(call_cost(&u, &opus()), Decimal::new(43416, 6));
        // ... and 0.00516 for a cache hit.
        let u = Usage {
            prompt_tokens: 3706,
            completion_tokens: 216,
            cached_tokens: 3680,
            cache_write_tokens: 0,
            provider_cost_usd: None,
        };
        assert_eq!(call_cost(&u, &opus()), Decimal::new(5160, 6));
    }

    #[test]
    fn estimate_is_pessimistic() {
        let est = estimate_cost(30_000, max_tokens(10), &opus());
        // 10_001 tokens at $5/M + 5_000 at $20/M.
        assert_eq!(est, Decimal::new(150005, 6));
    }

    #[test]
    fn london_midnight_follows_bst() {
        let now = Utc.with_ymd_and_hms(2026, 9, 26, 12, 0, 0).unwrap();
        assert_eq!(
            london_midnight(now),
            Utc.with_ymd_and_hms(2026, 9, 25, 23, 0, 0).unwrap()
        );
        let late = Utc.with_ymd_and_hms(2026, 9, 26, 23, 30, 0).unwrap();
        assert_eq!(
            london_midnight(late),
            Utc.with_ymd_and_hms(2026, 9, 26, 23, 0, 0).unwrap()
        );
    }
}
