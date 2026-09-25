-- Seed the Somerset House scraper (src/sources/somerset_house.rs), daily.
INSERT INTO events.sources (key, kind, base_url, domain, interval_minutes, enabled)
VALUES
    ('somerset-house', 'scraper', 'https://www.somersethouse.org.uk', 'www.somersethouse.org.uk', 1440, TRUE)
ON CONFLICT (key) DO NOTHING;
