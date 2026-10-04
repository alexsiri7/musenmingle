# Event API

## Purpose

Ingested events are worthless until something can ask for them. The read API gives frontends, and Muse & Mingle's own pages, a stable, documented way to find the events that match what a person is looking for.

## Requirements

### Requirement: Events are filtered by what the person wants

Listing events SHALL accept optional filters that combine freely: a London date window, one or more categories, free only, a maximum price, time of day (evening, after work, weekend, daytime), a quick pick (open now, tonight, openings, last chance, hands-on), sources, specific event ids, AI tags (medium, format, good for), venue type, London borough, music subtag, and a point with a radius or a walking time. An event with an end SHALL match a date window it overlaps; one without an end SHALL match when it starts inside the window; a multi-session event SHALL match on its sessions, not the days between them. Events with known weekly opening hours SHALL be judged "on now" or "evening" by those hours. Unknown parameters and invalid values SHALL be rejected with a clear 400 error.

#### Scenario: Long exhibition in a later window
- GIVEN an exhibition running 1 September to 30 November
- WHEN events are listed for 1 to 5 October
- THEN the exhibition is included

#### Scenario: Free talks near a point
- GIVEN talks and workshops across London
- WHEN events are listed with category talk, free only, and within 3 km of Charing Cross
- THEN only free talks within 3 km are returned, each with its distance

#### Scenario: Closed on Mondays
- GIVEN an exhibition whose opening hours exclude Mondays
- WHEN events on now are listed on a Monday evening
- THEN the exhibition is not included

### Requirement: Free-text search

Listing events SHALL accept a search query that matches titles most strongly, then venue names, then excerpts, category and tags, with English stemming, accents ignored, the last word matched as a prefix, and quoted phrases, "or" and exclusions supported. A word that matches no upcoming event SHALL be corrected to the nearest word in upcoming events' titles and venue names, and the response SHALL say what it was corrected to.

#### Scenario: Misspelt venue
- GIVEN upcoming events at Whitechapel Gallery
- WHEN events are searched for "Whitechaple"
- THEN those events are returned
- AND the response says the query was corrected to "whitechapel"

### Requirement: Results come in stable pages with counts

Results SHALL be returned in pages of a requested size (up to 100) with an opaque cursor for the next page, and paging through a result set SHALL return every matching event exactly once. A cursor SHALL be valid only for the order that issued it. On request, the response SHALL include per-option counts for time of day and price, and facet counts for tags, venue type, borough and music subtag, each ignoring its own selection so it says how many events choosing that option would show.

#### Scenario: Paging a long list
- GIVEN 120 matching events
- WHEN they are read 50 at a time following each next cursor
- THEN every event appears exactly once and the last page has no next cursor

#### Scenario: Facet ignores its own selection
- GIVEN a listing filtered to medium photography
- WHEN facets are requested
- THEN the medium facet counts events for every medium, as if photography were not selected

### Requirement: Each event carries every place it was found

Every event in a response SHALL list each source that found it, with that source's display name, its link, and when it was first and last seen, so the person can follow whichever link they prefer. A single event SHALL be retrievable by id, and an unknown or malformed id SHALL return a clear "not found" rather than an error.

#### Scenario: Unknown id
- GIVEN no event with a given id
- WHEN it is requested
- THEN the response is 404 "not found"

### Requirement: Similar events and journey times

The API SHALL offer, for an event, up to six upcoming events most like it with the tags they share, and SHALL offer the walking time and, when quicker, the public-transport time from a given point to an event's venue. The starting point SHALL be rounded to about 200 m before use and SHALL NOT be logged or stored; journey planners SHALL be called only by the server.

#### Scenario: Short walk
- GIVEN a starting point 800 m from the venue
- WHEN journey times are requested
- THEN the walking time is returned and no public-transport lookup is made

### Requirement: The contract is documented and stable

The response format SHALL be documented with examples, and field names SHALL NOT be renamed or removed without notice; new fields MAY be added. Browsers SHALL be able to call the API only from configured allowed origins, and none when none are configured.

#### Scenario: Unlisted origin
- GIVEN a browser page on an origin not in the allowed list
- WHEN it calls the event listing
- THEN the browser is not granted cross-origin access
