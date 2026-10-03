-- Bow Arts (tec-bow-arts) dropped most of its programme; issue #247. Its
-- list view's JSON-LD has no categories, so only titles and descriptions
-- were matched, and exhibition titles are artist or show names
-- ("Bhajan Hunjan: speaking through materials") that match no keyword.
--   * default_category "exhibition": Bow Arts' programme is mainly
--     exhibitions, openings and open studios; talks and workshops still
--     win through their keywords.
--   * skip_keywords ["yoga"]: its yoga sessions are out of scope, as for
--     luma-mason-and-fifth.
-- Events without a location (an Educator CPD session) stay skipped: Bow
-- Arts runs events at several sites, so there is no venue to default to.
--
-- Migrations are APPEND-ONLY: never edit this file once merged.

UPDATE events.sources
   SET config = config || '{"default_category": "exhibition", "skip_keywords": ["yoga"]}'::jsonb
 WHERE key = 'tec-bow-arts';
