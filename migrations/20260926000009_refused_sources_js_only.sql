-- Allow `js_only` as a refused_sources reason: the site's events only render
-- with JavaScript (we fetch plain HTML and never run a browser).
--
-- Migrations are APPEND-ONLY: never edit this file once merged.

ALTER TABLE events.refused_sources DROP CONSTRAINT refused_sources_reason_code_check;
ALTER TABLE events.refused_sources ADD CONSTRAINT refused_sources_reason_code_check
    CHECK (reason_code IN ('robots_disallowed', 'bot_blocked', 'no_event_data', 'terms', 'js_only', 'other'));
