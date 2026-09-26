---
created: '2026-09-26'
github_issue: null
id: '002'
status: draft
title: Anyone can suggest a site for Thaleia to cover
updated: '2026-09-26'
---

## Why

Much of London's creative scene (small galleries, writing groups, community meetups) has no API and isn't known to us. The people who go to these things know where they're listed. Letting them suggest sites is how coverage grows beyond what we find ourselves, and each suggestion should turn into scraper work without manual triage.

## What

Anyone can submit a website URL they think Thaleia should cover and gets an immediate answer: accepted, already covered, already suggested, or rejected as invalid.

- A site we already scrape, or one already waiting, isn't queued twice, however the URL is written (same domain, different path, http vs https, with or without www).
- One person can't flood the queue: submissions from the same source are rate-limited, with a clear "try again later".
- Each accepted suggestion becomes exactly one GitHub issue labelled `new-scraper`, carrying the URL and a checklist, so Archon can pick it up and write that site's scraper.
- Suggestions are never parsed automatically; a site only starts producing events once a hand-written scraper for it lands.

## Issues

_None yet._