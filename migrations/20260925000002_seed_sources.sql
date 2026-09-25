-- Seed the sources implemented in src/sources/. Enabling/disabling and the
-- per-source interval are data, so operators can change them without a deploy.
INSERT INTO events.sources (key, kind, base_url, domain, interval_minutes, enabled)
VALUES
    ('ticketmaster', 'api', 'https://app.ticketmaster.com', 'app.ticketmaster.com', 360, TRUE),
    ('serpentine-galleries', 'scraper', 'https://www.serpentinegalleries.org', 'www.serpentinegalleries.org', 1440, TRUE)
ON CONFLICT (key) DO NOTHING;
