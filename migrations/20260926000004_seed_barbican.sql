-- Seed the Barbican scraper (src/sources/barbican.rs), daily.
INSERT INTO events.sources (key, kind, base_url, domain, interval_minutes, enabled)
VALUES
    ('barbican', 'scraper', 'https://www.barbican.org.uk', 'www.barbican.org.uk', 1440, TRUE)
ON CONFLICT (key) DO NOTHING;
