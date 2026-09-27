-- Scraper QA (src/qa/, #98): checks that scrapers store what the venue's
-- page actually says.
--
--   * events.source_runs.events_checked / missing_venue / missing_coords
--                             per-run counts the "missing venue/coords jump"
--                             rules compare against (NULL on older runs)
--   * events.qa_findings      deterministic rule hits of one run (no AI)
--   * events.qa_checks        AI checks: a zero-retention model compares the
--                             pages the run fetched with what we extracted.
--                             Only page URLs and sizes plus the validated
--                             verdict (with short quotes) are kept; the page
--                             HTML/text itself is never stored.
--   * events.qa_issues        one open "Scraper check" GitHub issue per source
--                             (separate from health_issues, which close on the
--                             next clean run)
--   * events.enrichment_calls.pass  which pass made a call: the ledger is
--                             shared, the daily caps are not
--
-- Migrations are APPEND-ONLY: never edit this file once merged.

ALTER TABLE events.source_runs
    ADD COLUMN events_checked INTEGER NULL,
    ADD COLUMN missing_venue  INTEGER NULL,
    ADD COLUMN missing_coords INTEGER NULL;

CREATE TABLE events.qa_findings (
    id        BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    run_id    BIGINT      NOT NULL REFERENCES events.source_runs (id) ON DELETE CASCADE,
    source_id BIGINT      NOT NULL REFERENCES events.sources (id) ON DELETE CASCADE,
    rule      TEXT        NOT NULL CHECK (rule IN ('end_before_start', 'year_out_of_range',
                  'long_span', 'midnight_not_all_day', 'duplicate_title', 'missing_venue_jump',
                  'missing_coords_jump', 'same_date', 'count_drop')),
    affected  INTEGER     NOT NULL,
    detail    TEXT        NOT NULL,
    examples  JSONB       NOT NULL DEFAULT '[]',
    found_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (run_id, rule)
);
CREATE INDEX qa_findings_source_idx ON events.qa_findings (source_id, run_id DESC);

CREATE TABLE events.qa_checks (
    id                  BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    source_id           BIGINT         NOT NULL REFERENCES events.sources (id) ON DELETE CASCADE,
    run_id              BIGINT         NULL REFERENCES events.source_runs (id) ON DELETE SET NULL,
    checked_at          TIMESTAMPTZ    NOT NULL,
    reason              TEXT           NOT NULL
                            CHECK (reason IN ('first', 'code_changed', 'weekly', 'rule_hit')),
    code_hash           TEXT           NOT NULL,
    rules_hit           TEXT[]         NOT NULL DEFAULT '{}',
    pages               JSONB          NOT NULL DEFAULT '[]',
    model               TEXT           NOT NULL,
    prompt_version      INTEGER        NOT NULL,
    cost_usd            NUMERIC(12, 6) NOT NULL DEFAULT 0,
    status              TEXT           NOT NULL
                            CHECK (status IN ('ok', 'issues', 'no_pages', 'invalid', 'failed')),
    wrong_fields        INTEGER        NOT NULL DEFAULT 0,
    missed_events       INTEGER        NOT NULL DEFAULT 0,
    verdict             JSONB          NULL,
    error               TEXT           NULL,
    github_issue_number BIGINT         NULL
);
CREATE INDEX qa_checks_source_idx ON events.qa_checks (source_id, checked_at DESC);

CREATE TABLE events.qa_issues (
    id                  BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    source_id           BIGINT      NOT NULL REFERENCES events.sources (id) ON DELETE CASCADE,
    github_issue_number BIGINT      NOT NULL,
    opened_at           TIMESTAMPTZ NOT NULL DEFAULT now(),
    closed_at           TIMESTAMPTZ NULL
);
CREATE UNIQUE INDEX qa_issues_one_open_per_source_idx
    ON events.qa_issues (source_id) WHERE closed_at IS NULL;

-- Existing rows (enrichment + embeddings) are 'enrich'.
ALTER TABLE events.enrichment_calls
    ADD COLUMN pass TEXT NOT NULL DEFAULT 'enrich' CHECK (pass IN ('enrich', 'qa'));
