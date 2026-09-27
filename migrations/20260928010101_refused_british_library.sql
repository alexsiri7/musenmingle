-- Refuse the British Library (events.bl.uk, NW1): checked 2026-09-27 with
-- the MuseNMingleBot UA. robots.txt allows the event and exhibition pages
-- (it disallows only /cpresources/, /vendor/, /.env and /cache/), and the
-- event pages carry JSON-LD `Event` markup. But the events site's footer
-- links the British Library's terms (https://www.bl.uk/terms), whose
-- "Websites and Online Services" section covers bl.uk "and all associated
-- pages, domains and online services", and whose "Prohibited use" clause
-- says: "You agree not to use the Website: to create a database
-- (electronic or otherwise) that includes material downloaded or otherwise
-- obtained from the Website except where expressly permitted on the
-- Website or otherwise permitted in writing". That rules out even facts +
-- link, as for the National Gallery (same clause). Issue #35.
--
-- Migrations are APPEND-ONLY: never edit this file once merged.

INSERT INTO events.refused_sources (domain, name, url, reason_code, reason_text, checked_on, issue_url)
VALUES ('bl.uk', 'British Library', 'https://events.bl.uk/',
        'terms',
        'its website terms forbid creating a database that includes material obtained from its '
        || 'websites without the Library''s written permission (https://www.bl.uk/terms)',
        DATE '2026-09-27', 'https://github.com/alexsiri7/musenmingle/issues/35')
ON CONFLICT (domain) DO NOTHING;
