-- Refuse City Lit (short courses, https://www.citylit.ac.uk/courses):
-- checked 2026-09-27 with the MuseNMingleBot UA. The site's Fastly edge
-- answers every request with an empty 403 (`content-length: 0`), including
-- /robots.txt, the home page and /courses, on both www.citylit.ac.uk and
-- citylit.ac.uk, and did so again a few minutes later. The 2026-09-26 source
-- survey (#47, old ThaleiaBot UA) had seen a 200, so the block is new; it is
-- a block on our bot all the same. Nothing was retried with another
-- User-Agent: we don't evade blocks. Surveyed in issue #39.
--
-- If City Lit lifts the block, a later migration can delete this row and a
-- scraper can be written from the survey notes in #39 (JSON-LD Course +
-- CourseInstance on the subject listing pages).
--
-- Migrations are APPEND-ONLY: never edit this file once merged.

INSERT INTO events.refused_sources (domain, name, url, reason_code, reason_text, checked_on, issue_url)
VALUES ('citylit.ac.uk', 'City Lit', 'https://www.citylit.ac.uk/courses',
        'bot_blocked',
        'its site returns 403 to our bot for every page, even robots.txt; we don''t evade blocks',
        DATE '2026-09-27', 'https://github.com/alexsiri7/musenmingle/issues/39')
ON CONFLICT (domain) DO NOTHING;
