-- Refuse Leighton House & Sambourne House (the RBKC museums, W14 / W8):
-- checked 2026-09-27 with the MuseNMingleBot UA. robots.txt allows
-- /museums/whats-on and the event pages (only core/profiles/admin paths are
-- disallowed), and the listing is server-rendered Drupal, but the museums'
-- terms of use (https://www.rbkc.gov.uk/museums/terms-use, "Copyright and
-- intellectual property") say: "You can copy any information for your own
-- personal use. But you may not republish, transmit, store, reproduce,
-- communicate or make available to the public any of the information on this
-- website." That covers event facts too, so even facts + link is ruled out
-- (as with ArtRabbit, #96). Surveyed in issue #44.
--
-- The domain is the council's registrable domain, which hosts the museums'
-- site under /museums.
--
-- Migrations are APPEND-ONLY: never edit this file once merged.

INSERT INTO events.refused_sources (domain, name, url, reason_code, reason_text, checked_on, issue_url)
VALUES ('rbkc.gov.uk', 'Leighton House & Sambourne House',
        'https://www.rbkc.gov.uk/museums/whats-on',
        'terms',
        'the museums'' terms of use don''t allow republishing, storing or making available any '
        || 'of the information on their website, even basic event facts '
        || '(https://www.rbkc.gov.uk/museums/terms-use)',
        DATE '2026-09-27', 'https://github.com/alexsiri7/musenmingle/issues/44')
ON CONFLICT (domain) DO NOTHING;
