-- Refuse White Cube (London exhibitions): its website terms of use
-- (https://www.whitecube.com/terms-of-use, last updated May 2023, checked
-- 2026-09-26) do not permit "Unauthorised text/data mining of Website content
-- and metadata" or "the substantial or repeated extraction and/or storage of
-- Website content in any retrieval system". A daily scraper is exactly that,
-- so facts + link only is not enough. Surveyed in issue #47.
--
-- Migrations are APPEND-ONLY: never edit this file once merged.

INSERT INTO events.refused_sources (domain, name, url, reason_code, reason_text, checked_on, issue_url)
VALUES ('whitecube.com', 'White Cube', 'https://www.whitecube.com/exhibitions/london',
        'terms',
        'its website terms of use don''t permit text/data mining or the repeated extraction and '
        || 'storage of site content (https://www.whitecube.com/terms-of-use)',
        DATE '2026-09-26', 'https://github.com/alexsiri7/thaleia/issues/47')
ON CONFLICT (domain) DO NOTHING;
