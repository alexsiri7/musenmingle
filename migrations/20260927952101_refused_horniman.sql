-- Refuse the Horniman Museum and Gardens (SE23): checked 2026-09-27 with the
-- MuseNMingleBot UA. robots.txt allows everything (Crawl-delay: 5) and
-- /whats-on/ is server-rendered, but the website terms
-- (https://www.horniman.ac.uk/terms-and-conditions/, "Your use of this
-- website") allow reproduction for personal, non-commercial use only and
-- say: "You may not copy or otherwise incorporate into or store in any other
-- website, electronic retrieval system, publication or other work any of the
-- content of the website in any form ... unless We give our written
-- permission." That rules out even facts + link, as for Hall Place (#115).
-- Surveyed in issue #40.
--
-- Migrations are APPEND-ONLY: never edit this file once merged.

INSERT INTO events.refused_sources (domain, name, url, reason_code, reason_text, checked_on, issue_url)
VALUES ('horniman.ac.uk', 'Horniman Museum and Gardens', 'https://www.horniman.ac.uk/whats-on/',
        'terms',
        'its website terms allow personal, non-commercial use only and forbid storing any of '
        || 'the site''s content in another website or electronic retrieval system without '
        || 'written permission (https://www.horniman.ac.uk/terms-and-conditions/)',
        DATE '2026-09-27', 'https://github.com/alexsiri7/musenmingle/issues/40')
ON CONFLICT (domain) DO NOTHING;
