# AI Enrichment

## Purpose

Listings say little about why an event is worth going to or who it suits. AI enrichment adds tags and a short, clearly labelled note from what we already hold, so people can filter by medium, format and mood and find similar events, without letting a model supply facts or see visitors' data.

## Requirements

### Requirement: Enrichment adds labelled tags and notes after ingest

After each ingest run the system SHALL add to upcoming events tags from fixed vocabularies (medium, format, good for, vibe), named artists, whether it is an opening, a short "what's cool" note (at most 220 characters, British English, no hype) and a one-line summary. Its input SHALL be only what is stored about the event plus, for listings scraped in the same run from sources that allow descriptions, their cleaned page text, which SHALL be dropped after the run and never stored, logged or cached. It SHALL NEVER fetch pages or receive visitor data. AI text SHALL always be labelled as AI-written on pages and in the API, and SHALL NEVER be presented as the venue's words.

#### Scenario: Event page with a note
- GIVEN an enriched event
- WHEN its page is shown
- THEN the note appears labelled "✨ AI note" with a link to how Muse & Mingle uses AI

#### Scenario: Facts-only source
- GIVEN an event from a source whose terms forbid keeping descriptions
- WHEN it is enriched
- THEN only its stored facts are sent

### Requirement: Output is validated and grounded

Every enrichment answer SHALL be validated strictly: tags from the fixed vocabularies within their limits, lengths respected, and artists and opening evidence quoted verbatim from the input. An invalid answer SHALL be retried once with the complaints, then given up with no partial writes and not re-sent until its input or the prompt changes. When the listing says too little, the note SHALL be left empty rather than invented.

#### Scenario: Invented artist
- GIVEN an answer naming an artist absent from the input
- WHEN it is validated
- THEN it is rejected and retried once

### Requirement: Notes never outlive their facts

When an event's stored facts change, its AI fields SHALL be withdrawn at once and the event re-enriched; an event SHALL also be re-enriched when its page text or the prompt version changes.

#### Scenario: Date corrected
- GIVEN an enriched event whose date is corrected by its source
- WHEN the next ingest run stores the change
- THEN its AI note is removed until it is re-enriched

### Requirement: Spending is capped and alerts are raised

Every AI call, failed ones included, SHALL be recorded with its tokens and cost. A call SHALL be made only when its worst-case cost fits under the daily cap per London day and the per-run cap, and the pass SHALL stay within a time budget. Only models with zero data retention SHALL be used for chat. When the provider reports exhausted credits, the pass SHALL stop and the owner SHALL get one alert per day while it lasts and one when it recovers.

#### Scenario: Daily cap reached
- GIVEN the day's AI spending is at its cap
- WHEN an ingest run reaches the enrichment pass
- THEN no enrichment call is made until the next London day

### Requirement: Similar events are found by meaning

Enriched events SHALL be embedded as vectors from a documented text template, re-embedded when that text changes, and used to offer "More like this" on event pages. Embeddings SHALL be optional: without them, pages SHALL work and simply show no similar events.

#### Scenario: Embeddings unavailable
- GIVEN the database does not allow vectors
- WHEN an event page is shown
- THEN it renders without a "More like this" section
