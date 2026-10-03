-- South London Botanical Institute (tec-slbi) dropped most of its
-- programme; issue #267. Its list view's JSON-LD has no categories, and
-- titles such as "Mushroom University", "SLBI Open Evening" or "Harvest
-- Festival: A Family Celebration of Plants" match no keyword.
--   * default_category "community": SLBI's programme is open days, family
--     activities and co-learning groups; workshops, talks and exhibitions
--     still win through their keywords.
--
-- Migrations are APPEND-ONLY: never edit this file once merged.

UPDATE events.sources
   SET config = config || '{"default_category": "community"}'::jsonb
 WHERE key = 'tec-slbi';
