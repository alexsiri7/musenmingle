-- Seed the Wellcome Collection source (src/sources/wellcome_collection.rs):
-- its public Content API, no key, twice a day. api.wellcomecollection.org has
-- no robots.txt (404, i.e. allow all); wellcomecollection.org allows `/`.
-- Content policy: the site's footer says "Except where otherwise noted,
-- content on this site is licensed under a Creative Commons Attribution 4.0
-- International Licence", and the event images carry their own CC-BY or
-- CC-BY-NC credit: credited thumbnails linking to the event page fit both
-- while Muse & Mingle stays non-commercial (revisit store_image if that changes).
INSERT INTO events.sources (key, kind, base_url, domain, interval_minutes, enabled,
                            display_name, store_description, store_image, policy_note)
VALUES ('wellcome-collection', 'api', 'https://api.wellcomecollection.org',
        'api.wellcomecollection.org', 720, TRUE,
        'Wellcome Collection', TRUE, TRUE,
        'Site content is CC BY 4.0 unless noted (footer of https://wellcomecollection.org, '
            || 'checked 2026-09-27); event images are CC-BY or CC-BY-NC with credits, so '
            || 'credited thumbnails linking back are fine while the site is non-commercial')
ON CONFLICT (key) DO NOTHING;
