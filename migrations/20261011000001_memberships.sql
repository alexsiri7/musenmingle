-- Memberships (spec change #313, issue #315): find the events a membership
-- such as the National Art Pass gets you into free or for less.
--
-- * events.venue_memberships: the venues where a membership applies, with
--   the offer in words and where that fact came from (the venue's own page
--   and the date it was read, or the author). Kept by hand: rows are added
--   by migration only, never scraped from the membership provider (Art
--   Fund's terms forbid building a database from artfund.org, #310).
-- * events.event_memberships: the memberships each event matches, derived
--   by `crate::memberships::matches` (its venue's rows here, else its
--   source tags and stored excerpt naming the membership) and replaced by
--   `repo::sync_memberships` after each ingest run. `offer` is the venue's
--   offer, NULL when the event matched only by its text.
--
-- `membership` is a key of `crate::memberships::MEMBERSHIPS`; adding a
-- membership means widening both CHECKs in a new migration.
--
-- Migrations are APPEND-ONLY: never edit this file once merged.

CREATE TABLE events.venue_memberships (
    venue_id    BIGINT NOT NULL REFERENCES events.venues (id) ON DELETE CASCADE,
    membership  TEXT   NOT NULL CHECK (membership IN ('art_pass')),
    offer       TEXT   NOT NULL CHECK (btrim(offer) <> ''),
    -- e.g. 'https://www.tate.org.uk/visit/tate-modern 2026-10-10'.
    source_note TEXT   NOT NULL CHECK (btrim(source_note) <> ''),
    added_on    DATE   NOT NULL DEFAULT CURRENT_DATE,
    PRIMARY KEY (venue_id, membership)
);

CREATE TABLE events.event_memberships (
    event_id   UUID NOT NULL REFERENCES events.events (id) ON DELETE CASCADE,
    membership TEXT NOT NULL CHECK (membership IN ('art_pass')),
    offer      TEXT,
    PRIMARY KEY (event_id, membership)
);
