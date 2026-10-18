-- Refuse Autograph (Rivington Place, EC2A): checked 2026-10-18 with the
-- MuseNMingleBot UA. robots.txt allows everything but /my-account/ and
-- /checkout/, and /exhibitions/ and /events are server-rendered, but the
-- website terms (https://autograph.org.uk/terms-and-conditions, "Licence")
-- say "any use of extracts from this Website is prohibited" (5.1) and "No
-- part of this Website may be reproduced or stored in any other website or
-- included in any public or private electronic retrieval system or service
-- without our prior written permission" (5.3). That rules out even facts +
-- link, as for Hall Place (#115) and the Horniman (#40). Suggested in
-- issue #346.
--
-- Migrations are APPEND-ONLY: never edit this file once merged.

INSERT INTO events.refused_sources (domain, name, url, reason_code, reason_text, checked_on, issue_url)
VALUES ('autograph.org.uk', 'Autograph', 'https://autograph.org.uk/events',
        'terms',
        'its website terms prohibit any use of extracts from the site and forbid storing any '
        || 'part of it in another website or in any public or private electronic retrieval '
        || 'system without written permission (https://autograph.org.uk/terms-and-conditions)',
        DATE '2026-10-18', 'https://github.com/alexsiri7/musenmingle/issues/346')
ON CONFLICT (domain) DO NOTHING;
