-- Seed the Wellcome Collection source (src/sources/wellcome_collection.rs):
-- Wellcome's own public Content API, so kind 'scraper' (the venue's own data,
-- ahead of third-party listings); no key, twice a day. api.wellcomecollection.org
-- has no robots.txt (404, i.e. allow all); wellcomecollection.org allows `/`.
-- Content policy: Wellcome's terms (https://wellcome.org/who-we-are/privacy-and-terms,
-- linked from the site footer) and the footer say content is CC BY 4.0 unless
-- otherwise noted (attribution + a link to the source page); event images carry
-- their own CC-BY or CC-BY-NC credit. Credited thumbnails linking to the event
-- page fit both while Muse & Mingle stays non-commercial (revisit store_image
-- if that changes).
INSERT INTO events.sources (key, kind, base_url, domain, interval_minutes, enabled,
                            display_name, store_description, store_image, policy_note)
VALUES ('wellcome-collection', 'scraper', 'https://api.wellcomecollection.org',
        'api.wellcomecollection.org', 720, TRUE,
        'Wellcome Collection', TRUE, TRUE,
        'Site content is CC BY 4.0 unless noted (terms '
            || 'https://wellcome.org/who-we-are/privacy-and-terms and the site footer, checked '
            || '2026-09-27); event images are CC-BY or CC-BY-NC with credits, so credited '
            || 'thumbnails linking back are fine while the site is non-commercial')
ON CONFLICT (key) DO NOTHING;
