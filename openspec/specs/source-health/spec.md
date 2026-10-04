# Source Health

## Purpose

Scrapers break quietly when sites change. Source health makes every data source's state visible to anyone operating Muse & Mingle and turns breakage, and wrong data from runs that look clean, into repair work without anyone watching.

## Requirements

### Requirement: Every source's state is visible

The system SHALL list every configured source with its display name, kind, interval, whether it is enabled, its last real run (time, events found, errors, duration, success), any skip with its reason, its quality-check summary, and one status, first match wins: unconfigured while skipped; broken while a repair issue is open; pending if it never ran; degraded if its last run failed or had errors; otherwise healthy. A broken source SHALL link to its open repair issue. The same information SHALL be shown on a public Sources page, where each source links to its events.

#### Scenario: Broken source
- GIVEN a source with an open repair issue
- WHEN sources are listed
- THEN its status is broken
- AND it links to that issue

### Requirement: Breakage opens exactly one repair issue

After each run the system SHALL trip a source's health check when a successful run found no events although its recent average is above zero, when it had errors on two consecutive runs, or when its event count fell more than 60% below its recent average. A trip SHALL open exactly one repair issue titled "Scraper broken: <source>", deduplicated against both its own records and the open issues on GitHub, so a lost database does not duplicate it. When the source recovers the system SHALL comment on the issue and close it. Without GitHub access, trips SHALL be logged.

#### Scenario: Two failing runs
- GIVEN a source whose last run had errors
- WHEN its next run also has errors
- THEN one "Scraper broken" issue is opened
- AND a third failing run opens no further issue

#### Scenario: Recovery
- GIVEN a source with an open repair issue
- WHEN a later run succeeds normally
- THEN the issue is commented on and closed

### Requirement: Stored events match what the source says

Each source's stored events SHALL match what its pages say, and every in-scope event the pages list SHALL be ingested. After every successful run the system SHALL check the run's events against sanity rules without AI (ending before starting, dates far in the past or future, spans over a year, timed events at exactly midnight, same-title duplicates on one day, sudden jumps in missing venues or coordinates, many events on a single day, sharp count drops) and record the hits.

#### Scenario: Midnight start
- GIVEN a scraper stores a talk starting at exactly 00:00 London time
- WHEN its run is checked
- THEN a rule hit is recorded and shown with the source

### Requirement: An AI check compares extraction with the page

The system SHALL periodically compare a source's stored events with the pages its run fetched, using an AI judge: when the source was never checked, when its scraper code changed, a week after its last check, or when its latest run hit a rule the last check did not see. The check SHALL fetch at most four extra detail pages, SHALL store only page addresses, sizes, the verdict and short verbatim quotes (never page text), SHALL run within its own daily spending cap, and SHALL NEVER write event data. Wrong fields or missed events SHALL open one "Scraper check" repair issue per source listing stored value, what the page says and the quote; later checks SHALL comment on it and a clean check SHALL close it.

#### Scenario: Missed events
- GIVEN a source whose listing page shows seven events it did not store
- WHEN its AI check runs
- THEN one "Scraper check" issue is opened naming the missed events
- AND the stored events are unchanged
