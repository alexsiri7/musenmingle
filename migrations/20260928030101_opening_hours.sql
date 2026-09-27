-- Opening hours (issue #168). Exhibitions and other long-running events
-- often say when you can actually go ("on display Wednesdays, Thursdays,
-- Fridays & Sundays from 11am-3pm"); without it, "on now" and the
-- time-of-day filters treated an all-day run as open at any hour.
--
-- opening_hours: a weekly schedule in Europe/London wall-clock time,
--   [{"days": [3, 4, 5, 7], "opens": "11:00", "closes": "15:00"}]
--   (ISO weekdays, Monday = 1; opens < closes; days in one rule only).
--   Written by `repo::upsert_event` from `crate::hours` (deterministic
--   parsing of the listing's full description, never a model), only for
--   all-day events running more than one day. NULL = unknown: the listing
--   filters then keep their date-only behaviour.
-- hours_note: the listing's own sentence about its hours, shown verbatim;
--   only kept for sources whose content policy allows their descriptions.
--
-- events.venue_hours: a venue's usual hours, inherited by an all-day
-- exhibition there that states none (matched like events.venues, by
-- `normalise::normalise_venue_for_key` of `name`). Rows are added by hand in
-- new migrations, from the venue's own structured data.
--
-- No backfill: each ingest run re-derives hours for the events its sources
-- still list.
--
-- Migrations are APPEND-ONLY: never edit this file once merged.

ALTER TABLE events.events
    ADD COLUMN opening_hours JSONB CHECK (opening_hours IS NULL OR jsonb_typeof(opening_hours) = 'array'),
    ADD COLUMN hours_note TEXT CHECK (hours_note IS NULL OR length(hours_note) <= 200);

CREATE TABLE events.venue_hours (
    id            BIGSERIAL PRIMARY KEY,
    name          TEXT NOT NULL UNIQUE,
    opening_hours JSONB NOT NULL CHECK (jsonb_typeof(opening_hours) = 'array'),
    -- Where the hours came from, e.g. 'schema.org openingHours, slbi.org.uk 2026-09-27'.
    hours_source  TEXT NOT NULL,
    created_at    TIMESTAMPTZ NOT NULL DEFAULT now()
);

INSERT INTO events.venue_hours (name, opening_hours, hours_source) VALUES
    -- <meta itemprop="openingHours" datetime="Th 10:00-16:00"> and
    -- "Sa 10:00-14:00" in the footer of www.slbi.org.uk (the tec-slbi
    -- fixture, fetched 2026-09-27).
    ('South London Botanical Institute',
        '[{"days": [4], "opens": "10:00", "closes": "16:00"},
          {"days": [6], "opens": "10:00", "closes": "14:00"}]',
        'schema.org openingHours, www.slbi.org.uk 2026-09-27')
ON CONFLICT (name) DO NOTHING;
