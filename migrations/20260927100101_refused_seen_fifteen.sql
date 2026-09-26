-- Refuse Seen Fifteen (Peckham photography gallery): checked 2026-09-26 with
-- the MuseNMingleBot UA. robots.txt allows the site, but it lists nothing to
-- ingest: /exhibitions-current/ still shows Gabby Laurent (12-28 May 2023),
-- /exhibitions-upcoming/ says "There are no upcoming events at the moment",
-- and the exhibitions feed was last built on 15 Jul 2023. A scraper would
-- find no events. Surveyed in issue #125.
--
-- Migrations are APPEND-ONLY: never edit this file once merged.

INSERT INTO events.refused_sources (domain, name, url, reason_code, reason_text, checked_on, issue_url)
VALUES ('seenfifteen.com', 'Seen Fifteen', 'https://seenfifteen.com/exhibitions/',
        'no_event_data',
        'its website lists no current or upcoming exhibitions; the most recent one it shows '
        || 'ended in May 2023',
        DATE '2026-09-26', 'https://github.com/alexsiri7/musenmingle/issues/125')
ON CONFLICT (domain) DO NOTHING;
