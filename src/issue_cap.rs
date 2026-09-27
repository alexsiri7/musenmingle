//! The site-wide daily cap on GitHub issues filed for the public forms
//! (`crate::suggestions`, `crate::contact`), so a flood of submissions
//! can't flood the repository. Every issue or comment a form submission
//! causes takes a slot for the London day first ([`reserve`]); without one
//! the submission stays pending, the ingest run files it on a later day
//! (oldest first), and meanwhile the owner gets one ntfy digest a day
//! ([`FormIssueCap::digest`]). Health and QA issues are not capped.

use chrono::{DateTime, Utc};
use sqlx::PgPool;

use crate::enrich::store;
use crate::notify::{LogNotifier, Notifier};
use crate::repo;

/// `FORM_ISSUES_PER_DAY` default.
pub const DEFAULT_PER_DAY: u32 = 20;

/// `events.alert_state` key for form submissions held by the cap.
pub const ALERT: &str = "form-issues-held";
const ALERT_TITLE: &str = "Muse & Mingle: form submissions waiting";

/// Take one of today's `per_day` slots; false when they are all taken.
pub async fn reserve(pool: &PgPool, per_day: u32, now: DateTime<Utc>) -> sqlx::Result<bool> {
    let per_day = i32::try_from(per_day).unwrap_or(i32::MAX);
    repo::reserve_form_issue(pool, crate::normalise::london_date(now), per_day).await
}

/// The cap as the ingest runner applies it to pending submissions.
pub struct FormIssueCap {
    pub per_day: u32,
    pub notifier: Box<dyn Notifier>,
}

impl Default for FormIssueCap {
    fn default() -> Self {
        Self {
            per_day: DEFAULT_PER_DAY,
            notifier: Box::new(LogNotifier),
        }
    }
}

impl FormIssueCap {
    /// The owner's digest: while filing pending submissions stops at the cap
    /// (`capped`), say how many are waiting, at most once per London day.
    /// Errors are logged.
    pub async fn digest(&self, pool: &PgPool, now: DateTime<Utc>, capped: bool) {
        if !capped {
            if let Err(e) = store::alert_clear(pool, ALERT, now).await {
                tracing::error!(error = %e, "clearing the form-cap alert failed");
            }
            return;
        }
        let (suggestions, contacts) = match repo::held_form_counts(pool).await {
            Ok(counts) => counts,
            Err(e) => {
                tracing::error!(error = %e, "counting held form submissions failed");
                return;
            }
        };
        let body = digest_body(suggestions, contacts, self.per_day);
        let state = match store::alert_raise(pool, ALERT, &body, now).await {
            Ok(s) => s,
            Err(e) => {
                tracing::error!(error = %e, "recording the form-cap alert failed");
                return;
            }
        };
        let today = crate::normalise::london_date(now);
        let due = state
            .last_notified_at
            .is_none_or(|t| crate::normalise::london_date(t) != today);
        tracing::warn!(
            suggestions,
            contacts,
            notify = due,
            "form submissions held by the daily cap"
        );
        if !due {
            return;
        }
        match self.notifier.notify(ALERT_TITLE, &body, "default").await {
            Ok(()) => {
                if let Err(e) = store::alert_notified(pool, ALERT, now).await {
                    tracing::error!(error = %e, "recording the form-cap notification failed");
                }
            }
            Err(e) => tracing::error!(error = %e, "form-cap notification failed"),
        }
    }
}

fn digest_body(suggestions: i64, contacts: i64, per_day: u32) -> String {
    format!(
        "{suggestions} site suggestion(s) and {contacts} contact request(s) are waiting: today's cap \
         of {per_day} GitHub issues from the site's forms is reached (FORM_ISSUES_PER_DAY). They \
         are filed oldest first as the cap allows."
    )
}
