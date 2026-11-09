-- Refuse Sound and Music (the national new-music charity): checked
-- 2026-11-09 with the MuseNMingleBot UA. robots.txt allows everything but
-- named AI-training bots, /events/ is server-rendered and detail pages carry
-- JSON-LD Event markup, but the website terms
-- (https://soundandmusic.org/terms-and-conditions/) allow storing the
-- content "in electronic form on disk (but not on a server or other storage
-- device connected to a network) ... for your own personal, non-commercial
-- use" and say: "You may not otherwise reproduce, modify, copy or distribute
-- or use for commercial purposes any of the materials or content on the
-- Website without written permission from Sound and Music." That rules out
-- even facts + link, as for Autograph (#346) and the Horniman (#40).
-- Surveyed in #209, issue #226.
--
-- Migrations are APPEND-ONLY: never edit this file once merged.

INSERT INTO events.refused_sources (domain, name, url, reason_code, reason_text, checked_on, issue_url)
VALUES ('soundandmusic.org', 'Sound and Music', 'https://soundandmusic.org/events/',
        'terms',
        'its website terms allow storing the site''s content only on a personal disk, not on a '
        || 'networked server, for personal, non-commercial use, and forbid any other reproduction '
        || 'without written permission (https://soundandmusic.org/terms-and-conditions/)',
        DATE '2026-11-09', 'https://github.com/alexsiri7/musenmingle/issues/226')
ON CONFLICT (domain) DO NOTHING;
