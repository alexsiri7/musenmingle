# Venue requests

Venues and site owners reach us through the **Venue request** issue template
(`.github/ISSUE_TEMPLATE/venue-request.md`, label `venue-request`), linked
from the public About page (`/about#contact`). The About page promises that
we act on every request and **remove a venue's listings within 7 days**, so
check the `venue-request` label at least weekly.

> TODO(owner): add a contact email address so venues don't need a GitHub
> account. When it exists, add it to `/about#contact` (`src/web.rs`,
> `about`) and to the issue template, and list here who reads it.

Every change below is a normal PR with a **new** migration (migrations are
append-only); it takes effect when Railway deploys `main` (the API applies
migrations on start) and the next ingest tick (≤ 15 min) runs
`repo::enforce_content_policy`.

## Stop listing our events (removal)

One migration, e.g. `migrations/<timestamp>_owner_request_<key>.sql`:

```sql
-- Owner request: <venue>, <issue URL>.
UPDATE events.sources
   SET enabled = FALSE, store_description = FALSE, store_image = FALSE,
       policy_note = 'Owner asked us not to list their events (<issue URL>, <date>)'
 WHERE key = '<key>';

-- Events that other sources also list stay, without this venue's text or image
-- (the other sources refill theirs on their next run).
UPDATE events.events e SET description = NULL, image_url = NULL, image_source_id = NULL
 WHERE EXISTS (SELECT 1 FROM events.event_sources es JOIN events.sources s ON s.id = es.source_id
               WHERE es.event_id = e.id AND s.key = '<key>');
DELETE FROM events.event_sources
 WHERE source_id = (SELECT id FROM events.sources WHERE key = '<key>');
-- Events nothing else lists go (thumbnails cascade).
DELETE FROM events.events e
 WHERE NOT EXISTS (SELECT 1 FROM events.event_sources es WHERE es.event_id = e.id);

INSERT INTO events.refused_sources (domain, name, url, reason_code, reason_text, checked_on, issue_url)
VALUES ('<registrable domain>', '<Venue name>', '<https://… events page>', 'owner_request',
        'the venue asked us not to list their events, so we don''t', DATE '<today>',
        '<issue URL>');
```

The venue then shows under "Sites we couldn't use" on `/sources`, and new
suggestions for its domain are answered with that reason instead of filing a
`new-scraper` issue. Keep the scraper code (it is disabled by `enabled =
FALSE`); remove it in a later clean-up PR if you like.

If the venue only appears through another source (e.g. Ticketmaster), there is
no source to disable: delete its events in the migration by `venue_name`, and
make that source skip the venue from then on (return `Ok(None)` from its
`normalise` for that venue, with a test), in the same PR.

## Stop using our images / descriptions

Set the flag(s) on the source in a migration, with a `policy_note`:

```sql
UPDATE events.sources SET store_image = FALSE,
       policy_note = 'Owner asked us not to use their images (<issue URL>, <date>)'
 WHERE key = '<key>';
```

The next ingest tick clears the stored images and deletes their thumbnails
(`repo::enforce_content_policy`); `/thumbs/...` stops serving them at once.
`store_description = FALSE` works the same way for descriptions.

## Corrections

Wrong dates, venue or price usually mean a scraper bug: fix the parser, add
the page as a fixture and update the snapshot (see
`docs/adding-a-scraper.md`). The source re-reports the event on its next run
and its values replace the old ones. For a bad merge of two events use
`events.merge_overrides` (see README, "Merging and overrides").

## Closing the issue

Reply on the issue with what changed and the PR link, then close it once the
change is deployed and verified on the live site.
