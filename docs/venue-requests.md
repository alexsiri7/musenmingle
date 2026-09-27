# Venue requests

Venues and site owners use the **contact form** on the site (`/contact`,
linked from `/about`, the footer and the "Sites we couldn't use" note on
`/sources`; code in `src/contact.rs`). No GitHub account or email address is
needed. Each request:

- is stored in `events.contact_requests` (type, URL, registrable domain,
  details, optional reply email, salted IP hash, status, issue number);
- arrives as a GitHub issue labelled **`venue-request`**, titled
  "Venue request: <domain> (<type>)", filed by the API with the server-side
  `GITHUB_TOKEN`. The issue references the row as "contact request #<id>".
  **The reply email is never put in the issue**: look it up with
  `SELECT reply_email FROM events.contact_requests WHERE id = <id>;`;
- is added as a comment to the earlier issue when the same domain and type
  were requested within 7 days;
- stays `pending_issue` when GitHub is not configured or fails, and the next
  ingest run files it (the visitor sees "Thanks" either way);
- also waits as `pending_issue` when the site's forms have used up the day's
  GitHub issue cap (`FORM_ISSUES_PER_DAY`, default 20); it is filed, oldest
  first, on a later day, and meanwhile the owner gets an ntfy digest once a
  day.

Spam protection: a honeypot field, a signed form timestamp (submissions
under 3 s are dropped), 3 requests per hour / 10 per day per IP hash (an
IPv6 client's whole /64 counts as one IP), and a 16 KB body limit. No
CAPTCHA and no third-party scripts.

The About page promises that we act on every request and **remove a venue's
listings within 7 days**, so the owner checks the `venue-request` label at
least **weekly**. Reply (if an email was given) from your own mail client,
then note it on the issue.

(`.github/ISSUE_TEMPLATE/venue-request.md` stays for internal use; it is not
linked from public pages because the repository is private.)

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

-- Events only this venue lists go (their links and thumbnails cascade).
DELETE FROM events.events e
 WHERE EXISTS (SELECT 1 FROM events.event_sources es JOIN events.sources s ON s.id = es.source_id
               WHERE es.event_id = e.id AND s.key = '<key>')
   AND NOT EXISTS (SELECT 1 FROM events.event_sources es JOIN events.sources s ON s.id = es.source_id
                   WHERE es.event_id = e.id AND s.key <> '<key>');
-- Events other sources also list stay, without this venue's text or image
-- (the other sources refill theirs on their next run) and without its link.
UPDATE events.events e SET description = NULL, image_url = NULL, image_source_id = NULL
 WHERE EXISTS (SELECT 1 FROM events.event_sources es JOIN events.sources s ON s.id = es.source_id
               WHERE es.event_id = e.id AND s.key = '<key>');
DELETE FROM events.event_sources
 WHERE source_id = (SELECT id FROM events.sources WHERE key = '<key>');

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
