-- Seed the Design Museum scraper (src/sources/design_museum.rs), daily.
INSERT INTO events.sources (key, kind, base_url, domain, interval_minutes, enabled)
VALUES
    ('design-museum', 'scraper', 'https://designmuseum.org', 'designmuseum.org', 1440, TRUE)
ON CONFLICT (key) DO NOTHING;
