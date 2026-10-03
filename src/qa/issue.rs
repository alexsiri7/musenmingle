//! "Scraper check" GitHub issues: one open per source, deduped through
//! `events.qa_issues` and a title-prefix match on open `scraper-broken`
//! issues, closed when a later check finds nothing.
//!
//! Not `events.health_issues`: the health checker closes those on the next
//! clean run, and a source storing wrong dates usually runs cleanly.

use chrono::{DateTime, NaiveDate, Utc};
use sqlx::PgPool;

use super::output::Verdict;
use super::store;
use crate::github::IssueFiler;
use crate::health::LABEL;
use crate::repo::SourceRow;

/// What happened to the source's QA issue.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IssueAction {
    Opened(i64),
    /// An open GitHub issue with our title prefix, taken over and commented.
    Adopted(i64),
    /// Our open issue got the new report.
    Commented(i64),
    Closed(i64),
}

impl IssueAction {
    pub fn number(self) -> i64 {
        match self {
            IssueAction::Opened(n)
            | IssueAction::Adopted(n)
            | IssueAction::Commented(n)
            | IssueAction::Closed(n) => n,
        }
    }
}

/// Stable part of the title (the counts after it vary).
pub fn title_prefix(key: &str) -> String {
    format!("Scraper check: {key} — ")
}

pub fn title(key: &str, wrong: usize, missed: usize) -> String {
    let counts = match (wrong, missed) {
        (w, 0) => format!("{w} field(s) wrong"),
        (0, m) => format!("{m} missed event(s)"),
        (w, m) => format!("{w} field(s) wrong, {m} missed event(s)"),
    };
    format!("{}{counts}", title_prefix(key))
}

/// A Markdown table cell from third-party text: one line, no pipes or
/// backticks, at most 160 characters.
pub fn cell(s: &str) -> String {
    let flat: String = s
        .chars()
        .map(|c| match c {
            '\n' | '\r' => ' ',
            '`' => '\'',
            c => c,
        })
        .collect();
    let flat = flat.trim();
    let cut: String = if flat.chars().count() > 160 {
        flat.chars().take(159).collect::<String>() + "…"
    } else {
        flat.to_string()
    };
    cut.replace('|', "\\|")
}

/// A cell in backticks, so `@mentions`, `#123` and links stay inert.
fn code(s: &str) -> String {
    format!("`{}`", cell(s))
}

fn value_text(v: &serde_json::Value) -> String {
    match v {
        serde_json::Value::Null => "(none)".into(),
        serde_json::Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

/// Everything a fix needs.
pub struct Report<'a> {
    pub row: &'a SourceRow,
    pub check_id: i64,
    pub reason: &'a str,
    pub model: &'a str,
    /// `(kind, url)` of the pages the judge saw.
    pub pages: &'a [(String, String)],
    pub verdict: &'a Verdict,
    pub fetched_on: NaiveDate,
}

/// `https://<host>/` of the checked listing page (else the first page, else
/// the source's base URL). The factory screener (interstellarai.net
/// `ops/cron/lib/screen.sh`, `_screen_hosts`) reads the venue's host from the
/// `**Source:** https://<host>` line and allows links on that host only.
fn source_origin(r: &Report<'_>) -> String {
    let listing = r.pages.iter().find(|(kind, _)| kind == "listing");
    listing
        .or(r.pages.first())
        .map(|(_, url)| url.as_str())
        .into_iter()
        .chain([r.row.base_url.as_str()])
        .filter_map(|u| url::Url::parse(u).ok())
        .find_map(|u| {
            let host = u.host_str()?.to_ascii_lowercase();
            Some(format!("{}://{host}/", u.scheme()))
        })
        .unwrap_or_else(|| format!("https://{}/", r.row.domain))
}

/// The model's name without its vendor prefix (`vendor/name` → `name`):
/// vendor names such as the model provider's are on the screener's secrets
/// deny-list.
fn model_name(model: &str) -> &str {
    model.rsplit('/').next().unwrap_or(model)
}

pub fn body(r: &Report<'_>) -> String {
    let key = &r.row.key;
    let mut s = format!(
        "The scraper check found differences between what **`{key}`** stored and \
         the venue's pages.\n\n**Source:** {}\n\n**Why checked:** {}\n\n### Pages checked ({})\n",
        source_origin(r),
        r.reason,
        r.fetched_on.format("%Y-%m-%d"),
    );
    for (kind, url) in r.pages {
        s.push_str(&format!("- {kind}: <{}>\n", cell(url)));
    }
    if r.verdict.wrong_count() > 0 {
        s.push_str(
            "\n### Wrong fields\n| record | field | stored | page says | quote |\n|---|---|---|---|---|\n",
        );
        for f in r.verdict.wrong() {
            s.push_str(&format!(
                "| {} | {} | {} | {} | {} |\n",
                code(&f.record),
                f.field,
                code(&value_text(&f.ours)),
                code(f.page_says.as_deref().unwrap_or("")),
                code(f.evidence_quote.as_deref().unwrap_or("")),
            ));
        }
    }
    if !r.verdict.missed.is_empty() {
        s.push_str(
            "\n### Events on the listing we did not collect\n| title | quote |\n|---|---|\n",
        );
        for m in &r.verdict.missed {
            s.push_str(&format!(
                "| {} | {} |\n",
                code(&m.title),
                code(&m.evidence_quote)
            ));
        }
    }
    s.push_str(&format!(
        "\n### To fix\n\
         - [ ] Save the pages above (with the MuseNMingleBot User-Agent) as \
         `tests/fixtures/scrapers/{key}/qa-{date}.html` (one file per page, suffixed if several).\n\
         - [ ] Add a snapshot test on them, using the \"page says\" column as the expected values.\n\
         - [ ] Fix the scraper in `src/sources/` until the snapshot is right.\n\n\
         These verdicts come from an AI model (`{model}`) comparing the page with our records; \
         verify on the page before changing the scraper. The model never changes stored data.\n\n\
         _Filed automatically by musenmingle-ingest (scraper check #{id})._",
        date = r.fetched_on.format("%Y-%m-%d"),
        model = model_name(r.model),
        id = r.check_id,
    ));
    s
}

/// Open, adopt or comment on the source's QA issue for a check that found
/// problems (`report` with its title), or close it after a clean check
/// (`None`).
pub async fn sync(
    pool: &PgPool,
    filer: &dyn IssueFiler,
    source: &SourceRow,
    report: Option<(&str, &str)>,
    now: DateTime<Utc>,
) -> anyhow::Result<Option<IssueAction>> {
    let open = store::open_issue(pool, source.id).await?;
    let Some((title, body)) = report else {
        let Some((id, number)) = open else {
            return Ok(None);
        };
        filer
            .comment(
                number,
                &format!(
                    "Latest check at {} UTC found no problems. Closing automatically.",
                    now.format("%Y-%m-%d %H:%M")
                ),
            )
            .await?;
        filer.close_issue(number).await?;
        store::close_issue(pool, id, now).await?;
        return Ok(Some(IssueAction::Closed(number)));
    };
    if let Some((_, number)) = open {
        filer.comment(number, body).await?;
        return Ok(Some(IssueAction::Commented(number)));
    }
    let prefix = title_prefix(&source.key);
    let existing = filer
        .list_open_issues(LABEL)
        .await?
        .into_iter()
        .find(|i| i.title.starts_with(&prefix));
    let action = match existing {
        Some(i) => {
            filer.comment(i.number, body).await?;
            IssueAction::Adopted(i.number)
        }
        None => IssueAction::Opened(filer.create_issue(title, body, &[LABEL]).await?),
    };
    store::insert_issue(pool, source.id, action.number()).await?;
    Ok(Some(action))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::SourceKind;
    use crate::qa::output::{FieldVerdict, MissedEvent};
    use serde_json::json;

    #[test]
    fn titles_keep_a_stable_prefix() {
        assert_eq!(
            title("tate", 2, 0),
            "Scraper check: tate — 2 field(s) wrong"
        );
        assert_eq!(
            title("tate", 0, 1),
            "Scraper check: tate — 1 missed event(s)"
        );
        assert_eq!(
            title("tate", 1, 3),
            "Scraper check: tate — 1 field(s) wrong, 3 missed event(s)"
        );
        assert!(title("tate", 5, 5).starts_with(&title_prefix("tate")));
    }

    #[test]
    fn cells_are_inert_single_lines() {
        assert_eq!(cell("a|b\n`c`"), "a\\|b 'c'");
        let long = "x".repeat(200);
        assert_eq!(cell(&long).chars().count(), 160);
        assert!(cell(&long).ends_with('…'));
    }

    fn sample_row() -> SourceRow {
        SourceRow {
            id: 1,
            key: "fake".into(),
            kind: SourceKind::Scraper,
            base_url: "https://venue.test".into(),
            domain: "venue.test".into(),
            interval_minutes: 1440,
            enabled: true,
            last_run_at: None,
            platform: None,
            config: None,
            may_be_empty: false,
        }
    }

    fn sample_verdict() -> Verdict {
        Verdict {
            findings: vec![FieldVerdict {
                id: "r1".into(),
                record: "Night Talk".into(),
                field: "starts_at".into(),
                ours: json!("2026-11-12 00:00"),
                page_says: Some("12 Nov, 8pm @someone #12".into()),
                verdict: "wrong".into(),
                evidence_quote: Some("12 November | 8pm".into()),
            }],
            missed: vec![MissedEvent {
                title: "Print Fair".into(),
                evidence_quote: "Print Fair — 20 November".into(),
            }],
        }
    }

    fn sample_body(model: &str, pages: &[(String, String)]) -> String {
        body(&Report {
            row: &sample_row(),
            check_id: 7,
            reason: "first",
            model,
            pages,
            verdict: &sample_verdict(),
            fetched_on: NaiveDate::from_ymd_opt(2026, 10, 1).unwrap(),
        })
    }

    #[test]
    fn body_has_the_table_the_fixture_path_and_no_live_mentions() {
        let pages = [("detail".to_string(), "https://venue.test/e/1".to_string())];
        let b = sample_body("m", &pages);
        assert!(b.contains(
            "| `Night Talk` | starts_at | `2026-11-12 00:00` | `12 Nov, 8pm @someone #12` | `12 November \\| 8pm` |"
        ));
        assert!(b.contains("| `Print Fair` | `Print Fair — 20 November` |"));
        assert!(b.contains("tests/fixtures/scrapers/fake/qa-2026-10-01.html"));
        assert!(b.contains("<https://venue.test/e/1>"));
        assert!(b.contains("scraper check #7"));
    }

    /// Words from the factory screener's H-SECRETS deny-list (interstellarai.net
    /// `ops/cron/lib/screen.sh`) that our own template text could plausibly
    /// contain. Any hit holds the issue for the owner.
    const SCREENER_DENY: &[&str] = &[
        "anthropic",
        "requesty",
        "api_key",
        "api key",
        "api-key",
        "apikey",
        "token",
        "secret",
        "credential",
        "password",
        "passwd",
        ".env",
        ".config/",
        "environment variable",
        "system prompt",
        "as an ai",
    ];

    #[test]
    fn body_passes_the_factory_screener() {
        let pages = [
            (
                "listing".to_string(),
                "https://WWW.Venue.test/whats-on?page=2".to_string(),
            ),
            (
                "detail".to_string(),
                "https://www.venue.test/e/1".to_string(),
            ),
        ];
        let b = sample_body("anthropic/claude-opus-5-5", &pages);
        // `_screen_hosts` parses `\*\*Source:\*\* https?://<host>`.
        assert!(b.contains("\n**Source:** https://www.venue.test/\n"), "{b}");
        assert!(b.contains("(`claude-opus-5-5`)"));
        let lower = b.to_lowercase();
        for word in SCREENER_DENY {
            assert!(!lower.contains(word), "body contains {word:?}:\n{b}");
        }
    }

    #[test]
    fn source_falls_back_to_the_first_page_then_the_base_url() {
        let pages = [("api".to_string(), "http://api.venue.test/v1".to_string())];
        assert!(sample_body("m", &pages).contains("**Source:** http://api.venue.test/\n"));
        assert!(sample_body("m", &[]).contains("**Source:** https://venue.test/\n"));
    }

    #[test]
    fn model_names_lose_the_vendor_prefix() {
        assert_eq!(model_name("anthropic/claude-opus-5-5"), "claude-opus-5-5");
        assert_eq!(model_name("claude-opus-5-5"), "claude-opus-5-5");
    }
}
