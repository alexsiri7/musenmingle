---
created: '2026-09-28'
github_issue: null
id: '005'
status: draft
title: Adopt an OpenSpec specification as the statement of what Muse & Mingle should
  be
updated: '2026-09-28'
---

## Why

Requirement files record changes, not what the project should be. Auditing Muse & Mingle against them means replaying a change log, and hand-kept status drifts from the real state of the work. A spec per capability, changed only through spec-change pull requests, gives one document to audit against and lets Lachesis derive status from the work itself. This is the same move Lachesis made in its own requirement 030.

## What

The repository holds its specification in OpenSpec format under openspec/specs/, one file per capability: ai-enrichment, content-policy, event-api, event-ingestion, event-merging, event-ordering, site-suggestions, source-health, venues-and-places and web-experience. The specs are validated in CI. From then on, work starts as spec changes, and the existing requirement files are superseded by the spec.

## Issues

_None yet._