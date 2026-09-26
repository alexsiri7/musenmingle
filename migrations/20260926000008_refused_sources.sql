-- Sites we looked at and decided not to scrape (robots.txt disallows the
-- events pages, the site blocks our bot, there is no usable event data, ...).
-- Shown on /sources and in GET /v1/sources (`refused`) so visitors know why a
-- venue is missing, and checked by site suggestions (a suggestion for a
-- refused registrable domain is answered with the reason and never filed).
--
-- Add a row with a NEW migration in the same PR that closes a `new-scraper`
-- issue as not possible (see docs/adding-a-scraper.md).
--
-- Migrations are APPEND-ONLY: never edit this file once merged.

CREATE TABLE events.refused_sources (
    id          BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    -- Registrable domain (e.g. 'southbankcentre.co.uk'), as suggestions use.
    domain      TEXT        NOT NULL UNIQUE,
    name        TEXT        NOT NULL,
    url         TEXT        NOT NULL,
    reason_code TEXT        NOT NULL
        CHECK (reason_code IN ('robots_disallowed', 'bot_blocked', 'no_event_data', 'terms', 'other')),
    reason_text TEXT        NOT NULL,
    checked_on  DATE        NOT NULL,
    issue_url   TEXT        NULL,
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now()
);

INSERT INTO events.refused_sources (domain, name, url, reason_code, reason_text, checked_on, issue_url)
VALUES
    ('creativemornings.com', 'CreativeMornings London', 'https://creativemornings.com/cities/lon',
     'robots_disallowed',
     'robots.txt disallows /happening and the site answers our bot with an empty 202',
     DATE '2026-09-25', 'https://github.com/alexsiri7/thaleia/issues/4'),
    ('southbankcentre.co.uk', 'Southbank Centre', 'https://www.southbankcentre.co.uk/whats-on',
     'bot_blocked',
     'it returns 403 to the ThaleiaBot User-Agent; we don''t evade blocks',
     DATE '2026-09-25', 'https://github.com/alexsiri7/thaleia/issues/6');

-- A suggestion for a refused domain is recorded as 'refused' (it still
-- counts toward the submitter's rate limit) and never filed.
ALTER TABLE events.site_suggestions DROP CONSTRAINT site_suggestions_status_check;
ALTER TABLE events.site_suggestions ADD CONSTRAINT site_suggestions_status_check
    CHECK (status IN ('pending', 'accepted', 'rejected', 'duplicate', 'refused'));
