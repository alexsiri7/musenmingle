---
created: '2026-09-27'
github_issue: 201
id: '004'
status: idea
title: Richer listings rise to the top when no order is asked for
updated: '2026-09-27'
---

## Why

Muse & Mingle is a visual discovery app for creative people. When someone browses without choosing an order, a page that opens on text-only listings feels thin, even when well-illustrated events are just below. Showing the events we know most about first makes browsing more visual and inviting, but it must not bury what's happening soon, and it must not override an order the person asked for.

## What

When a request for events doesn't ask for a particular order, results are grouped by day (Europe/London), soonest first. Within each day, the events we have the most information about come first.

- Richness counts, in order of weight: an image (by far the most), a description, a price (including "free"), and an exact venue or location.
- A multi-day exhibition belongs to the first day it's on within the requested window (today, if it's already open), not its original opening day.
- Timeliness wins over richness: an event tonight is never shown after one next week, however well-illustrated.
- An explicit order always wins. Asking for nearest first (the proximity filter) or any other order ignores richness entirely.
- The order is stable: the same data gives the same order across requests and pages, so paging never repeats or skips an event. Ties within a day fall back to start time, then a fixed identifier.

## Issues

- #201 — Default event order: group by day, richest listings first within each day