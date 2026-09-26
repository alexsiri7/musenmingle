-- Seed the Whitechapel Gallery scraper (src/sources/whitechapel_gallery.rs), daily.
INSERT INTO events.sources (key, kind, base_url, domain, interval_minutes, enabled)
VALUES
    ('whitechapel-gallery', 'scraper', 'https://www.whitechapelgallery.org', 'www.whitechapelgallery.org', 1440, TRUE)
ON CONFLICT (key) DO NOTHING;
