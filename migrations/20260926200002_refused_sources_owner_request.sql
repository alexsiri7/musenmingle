-- Allow `owner_request` as a refused_sources reason: the site's owner asked us
-- not to list their events (see docs/venue-requests.md). We honour such
-- requests within 7 days and never re-add the site.
--
-- Migrations are APPEND-ONLY: never edit this file once merged.

ALTER TABLE events.refused_sources DROP CONSTRAINT refused_sources_reason_code_check;
ALTER TABLE events.refused_sources ADD CONSTRAINT refused_sources_reason_code_check
    CHECK (reason_code IN ('robots_disallowed', 'bot_blocked', 'no_event_data', 'terms', 'js_only',
                           'owner_request', 'other'));
