-- Seed the ArtRabbit listing scraper (src/sources/artrabbit.rs), daily.
--
-- ArtRabbit's terms forbid "reproducing, copying ... or incorporating into
-- any other materials, any of the Website". The owner decided to include it
-- respectfully with facts + a link only: no description, no image (and so no
-- raw payload either, see repo::SourcePolicy::restricted). The scraper reads
-- only the listing pages, at most 20 per run, one request per 5 s
-- (config::BUILTIN_MIN_INTERVALS).
--
-- ArtRabbit is a third-party listing site, not the venue's own site, so it
-- gets a new source kind: an `aggregator` never takes precedence in a merge
-- (it only fills gaps) and pages link to the venue's own site before it.
--
-- Migrations are APPEND-ONLY: never edit this file once merged.
ALTER TABLE events.sources DROP CONSTRAINT sources_kind_check;
ALTER TABLE events.sources ADD CONSTRAINT sources_kind_check
    CHECK (kind IN ('api', 'scraper', 'aggregator'));

INSERT INTO events.sources (key, kind, base_url, domain, interval_minutes, enabled,
                            display_name, store_description, store_image, policy_note)
VALUES ('artrabbit', 'aggregator', 'https://www.artrabbit.com', 'www.artrabbit.com', 1440, TRUE,
        'ArtRabbit', FALSE, FALSE,
        'Terms prohibit "reproducing, copying, editing, transmitting, uploading or incorporating '
        || 'into any other materials, any of the Website, including without limitation, any '
        || 'information, articles, photographs, images or submissions": facts + link only, from '
        || 'the listing pages only (https://www.artrabbit.com/about/terms, checked 2026-09-26)')
ON CONFLICT (key) DO NOTHING;
