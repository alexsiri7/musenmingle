-- Seed the London Review Bookshop scraper (src/sources/london_review_bookshop.rs), daily.
INSERT INTO events.sources (key, kind, base_url, domain, interval_minutes, enabled,
                            display_name, store_description, store_image, policy_note)
VALUES ('london-review-bookshop', 'scraper', 'https://www.londonreviewbookshop.co.uk',
        'www.londonreviewbookshop.co.uk', 1440, TRUE,
        'London Review Bookshop', FALSE, FALSE,
        'The LRB terms (https://www.lrb.co.uk/terms, which cover londonreviewbookshop.co.uk; '
            || 'checked 2026-09-27) forbid storing or republishing LRB material and images without '
            || 'written permission, but don''t restrict event facts; so facts + link only')
ON CONFLICT (key) DO NOTHING;
