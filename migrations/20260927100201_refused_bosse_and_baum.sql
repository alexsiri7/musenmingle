-- Refuse Bosse & Baum (Peckham gallery): checked 2026-09-26 with the
-- MuseNMingleBot UA. robots.txt allows the site, but it lists nothing to
-- ingest: the "current exhibitions" block of /exhibitions/ is empty, every
-- show listed is under "Previous Exhibitions" (the latest ran 24 Nov - 9 Dec
-- 2025), and the exhibitions sitemap was last modified on 22 Nov 2025. A
-- scraper would find no events. Surveyed in issue #124.
--
-- Migrations are APPEND-ONLY: never edit this file once merged.

INSERT INTO events.refused_sources (domain, name, url, reason_code, reason_text, checked_on, issue_url)
VALUES ('bosseandbaum.com', 'Bosse & Baum', 'https://www.bosseandbaum.com/exhibitions/',
        'no_event_data',
        'its website lists no current or upcoming exhibitions; the most recent one it shows '
        || 'ended in December 2025',
        DATE '2026-09-26', 'https://github.com/alexsiri7/musenmingle/issues/124')
ON CONFLICT (domain) DO NOTHING;
