-- Refuse the National Gallery (exhibitions): its website terms of use
-- (https://www.nationalgallery.org.uk/terms-of-use, checked 2026-09-26) list
-- as prohibited use "creating a database that includes material downloaded or
-- obtained from the Website without written permission from the Gallery", and
-- allow copying content only for "your own personal, non-commercial use".
-- A scraper would do exactly that, so facts + link only is not enough.
-- Surveyed in issue #47.
--
-- Migrations are APPEND-ONLY: never edit this file once merged.

INSERT INTO events.refused_sources (domain, name, url, reason_code, reason_text, checked_on, issue_url)
VALUES ('nationalgallery.org.uk', 'National Gallery', 'https://www.nationalgallery.org.uk/exhibitions',
        'terms',
        'its website terms of use forbid creating a database of material obtained from the site '
        || 'without the Gallery''s written permission (https://www.nationalgallery.org.uk/terms-of-use)',
        DATE '2026-09-26', 'https://github.com/alexsiri7/thaleia/issues/47')
ON CONFLICT (domain) DO NOTHING;
