-- Seed the Mall Galleries scraper (src/sources/mall_galleries.rs), daily:
-- the art-society annual exhibitions at the Mall Galleries.
INSERT INTO events.sources (key, kind, base_url, domain, interval_minutes, enabled,
                            display_name, store_description, store_image, policy_note)
VALUES ('mall-galleries', 'scraper', 'https://www.mallgalleries.org.uk', 'www.mallgalleries.org.uk',
        1440, TRUE, 'Mall Galleries', TRUE, TRUE,
        'Terms found cover artwork sales, calls for entries, Friends membership and the bookshop '
            || 'only, nothing restricting reuse of listings; short excerpt and a credited thumbnail '
            || '(https://www.mallgalleries.org.uk/terms-and-conditions, checked 2026-09-26)')
ON CONFLICT (key) DO NOTHING;
