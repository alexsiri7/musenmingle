-- Refuse Art Fund (artfund.org, the UK-wide exhibitions listing behind the
-- National Art Pass): its website terms of use
-- (https://www.artfund.org/terms-and-conditions/use-of-this-website-and-mobile-applications,
-- checked 2026-10-10) §2 forbid using the site "to create a database
-- (electronic or otherwise) that includes material downloaded or otherwise
-- obtained from the Website". A daily scraper is exactly that, so facts +
-- link only is not enough. The National Art Pass offers are kept by hand in
-- events.venue_memberships from each venue's own page instead. Issue #310.
--
-- Migrations are APPEND-ONLY: never edit this file once merged.

INSERT INTO events.refused_sources (domain, name, url, reason_code, reason_text, checked_on, issue_url)
VALUES ('artfund.org', 'Art Fund', 'https://www.artfund.org/explore/exhibitions',
        'terms',
        'its website terms of use forbid using the site to create a database that includes '
        || 'material obtained from it (https://www.artfund.org/terms-and-conditions/use-of-this-website-and-mobile-applications)',
        DATE '2026-10-10', 'https://github.com/alexsiri7/musenmingle/issues/310')
ON CONFLICT (domain) DO NOTHING;
