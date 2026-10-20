-- National Art Pass venues (issue #317, spec change #313): the London
-- venues we list whose OWN website states an offer for National Art Pass
-- holders, each read once on 2026-10-11 with the MuseNMingleBot UA (robots.txt
-- obeyed, 2 s apart). Never from artfund.org: its terms forbid building a
-- database from it (#310). `offer` is in our words with the venue's figures;
-- `source_note` is the page that says it and the date it was read.
--
-- Checked and left out: the Royal Academy, the National Portrait Gallery,
-- Dulwich Picture Gallery and the Hayward Gallery (their sites answer our bot
-- with a 403; we don't evade blocks); the Design Museum, the V&A, the Science
-- Museum, Somerset House and Royal Museums Greenwich (no Art Pass offer on
-- the visit, FAQ or exhibition pages we read); Wellcome Collection and the
-- William Morris Gallery (free entry for everyone).
--
-- Venues are matched on their exact name, as the sources list them; rows
-- not created yet by `repo::sync_venues` are added (it fills them in), as in
-- the #71/#78 seeds. Add, change or withdraw offers in new migrations.
--
-- Migrations are APPEND-ONLY: never edit this file once merged.

WITH offers (venue, offer, source_note) AS (VALUES
    ('Tate Modern', '50% off exhibitions',
        'https://www.tate.org.uk/about-us/frequently-asked-questions 2026-10-11'),
    ('Tate Britain', '50% off exhibitions',
        'https://www.tate.org.uk/about-us/frequently-asked-questions 2026-10-11'),
    ('Barbican Centre', 'Reduced-price Art Gallery exhibition tickets (£11, standard £20.50)',
        'https://www.barbican.org.uk/whats-on/2026/event/robert-ryman-the-real-thing 2026-10-11'),
    ('Whitechapel Gallery', '50% off exhibition tickets (£8.25, standard £16.50)',
        'https://www.whitechapelgallery.org/visit/ 2026-10-11'),
    ('Estorick Collection', '50% off admission (£4.75, standard £9.50)',
        'https://www.estorickcollection.com/visitor-information 2026-10-11'),
    ('The Photographers'' Gallery', 'Reduced admission (£8, standard £14)',
        'https://thephotographersgallery.org.uk/visit 2026-10-11'),
    ('The Courtauld Gallery',
        'Free entry to the permanent collection and Drawings Gallery exhibitions',
        'https://courtauld.ac.uk/gallery/plan-your-visit/ 2026-10-11'),
    ('Old Royal Naval College', '50% off tickets (£9.50, standard £19)',
        'https://ornc.org/plan-a-visit/ 2026-10-11'),
    ('Garden Museum', '50% off admission (£8, standard £16)',
        'https://www.gardenmuseum.org.uk/visit/ 2026-10-11'),
    ('Handel Hendrix House', 'Free entry',
        'https://handelhendrix.org/visit 2026-10-11'),
    ('Foundling Museum', 'Free entry',
        'https://foundlingmuseum.org.uk/visit-us/ticketing-information/ 2026-10-11')
),
added AS (
    INSERT INTO events.venues (name)
    SELECT venue FROM offers
    ON CONFLICT (name) DO NOTHING
    RETURNING id, name
),
-- The statement can't see the rows `added` inserts except through RETURNING.
venues AS (
    SELECT id, name FROM added
    UNION ALL
    SELECT id, name FROM events.venues WHERE name IN (SELECT venue FROM offers)
)
INSERT INTO events.venue_memberships (venue_id, membership, offer, source_note)
SELECT v.id, 'art_pass', o.offer, o.source_note
FROM offers o JOIN venues v ON v.name = o.venue
ON CONFLICT (venue_id, membership) DO NOTHING;
