-- Site-wide daily cap on GitHub issues and comments filed for the public
-- forms (issue #88): `/v1/suggestions` and `/contact`. One row per London
-- date (`day`); `filed` counts the slots reserved that day
-- (`repo::reserve_form_issue`, before each GitHub call). Requests over the
-- cap (`FORM_ISSUES_PER_DAY`) stay pending and are filed on a later day;
-- the owner gets a daily ntfy digest meanwhile (`crate::issue_cap`).
--
-- Migrations are APPEND-ONLY: never edit this file once merged.

CREATE TABLE events.form_issue_quota (
    day   DATE    PRIMARY KEY,
    filed INTEGER NOT NULL CHECK (filed >= 0)
);
