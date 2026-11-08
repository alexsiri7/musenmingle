-- Refuse IKLECTIK (Old Paradise Yard, SE1): checked 2026-11-08 with the
-- MuseNMingleBot UA. robots.txt allows everything but ?lightbox= URLs, and
-- the Wix home page is server-rendered, but its events widget is not: the
-- raw HTML holds only the placeholders "Loading events..." and "No
-- upcoming events", and its only JSON-LD is a WebSite block. Guessed
-- listing paths (/whats-on, /events, /calendar, /programme, ...) 404, and
-- the sitemap lists one generic /event-detail template, not per-event
-- pages. Suggested in issue #216 (from the #209 music-venue survey).
--
-- Migrations are APPEND-ONLY: never edit this file once merged.

INSERT INTO events.refused_sources (domain, name, url, reason_code, reason_text, checked_on, issue_url)
VALUES ('iklectik.org', 'IKLECTIK', 'https://www.iklectik.org/',
        'js_only',
        'its events only render with JavaScript; the page itself just says "Loading events..."',
        DATE '2026-11-08', 'https://github.com/alexsiri7/musenmingle/issues/216')
ON CONFLICT (domain) DO NOTHING;
