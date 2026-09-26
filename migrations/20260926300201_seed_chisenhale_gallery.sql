-- Seed the Chisenhale Gallery scraper (src/sources/chisenhale_gallery.rs), daily.
-- robots.txt asks for Crawl-delay 20: the scraper fetches the listing only.
INSERT INTO events.sources (key, kind, base_url, domain, interval_minutes, enabled,
                            display_name, store_description, store_image, policy_note)
VALUES ('chisenhale-gallery', 'scraper', 'https://chisenhale.org.uk', 'chisenhale.org.uk', 1440, TRUE,
        'Chisenhale Gallery', TRUE, TRUE,
        'No website terms of use found (footer links only cookies, ethics, safeguarding and privacy '
        || 'policies); short excerpt and a credited thumbnail (https://chisenhale.org.uk/whats-on/, '
        || 'checked 2026-09-26)')
ON CONFLICT (key) DO NOTHING;
