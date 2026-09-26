---
created: '2026-09-26'
github_issue: 3
id: '003'
status: idea
title: One real-world event, one listing
updated: '2026-09-26'
---

## Why

The same exhibition or talk often appears in several sources (Ticketmaster, the venue's own site, a listings site), each with slightly different titles, times and venue names. Showing it three times clutters results and makes Muse & Mingle feel untrustworthy; exact matching alone misses most of these duplicates.

## What

When several sources describe the same real-world event, the person sees it once, with every source's link attached.

- Small differences don't stop a match: punctuation, capitalisation, "The" prefixes, subtitles, or venue name variants ("Tate Modern" vs "Tate Modern, Bankside").
- Genuinely different events aren't merged: different dates, different venues, or different instalments of a recurring series (this month's CreativeMornings vs last month's) stay separate.
- When sources disagree on a detail, the merged listing uses the most trustworthy one, and the choice is consistent across runs.
- A wrongly merged or wrongly split event can be corrected, and the correction survives later ingestion runs.

## Issues

- #3 — Cross-source merge beyond exact dedupe_key: fuzzy title and venue matching