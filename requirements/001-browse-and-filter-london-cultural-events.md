---
created: '2026-09-26'
github_issue: 1
id: '001'
status: done
title: Browse and filter London cultural events
updated: '2026-09-28'
---

## Why

Muse & Mingle exists to help creative people in London find exhibitions, expos and community events (talks, workshops, writing groups, CreativeMornings) they'll find interesting. Ingested events are worthless until something can ask for them, and the upcoming frontend needs a stable way to do so.

## What

A client can ask Muse & Mingle for events and get back only the ones that match what the person is looking for:
- A date window. A multi-week exhibition shows up for any window it overlaps, not just its opening day; a one-off event shows up if it happens inside the window.
- One or more categories: exhibition, expo, community, talk, workshop, music (the 'arty' end: classical, contemporary, experimental, jazz, sound art, each a subtag that can be filtered on).
- Free events only.
- Near a point: events within a given radius, nearest first.

Filters combine freely. Results come back in pages, so long lists are cheap to scroll.

Each event shows every place it was found (for example, both Ticketmaster and the venue's own site), so the person can follow whichever link they prefer. A single event can be fetched by id; an unknown id is a clear "not found", not an error.

Anyone operating Muse & Mingle can see every data source and whether it's healthy, degraded or broken, with a link to the open repair issue when it's broken.

The response format is documented with examples and doesn't change field names without notice, and only allowed frontends can call it from a browser.

## Issues

- #1 — Read API: list events with filters, event by id, sources with health