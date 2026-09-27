-- Full-text search (issue #77): a generated, weighted tsvector on
-- events.events with a GIN index. No extensions: accents are folded by
-- events.search_fold (a fixed translate() table, IMMUTABLE, so a generated
-- column may use it; unaccent() is only STABLE and would need an
-- extension). Typo tolerance is done in Rust (src/search.rs) against a
-- word list read from these same folded columns.
--
-- Weights: A = title, B = venue name, C = description excerpt, category and
-- tags (source tags and the AI/default medium, format and good-for tags).
--
-- The query side must fold the same way: events.search_fold in SQL, and
-- crate::search::fold in Rust (tests check they agree).
--
-- Migrations are APPEND-ONLY: never edit this file once merged.

CREATE FUNCTION events.search_fold(t TEXT) RETURNS TEXT
    LANGUAGE sql IMMUTABLE PARALLEL SAFE
AS $fold$
    SELECT lower(translate(
        replace(replace(replace(replace(replace(replace(replace(replace(replace(replace(replace(replace(coalesce(t, ''),
            'ß', 'ss'), 'ẞ', 'SS'), 'æ', 'ae'), 'Æ', 'AE'), 'œ', 'oe'), 'Œ', 'OE'),
            'ø', 'o'), 'Ø', 'O'), 'þ', 'th'), 'Þ', 'TH'), 'ð', 'd'), 'Ð', 'D'),
        'áÁàÀâÂäÄãÃåÅāĀăĂąĄçÇćĆĉĈčČďĎđĐéÉèÈêÊëËēĒĕĔėĖęĘěĚĝĜğĞġĠģĢĥĤħĦíÍìÌîÎïÏĩĨīĪĭĬįĮıIĵĴķĶĺĹļĻľĽŀĿłŁñÑńŃņŅňŇóÓòÒôÔöÖõÕōŌŏŎőŐŕŔŗŖřŘśŚŝŜşŞšŠșȘţŢťŤțȚúÚùÙûÛüÜũŨūŪŭŬůŮűŰųŲŵŴýÝÿŸŷŶźŹżŻžŽʼ’‘',
        'aaaaaaaaaaaaaaaaaaccccccccddddeeeeeeeeeeeeeeeeeegggggggghhhhiiiiiiiiiiiiiiiiiijjkkllllllllllnnnnnnnnoooooooooooooooorrrrrrssssssssssttttttuuuuuuuuuuuuuuuuuuuuwwyyyyyyzzzzzz'''''''
    ))
$fold$;

-- Words of a tag array (array_to_string is only STABLE for anyarray; for
-- text[] it is immutable in practice).
CREATE FUNCTION events.search_words(a TEXT[]) RETURNS TEXT
    LANGUAGE sql IMMUTABLE PARALLEL SAFE
AS $words$
    SELECT coalesce(array_to_string(a, ' '), '')
$words$;

ALTER TABLE events.events ADD COLUMN search tsvector GENERATED ALWAYS AS (
    setweight(to_tsvector('pg_catalog.english'::regconfig, events.search_fold(title)), 'A')
    || setweight(to_tsvector('pg_catalog.english'::regconfig, events.search_fold(venue_name)), 'B')
    || setweight(to_tsvector('pg_catalog.english'::regconfig, events.search_fold(
        coalesce(description, '') || ' ' || category || ' '
        || events.search_words(tags) || ' ' || events.search_words(medium_tags) || ' '
        || events.search_words(format_tags) || ' ' || events.search_words(good_for))), 'C')
) STORED;

CREATE INDEX events_search_idx ON events.events USING gin (search);
