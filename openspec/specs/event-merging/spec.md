# Event Merging

## Purpose

The same exhibition or talk often appears in several sources with slightly different titles, times and venue names. Merging makes one real-world event one listing, with every source's link attached, so results are uncluttered and trustworthy.

## Requirements

### Requirement: One real-world event is one listing

When several sources describe the same real-world event, the system SHALL store and show it once, carrying every source that listed it with that source's own link, oldest source first. A listing SHALL join an existing event when their London dates agree (the same day, or overlapping ranges when both run over several days), their venues agree (the same or a contained name after normalisation, or coordinates within 150 m) and their titles agree after ignoring punctuation, capitalisation, leading articles, subtitles, years, venue words and filler such as "exhibition" or "tickets".

#### Scenario: Same exhibition, two sources
- GIVEN "The Turner Prize 2026" at "Tate Britain" from Ticketmaster
- AND "Turner Prize" at "Tate Britain, Millbank" from the venue's site on overlapping dates
- WHEN both are ingested
- THEN one event exists
- AND it lists both sources, each with its own link

### Requirement: Distinct events stay apart

The system SHALL NOT merge events on different dates, at different venues, or different instalments of a recurring series. Fuzzy matching SHALL NOT join two distinct listings from the same source. Every fuzzy merge SHALL be logged with both titles and the match scores.

#### Scenario: Monthly series
- GIVEN this month's CreativeMornings talk and last month's, at the same venue with similar titles
- WHEN both are ingested
- THEN they remain two events

#### Scenario: Two listings from one source
- GIVEN one source lists two talks at the same venue on the same evening with near-identical titles
- WHEN both are ingested
- THEN they remain two events

### Requirement: Disagreements resolve by trust, consistently

When merged sources disagree on a detail, the merged event SHALL take each field from the most trustworthy source for it, and the choice SHALL be the same on every run: the title stays as first seen; dates, description and image come from the venue's own site; price and primary link come from an API when it reports them; any other field keeps its existing value and a newcomer only fills gaps; tags are combined. A third-party listings site SHALL never win a field, only fill gaps, and pages SHALL link to the venue's own site before it.

#### Scenario: Venue and API disagree on time
- GIVEN Ticketmaster lists a talk at 19:00 and the venue's own site lists it at 18:30
- WHEN the event is merged
- THEN its start is 18:30
- AND re-running ingestion does not change it

### Requirement: Wrong merges can be corrected durably

An operator SHALL be able to declare that two source listings must never be merged, or must always be merged, and the correction SHALL survive every later ingestion run, taking effect the next time either listing is ingested. A never-merge correction SHALL hold even when the two listings would otherwise match exactly.

#### Scenario: Keep two talks apart
- GIVEN two different talks that were wrongly merged
- AND a never-merge correction naming both source listings
- WHEN either listing is next ingested
- THEN the talks are separate events
- AND later runs keep them separate
