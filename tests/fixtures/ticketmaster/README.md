# Ticketmaster Discovery API fixtures

`events-page-0.json` / `events-page-1.json` are **hand-built** responses that
follow the documented shape of `GET /discovery/v2/events.json` (no API key was
available when this source was written). They exercise: pagination
(`page.totalPages`), UTC `dateTime` vs `localDate`/`localTime`-only starts,
multi-day ranges (`dates.end`), string lat/lng, price ranges, a free event,
HTML in descriptions, 16:9 image selection, an out-of-scope classification
(Theatre/Musical, skipped) and an `Undefined` classification falling back to
title keywords.

When a real key is available, replace these with a real (trimmed) response
captured with the bot and update the snapshots with
`INSTA_UPDATE=always cargo test --test source_ticketmaster` after reviewing
the diff.
