-- Refuse the music venues the #209 survey (music, option (b)) could not
-- use, checked 2026-09-27 with the MuseNMingleBot UA at about 1 request/s.
-- The usable venues got "New scraper" issues (#214–#226); Barbican music
-- is #227.
--
-- * Wigmore Hall: CloudFront answers "403 ERROR … Request blocked." to
--   every request, including /robots.txt; same again a few minutes later.
-- * Kings Place: a Cloudflare "Just a moment..." challenge (403) on every
--   page, including /robots.txt; same again a few minutes later.
-- * Ronnie Scott's: a Cloudflare challenge (403, `cf-mitigated: challenge`)
--   on /whats-on and the home page; same again a few minutes later.
-- * London Symphony Orchestra (LSO St Luke's): its terms of use
--   (https://www.lso.co.uk/terms-and-conditions/) say "It is prohibited to
--   use any automated system, software, robot, or any other method of
--   'screen scraping', to extract data from this website for commercial
--   purposes". We're non-commercial, but we don't scrape a site whose
--   terms name scraping, as with White Cube. LSO concerts at the Barbican
--   can still come through the Barbican source (#227).
-- * Cadogan Hall: its terms (https://cadoganhall.com/terms-conditions/)
--   allow the site's text and images "for personal use only" and "No part
--   of this site may be reproduced without written permission", as with
--   D&AD.
-- * Rich Mix: its terms (https://richmix.org.uk/terms-and-conditions) say
--   "You may not modify, copy, distribute, transmit, display, perform,
--   reproduce, publish … any information obtained from it", as with
--   ArtRabbit.
--
-- Nothing was retried with another User-Agent: we don't evade blocks.
--
-- Migrations are APPEND-ONLY: never edit this file once merged.

INSERT INTO events.refused_sources (domain, name, url, reason_code, reason_text, checked_on, issue_url)
VALUES
    ('wigmore-hall.org.uk', 'Wigmore Hall', 'https://www.wigmore-hall.org.uk/whats-on',
     'bot_blocked',
     'its site returns 403 to our bot for every page, even robots.txt; we don''t evade blocks',
     DATE '2026-09-27', 'https://github.com/alexsiri7/musenmingle/issues/209'),
    ('kingsplace.co.uk', 'Kings Place', 'https://www.kingsplace.co.uk/whats-on/',
     'bot_blocked',
     'its site answers our bot with a Cloudflare challenge (403), even for robots.txt; we don''t evade blocks',
     DATE '2026-09-27', 'https://github.com/alexsiri7/musenmingle/issues/209'),
    ('ronniescotts.co.uk', 'Ronnie Scott''s', 'https://www.ronniescotts.co.uk/whats-on',
     'bot_blocked',
     'its site answers our bot with a Cloudflare challenge (403); we don''t evade blocks',
     DATE '2026-09-27', 'https://github.com/alexsiri7/musenmingle/issues/209'),
    ('lso.co.uk', 'London Symphony Orchestra (LSO St Luke''s)', 'https://www.lso.co.uk/whats-on/',
     'terms',
     'its terms of use prohibit screen-scraping data from the site for commercial purposes; '
     || 'we read that conservatively (https://www.lso.co.uk/terms-and-conditions/)',
     DATE '2026-09-27', 'https://github.com/alexsiri7/musenmingle/issues/209'),
    ('cadoganhall.com', 'Cadogan Hall', 'https://cadoganhall.com/whats-on/',
     'terms',
     'its terms limit the site''s content to personal use and forbid reproducing any part of it '
     || '(https://cadoganhall.com/terms-conditions/)',
     DATE '2026-09-27', 'https://github.com/alexsiri7/musenmingle/issues/209'),
    ('richmix.org.uk', 'Rich Mix', 'https://richmix.org.uk/whats-on/',
     'terms',
     'its terms don''t allow reproducing any information obtained from the site '
     || '(https://richmix.org.uk/terms-and-conditions)',
     DATE '2026-09-27', 'https://github.com/alexsiri7/musenmingle/issues/209')
ON CONFLICT (domain) DO NOTHING;
