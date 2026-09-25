//! Source health checks and GitHub issue lifecycle.
//!
//! After every run of a source, [`evaluate`] applies three rules to its
//! recent runs (newest first):
//!
//! 1. **Zero events**: the run found 0 events while the average of the last
//!    `trailing_runs` successful (`ok`) runs before it is > 0.
//! 2. **Consecutive errors**: the last `consecutive_error_runs` runs
//!    (including this one) all had errors or failed.
//! 3. **Count drop**: the run found some events, but fewer than
//!    `(100 - drop_threshold_pct)%` of the trailing average (needs at least
//!    `min_history_for_drop` prior successful runs).
//!
//! On a trip, [`HealthChecker`] opens ONE `scraper-broken` issue per source
//! titled `Scraper broken: <key>`. Dedupe is two-layered: the open row in
//! `events.health_issues`, and a lookup of open GitHub issues with that label
//! and exact title (so a DB reset never produces duplicates). When the source
//! recovers (clean run, no rule tripped) the issue gets a comment and is
//! closed.

use std::fmt;

use sqlx::PgPool;
use tokio::sync::Mutex;

use crate::github::{IssueFiler, IssueRef};
use crate::repo::{self, RunRow, SourceRow};

pub const LABEL: &str = "scraper-broken";

#[derive(Debug, Clone, PartialEq)]
pub struct HealthConfig {
    pub trailing_runs: usize,
    pub consecutive_error_runs: usize,
    pub drop_threshold_pct: f64,
    pub min_history_for_drop: usize,
}

impl Default for HealthConfig {
    fn default() -> Self {
        Self {
            trailing_runs: 5,
            consecutive_error_runs: 2,
            drop_threshold_pct: 60.0,
            min_history_for_drop: 3,
        }
    }
}

/// The per-run numbers the rules look at.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RunStats {
    pub events_found: i32,
    pub errors: i32,
    pub ok: bool,
}

impl From<&RunRow> for RunStats {
    fn from(r: &RunRow) -> Self {
        Self {
            events_found: r.events_found,
            errors: r.errors,
            ok: r.ok,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Trip {
    ZeroEvents {
        trailing_avg: f64,
    },
    ConsecutiveErrors {
        runs: usize,
    },
    CountDrop {
        current: i32,
        trailing_avg: f64,
        drop_pct: f64,
    },
}

impl fmt::Display for Trip {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Trip::ZeroEvents { trailing_avg } => write!(
                f,
                "found 0 events (trailing average of successful runs: {trailing_avg:.1})"
            ),
            Trip::ConsecutiveErrors { runs } => {
                write!(f, "errors on {runs} consecutive runs")
            }
            Trip::CountDrop {
                current,
                trailing_avg,
                drop_pct,
            } => write!(
                f,
                "found {current} events, {drop_pct:.0}% below the trailing average of {trailing_avg:.1}"
            ),
        }
    }
}

fn has_errors(r: &RunStats) -> bool {
    !r.ok || r.errors > 0
}

/// Apply the three rules. `runs` must be newest first, `runs[0]` being the
/// run just completed. Returns every rule that tripped.
pub fn evaluate(runs: &[RunStats], cfg: &HealthConfig) -> Vec<Trip> {
    let Some(current) = runs.first() else {
        return Vec::new();
    };
    let mut trips = Vec::new();

    let trailing: Vec<i32> = runs[1..]
        .iter()
        .filter(|r| r.ok)
        .take(cfg.trailing_runs)
        .map(|r| r.events_found)
        .collect();
    let avg = if trailing.is_empty() {
        0.0
    } else {
        trailing.iter().map(|&n| f64::from(n)).sum::<f64>() / trailing.len() as f64
    };

    if current.events_found == 0 && !trailing.is_empty() && avg > 0.0 {
        trips.push(Trip::ZeroEvents { trailing_avg: avg });
    }

    let k = cfg.consecutive_error_runs.max(1);
    if runs.len() >= k && runs[..k].iter().all(has_errors) {
        trips.push(Trip::ConsecutiveErrors { runs: k });
    }

    if current.events_found > 0 && trailing.len() >= cfg.min_history_for_drop && avg > 0.0 {
        let floor = avg * (1.0 - cfg.drop_threshold_pct / 100.0);
        if f64::from(current.events_found) < floor {
            trips.push(Trip::CountDrop {
                current: current.events_found,
                trailing_avg: avg,
                drop_pct: (1.0 - f64::from(current.events_found) / avg) * 100.0,
            });
        }
    }
    trips
}

/// Title of the health issue for a source.
pub fn issue_title(key: &str) -> String {
    format!("Scraper broken: {key}")
}

fn runs_table(runs: &[RunRow]) -> String {
    let mut s = String::from(
        "| started (UTC) | ok | events | errors | duration | error summary |\n|---|---|---|---|---|---|\n",
    );
    for r in runs {
        let summary = r
            .error_summary
            .as_deref()
            .unwrap_or("")
            .replace('|', "\\|")
            .replace('\n', " ");
        let summary: String = summary.chars().take(200).collect();
        s.push_str(&format!(
            "| {} | {} | {} | {} | {} ms | {} |\n",
            r.started_at.format("%Y-%m-%d %H:%M"),
            if r.ok { "yes" } else { "no" },
            r.events_found,
            r.errors,
            r.duration_ms,
            summary
        ));
    }
    s
}

/// Body of a newly opened health issue.
pub fn issue_body(source: &SourceRow, trips: &[Trip], runs: &[RunRow]) -> String {
    let reasons: String = trips.iter().map(|t| format!("- {t}\n")).collect();
    format!(
        "The ingestion health check tripped for source **`{key}`** ({kind}).\n\n\
         **Source:** {url}\n\n\
         ### Reason\n{reasons}\n\
         ### Recent runs (newest first)\n{table}\n\
         Implementation: `src/sources/` (key `{key}`). Fix the source, then re-run \
         `thaleia-ingest`; this issue is closed automatically once a run is healthy again.\n\n\
         _Filed automatically by thaleia-ingest._",
        key = source.key,
        kind = source.kind,
        url = source.base_url,
        table = runs_table(runs),
    )
}

/// What the checker did for one source.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HealthAction {
    Healthy,
    /// Tripped, but an issue is already open.
    AlreadyOpen(i64),
    /// Tripped; adopted an open GitHub issue found by label + title.
    Adopted(i64),
    /// Tripped; opened a new issue.
    Opened(i64),
    /// Recovered; commented on and closed these issues.
    Closed(Vec<i64>),
    /// Tripped or recovered but no GitHub filer is configured.
    NoFiler,
    /// Not healthy, not tripped (e.g. one error, below the threshold).
    Degraded,
}

/// Evaluates source health and manages GitHub issues.
pub struct HealthChecker {
    pub config: HealthConfig,
    filer: Option<Box<dyn IssueFiler>>,
    open_issues_cache: Mutex<Option<Vec<IssueRef>>>,
}

impl HealthChecker {
    pub fn new(config: HealthConfig, filer: Option<Box<dyn IssueFiler>>) -> Self {
        Self {
            config,
            filer,
            open_issues_cache: Mutex::new(None),
        }
    }

    /// Open `scraper-broken` issues on GitHub, listed once per checker
    /// (i.e. once per ingest run) and kept up to date locally.
    async fn open_issue_with_title(
        &self,
        filer: &dyn IssueFiler,
        title: &str,
    ) -> anyhow::Result<Vec<i64>> {
        let mut cache = self.open_issues_cache.lock().await;
        if cache.is_none() {
            *cache = Some(filer.list_open_issues(LABEL).await?);
        }
        Ok(cache
            .as_ref()
            .expect("filled above")
            .iter()
            .filter(|i| i.title == title)
            .map(|i| i.number)
            .collect())
    }

    async fn cache_add(&self, issue: IssueRef) {
        if let Some(c) = self.open_issues_cache.lock().await.as_mut() {
            c.push(issue);
        }
    }

    async fn cache_remove(&self, number: i64) {
        if let Some(c) = self.open_issues_cache.lock().await.as_mut() {
            c.retain(|i| i.number != number);
        }
    }

    /// Check one source after a run and open/close issues as needed.
    pub async fn check_source(
        &self,
        pool: &PgPool,
        source: &SourceRow,
    ) -> anyhow::Result<HealthAction> {
        let limit = (self.config.trailing_runs + self.config.consecutive_error_runs).max(10) as i64;
        let runs = repo::recent_runs(pool, source.id, limit).await?;
        let stats: Vec<RunStats> = runs.iter().map(RunStats::from).collect();
        let trips = evaluate(&stats, &self.config);
        let title = issue_title(&source.key);

        if !trips.is_empty() {
            let reason = trips
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join("; ");
            tracing::warn!(source = %source.key, %reason, "source health check tripped");
            if let Some(open) = repo::open_health_issue(pool, source.id).await? {
                return Ok(HealthAction::AlreadyOpen(open.github_issue_number));
            }
            let Some(filer) = self.filer.as_deref() else {
                return Ok(HealthAction::NoFiler);
            };
            if let Some(&n) = self.open_issue_with_title(filer, &title).await?.first() {
                repo::insert_health_issue(pool, source.id, n, &reason).await?;
                return Ok(HealthAction::Adopted(n));
            }
            let body = issue_body(source, &trips, &runs);
            let n = filer.create_issue(&title, &body, &[LABEL]).await?;
            self.cache_add(IssueRef {
                number: n,
                title: title.clone(),
            })
            .await;
            repo::insert_health_issue(pool, source.id, n, &reason).await?;
            return Ok(HealthAction::Opened(n));
        }

        let current_clean = stats.first().is_some_and(|r| r.ok && r.errors == 0);
        if !current_clean {
            return Ok(HealthAction::Degraded);
        }

        let db_open = repo::open_health_issue(pool, source.id).await?;
        let Some(filer) = self.filer.as_deref() else {
            if db_open.is_some() {
                repo::close_health_issues(pool, source.id).await?;
                return Ok(HealthAction::NoFiler);
            }
            return Ok(HealthAction::Healthy);
        };
        let mut numbers = self.open_issue_with_title(filer, &title).await?;
        if let Some(o) = &db_open {
            if !numbers.contains(&o.github_issue_number) {
                numbers.push(o.github_issue_number);
            }
        }
        if numbers.is_empty() {
            return Ok(HealthAction::Healthy);
        }
        let latest = &runs[0];
        let comment = format!(
            "Recovered: the run at {} UTC found {} events with no errors. Closing automatically.",
            latest.started_at.format("%Y-%m-%d %H:%M"),
            latest.events_found
        );
        numbers.sort_unstable();
        for &n in &numbers {
            filer.comment(n, &comment).await?;
            filer.close_issue(n).await?;
            self.cache_remove(n).await;
        }
        repo::close_health_issues(pool, source.id).await?;
        tracing::info!(source = %source.key, issues = ?numbers, "source recovered; closed issues");
        Ok(HealthAction::Closed(numbers))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn r(events: i32, errors: i32, ok: bool) -> RunStats {
        RunStats {
            events_found: events,
            errors,
            ok,
        }
    }

    fn kinds(trips: &[Trip]) -> Vec<&'static str> {
        trips
            .iter()
            .map(|t| match t {
                Trip::ZeroEvents { .. } => "zero",
                Trip::ConsecutiveErrors { .. } => "errors",
                Trip::CountDrop { .. } => "drop",
            })
            .collect()
    }

    #[test]
    fn rules_table() {
        let cfg = HealthConfig::default();
        // (name, runs newest-first, expected trips)
        let cases: Vec<(&str, Vec<RunStats>, Vec<&str>)> = vec![
            ("no runs", vec![], vec![]),
            ("first run, zero events", vec![r(0, 0, true)], vec![]),
            (
                "steady",
                vec![r(10, 0, true), r(11, 0, true), r(9, 0, true)],
                vec![],
            ),
            // Rule 1: zero events vs positive trailing average.
            (
                "zero after good runs",
                vec![r(0, 0, true), r(10, 0, true)],
                vec!["zero"],
            ),
            (
                "zero after zero runs",
                vec![r(0, 0, true), r(0, 0, true), r(0, 0, true)],
                vec![],
            ),
            (
                "zero ignores failed runs in average",
                vec![r(0, 0, true), r(0, 1, false), r(8, 0, true)],
                vec!["zero"],
            ),
            (
                "zero, only failed history",
                vec![r(0, 0, true), r(5, 1, false)],
                vec![],
            ),
            // Rule 2: K=2 consecutive runs with errors.
            (
                "one error run",
                vec![r(10, 1, true), r(10, 0, true)],
                vec![],
            ),
            (
                "two error runs",
                vec![r(10, 1, true), r(10, 2, true)],
                vec!["errors"],
            ),
            (
                "two failed runs",
                vec![r(0, 1, false), r(0, 1, false)],
                vec!["errors"],
            ),
            (
                "errors not consecutive",
                vec![r(10, 1, true), r(10, 0, true), r(10, 1, true)],
                vec![],
            ),
            (
                "failed + zero after good history",
                vec![r(0, 1, false), r(0, 1, false), r(10, 0, true)],
                vec!["zero", "errors"],
            ),
            // Rule 3: >60% drop vs trailing average, needs 3 prior ok runs.
            (
                "drop 70%",
                vec![
                    r(3, 0, true),
                    r(10, 0, true),
                    r(10, 0, true),
                    r(10, 0, true),
                ],
                vec!["drop"],
            ),
            (
                "drop exactly 60% is fine",
                vec![
                    r(4, 0, true),
                    r(10, 0, true),
                    r(10, 0, true),
                    r(10, 0, true),
                ],
                vec![],
            ),
            (
                "drop 50% is fine",
                vec![
                    r(5, 0, true),
                    r(10, 0, true),
                    r(10, 0, true),
                    r(10, 0, true),
                ],
                vec![],
            ),
            (
                "drop but short history",
                vec![r(1, 0, true), r(10, 0, true), r(10, 0, true)],
                vec![],
            ),
            (
                "trailing window is last 5 ok runs",
                vec![
                    r(3, 0, true),
                    r(4, 0, true),
                    r(4, 0, true),
                    r(4, 0, true),
                    r(4, 0, true),
                    r(4, 0, true),
                    r(100, 0, true),
                ],
                vec![],
            ),
        ];
        for (name, runs, want) in cases {
            assert_eq!(kinds(&evaluate(&runs, &cfg)), want, "case {name}");
        }
    }

    #[test]
    fn thresholds_are_configurable() {
        let cfg = HealthConfig {
            trailing_runs: 2,
            consecutive_error_runs: 3,
            drop_threshold_pct: 20.0,
            min_history_for_drop: 1,
        };
        assert_eq!(
            kinds(&evaluate(&[r(7, 0, true), r(10, 0, true)], &cfg)),
            ["drop"]
        );
        assert!(evaluate(&[r(10, 1, true), r(10, 1, true)], &cfg).is_empty());
        assert_eq!(
            kinds(&evaluate(
                &[r(10, 1, true), r(10, 1, true), r(10, 1, true)],
                &cfg
            )),
            ["errors"]
        );
    }

    #[test]
    fn trip_display() {
        assert_eq!(
            Trip::CountDrop {
                current: 3,
                trailing_avg: 10.0,
                drop_pct: 70.0
            }
            .to_string(),
            "found 3 events, 70% below the trailing average of 10.0"
        );
        assert_eq!(issue_title("ticketmaster"), "Scraper broken: ticketmaster");
    }
}
