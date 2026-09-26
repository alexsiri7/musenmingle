-- Refuse Chiswick House & Gardens (W4): checked 2026-09-26 with the
-- MuseNMingleBot UA. robots.txt allows /whats-on/ and /event/…, and the detail
-- pages carry JSON-LD Event markup, but the site's terms of use
-- (https://chiswickhouseandgardens.org.uk/terms-conditions/, clause 7 "No text
-- or data mining, or web scraping") forbid any "robot", "bot", "spider" or
-- "scraper" used "to access, obtain, copy, monitor or republish any portion of
-- our Site or any data, content, information" for any purpose. That rules out
-- even facts + link. Surveyed in issue #121.
--
-- Migrations are APPEND-ONLY: never edit this file once merged.

INSERT INTO events.refused_sources (domain, name, url, reason_code, reason_text, checked_on, issue_url)
VALUES ('chiswickhouseandgardens.org.uk', 'Chiswick House & Gardens',
        'https://chiswickhouseandgardens.org.uk/whats-on/',
        'terms',
        'its website terms of use forbid web scraping and any bot that copies or monitors data '
        || 'from the site (https://chiswickhouseandgardens.org.uk/terms-conditions/)',
        DATE '2026-09-26', 'https://github.com/alexsiri7/musenmingle/issues/121')
ON CONFLICT (domain) DO NOTHING;
