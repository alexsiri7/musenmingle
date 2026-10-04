# Event Ingestion

## Purpose

Ingestion gathers London's exhibitions, expos, talks, workshops, community events and 'arty' music from APIs, venue sites and community calendars, and turns each listing into one consistent event record. It exists because the people Muse & Mingle serves should not have to check dozens of sites, and it must do so as a polite, predictable visitor to those sites.

## Requirements

### Requirement: Sources run on their own schedule

The system SHALL run ingestion on a fixed tick and, on each tick, SHALL run every enabled source whose own interval has elapsed, each within a time limit. Runs SHALL NOT overlap. A source that cannot be built (for example, missing credentials) SHALL be recorded as skipped with its reason and retried on the next tick, and SHALL NOT count as a failure.

#### Scenario: Daily source on a 15-minute tick
- GIVEN a source with a daily interval that last ran 3 hours ago
- WHEN an ingest tick starts
- THEN that source is not run on this tick
- AND sources whose interval has elapsed are run

#### Scenario: Missing API key
- GIVEN the Ticketmaster key is not configured
- WHEN an ingest tick runs
- THEN Ticketmaster is recorded as skipped with the reason "TICKETMASTER_API_KEY not set"
- AND no error is counted against it

#### Scenario: Overlapping ticks
- GIVEN an ingest run still in progress
- WHEN the next tick starts
- THEN the second run does not process any source

### Requirement: Sites are fetched politely

Every request the system makes to a source site SHALL identify itself as MuseNMingleBot with a link to the venue information on the About page, SHALL obey that site's robots.txt (including Crawl-delay), and SHALL respect a per-domain minimum interval between requests, 2 seconds by default, which configuration MAY raise but SHALL NOT lower below a site's built-in floor. Requests SHALL reach only public internet addresses over http(s), follow at most 3 redirects and read at most 10 MB of a body. Secrets in URLs SHALL NOT appear in logs or errors.

#### Scenario: robots.txt disallows a path
- GIVEN a venue whose robots.txt disallows `/events/`
- WHEN its source asks to fetch a page under `/events/`
- THEN the request is not sent and the fetch fails

#### Scenario: Redirect to a private address
- GIVEN a source page that redirects to a host resolving to a private IP address
- WHEN it is fetched
- THEN the redirect is refused

### Requirement: Extraction is deterministic

Each source SHALL extract events with deterministic code (structured data first, such as schema.org JSON-LD, embedded JSON or official feeds, then page selectors), and SHALL NOT use a language model to read or parse listings. Items outside Muse & Mingle's scope (online-only, outside London, film screenings, family-only sessions and similar exclusions each source documents) SHALL be skipped, and a skip SHALL NOT count as an error; only real failures SHALL count toward a run's errors.

#### Scenario: Online-only talk
- GIVEN a venue listing an online-only talk
- WHEN its source normalises the listing
- THEN no event is produced
- AND the run's error count is unchanged

### Requirement: Listings become consistent events

Every stored event SHALL have a title, a start in UTC derived from Europe/London wall-clock time, and one category: exhibition, expo, community, talk, workshop or music. Events given by date only SHALL be marked all-day, spanning London midnight of their first day to their last day inclusive. Prices SHALL be kept as a minimum and maximum with currency, and free events SHALL be recognised as free. A multi-session event, such as a fortnightly workshop series, SHALL be stored once with its list of sessions. Where a listing states weekly opening hours for a multi-day run, they SHALL be kept.

#### Scenario: Exhibition with dates only
- GIVEN a gallery listing "12 September – 30 November"
- WHEN it is ingested
- THEN the event is all-day, starting at London midnight on 12 September and running through 30 November

#### Scenario: Timed talk in summer time
- GIVEN a talk listed at 18:30 on a day in British Summer Time
- WHEN it is ingested
- THEN its start is stored as 17:30 UTC

### Requirement: New venues are configuration where a platform allows

Venues whose sites run a platform the system already reads (such as Artlogic galleries, The Events Calendar sites or Luma calendars) SHALL be added as a configuration entry for that platform, not as new code. Every other site SHALL get a hand-written scraper with a saved page fixture and a snapshot of its normalised output.

#### Scenario: Another Artlogic gallery
- GIVEN a London commercial gallery whose site runs Artlogic
- WHEN it is added to Muse & Mingle
- THEN it is covered by a new source configuration entry with its listing paths and London locations
- AND no new scraper code is written

### Requirement: Coverage spans the creative scene

The system SHALL cover the kinds of events creative Londoners go to: public museums and galleries, commercial and artist-run galleries, talks and bookshop events, workshops, community meetups, and the 'arty' end of live music (classical, contemporary, experimental, jazz and sound art). A source SHALL include every in-scope event its site lists, not only the ones in one section of the site.

#### Scenario: Venue with a concerts section
- GIVEN a venue whose site lists talks under one section and contemporary music concerts under another
- WHEN its source runs
- THEN both the talks and the concerts are ingested, the concerts under the music category
