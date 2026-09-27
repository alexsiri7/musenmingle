-- Seed the first Luma calendars (src/sources/luma.rs, platform 'luma'),
-- daily; issue #75. One row per calendar: `config` holds its `calendar_id`
-- and category rules (LumaConfig). Each is read ONLY through its official
-- iCal subscription feed (https://api2.luma.com/ics/get?entity=calendar&id=…),
-- never luma.com pages. Why each calendar was chosen: docs/luma-calendars.md.
--
-- kind 'aggregator': Luma is a third-party listing platform, so a venue's
-- own scraper wins a cross-source merge and the main link.
--
-- Terms (https://luma.com/terms, checked 2026-09-27): no reproducing site
-- content "except when such actions occur in connection with bona fide uses
-- of the Service through our publicly supported interfaces"; no downloading
-- or reuse of any image "as a stand-alone file"; no access "by any means
-- other than our publicly supported interfaces". So facts + link only.
-- robots.txt of api2.luma.com (checked 2026-09-27) disallows only /insights/.
--
-- Migrations are APPEND-ONLY: never edit this file once merged.

INSERT INTO events.sources (key, kind, base_url, domain, interval_minutes, enabled,
                            display_name, store_description, store_image, policy_note,
                            platform, config)
VALUES
    ('luma-new-media-london', 'aggregator', 'https://api2.luma.com', 'api2.luma.com', 1440, TRUE,
     'New Media London (Luma)', FALSE, FALSE,
     'Luma terms (https://luma.com/terms, checked 2026-09-27) allow reuse only "through our publicly '
         || 'supported interfaces" and forbid reusing images as stand-alone files: read via the '
         || 'official iCal feed, facts + link only',
     'luma',
     '{"calendar_id": "cal-fMoq9nuFiKXYCzi", "default_category": "community"}'::jsonb),
    ('luma-creative-ai-meetup', 'aggregator', 'https://api2.luma.com', 'api2.luma.com', 1440, TRUE,
     'Creative AI Meetup (Luma)', FALSE, FALSE,
     'Luma terms (https://luma.com/terms, checked 2026-09-27) allow reuse only "through our publicly '
         || 'supported interfaces" and forbid reusing images as stand-alone files: read via the '
         || 'official iCal feed, facts + link only',
     'luma',
     '{"calendar_id": "cal-bDC6E5p1xVynAEf", "default_category": "talk"}'::jsonb),
    ('luma-mason-and-fifth', 'aggregator', 'https://api2.luma.com', 'api2.luma.com', 1440, TRUE,
     'Mason & Fifth (Luma)', FALSE, FALSE,
     'Luma terms (https://luma.com/terms, checked 2026-09-27) allow reuse only "through our publicly '
         || 'supported interfaces" and forbid reusing images as stand-alone files: read via the '
         || 'official iCal feed, facts + link only',
     'luma',
     '{"calendar_id": "cal-mJXPpBb7tgosK3a", "skip_keywords": ["screening", "cinema", "listening", "sound bath", "party", "afterparty", "pop up", "retail", "running", "fitness", "yoga"]}'::jsonb),
    ('luma-for-writers', 'aggregator', 'https://api2.luma.com', 'api2.luma.com', 1440, TRUE,
     'For Writers (Luma)', FALSE, FALSE,
     'Luma terms (https://luma.com/terms, checked 2026-09-27) allow reuse only "through our publicly '
         || 'supported interfaces" and forbid reusing images as stand-alone files: read via the '
         || 'official iCal feed, facts + link only',
     'luma',
     '{"calendar_id": "cal-rc3wsjuGa6p18Et", "default_category": "workshop"}'::jsonb)
ON CONFLICT (key) DO NOTHING;
