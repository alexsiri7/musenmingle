# Content Policy

## Purpose

Muse & Mingle is a free, for-fun aggregator that should send people to venues rather than take their content or traffic. The content policy keeps only facts, a short excerpt and a small credited thumbnail, honours each site's terms, and lets venues reach us and be removed.

## Requirements

### Requirement: Each source's terms decide what is kept

Every source SHALL have a display name and SHALL state whether its descriptions and its images may be kept, with a note of the terms that decided it. Where a site's terms restrict reproduction, only facts and a link SHALL be kept. Turning a permission off SHALL also clear what was already stored under it.

#### Scenario: Facts-only source
- GIVEN a source whose terms restrict reuse
- WHEN its events are ingested
- THEN they have no description and no image
- AND each still links to the source's page

#### Scenario: Permission withdrawn
- GIVEN a source whose descriptions were stored
- WHEN its description permission is turned off
- THEN its stored descriptions are removed on the next ingest run

### Requirement: Only a short excerpt is kept

The system SHALL keep at most a 300-character excerpt of any description, cut at a sentence or else word boundary and ending in an ellipsis, and SHALL NOT persist the full description text anywhere, including raw source payloads. Pages SHALL present it as an excerpt and point to the venue for the rest.

#### Scenario: Long gallery text
- GIVEN a listing with a 2,000-character description from a source that allows descriptions
- WHEN it is stored
- THEN the event's description is at most 300 characters and ends in "…"
- AND the full text is not retained in any stored copy of the listing

### Requirement: Images are credited thumbnails, never hotlinks

The system SHALL NOT expose a source's image URL on pages or in the API. Where a source allows images, the system SHALL fetch each image once, politely, and serve its own small thumbnail (at most 480 px, compressed), always shown with the visible credit "Image: <source name>" linking to the event's page on that source. A failed image SHALL be retried with growing delays and then given up until the image changes.

#### Scenario: Event with an image
- GIVEN an event whose source allows images
- WHEN it is shown on a page or returned by the API
- THEN the image is served from Muse & Mingle's own thumbnail address
- AND a credit names the source and links to the event's page there

### Requirement: Venues are sent the traffic

Links to venues SHALL let the venue see that visitors came from Muse & Mingle (origin only). On an event's page the primary call to action SHALL be the source's own page ("See it on <venue> →").

#### Scenario: Visitor follows a listing
- GIVEN an event page for a Barbican talk
- WHEN the visitor clicks "See it on Barbican →"
- THEN they reach the Barbican's page for the talk
- AND the Barbican can see the visit came from Muse & Mingle's origin

### Requirement: Venues can reach us and be removed

The About page SHALL state truthfully how Muse & Mingle collects, what it keeps, how it uses AI and what it does with visitors' data, and SHALL be updated whenever that behaviour changes. A contact form SHALL let a venue ask for changes or removal; each request SHALL be stored and filed for the owner, protected against spam, with any reply email kept out of the public tracker. Removal requests SHALL be honoured within 7 days.

#### Scenario: Venue asks to be removed
- GIVEN a venue submits the contact form asking for removal
- WHEN the request is accepted
- THEN it is filed for the owner without the venue's email address
- AND the venue's events are removed within 7 days

### Requirement: Declined sites are recorded and not retried

When a site is examined and cannot be covered (robots.txt disallows it, it blocks our crawler, it has no usable event data, its terms forbid it, it needs JavaScript we cannot read, or its owner asked), the system SHALL record the site with its reason and date, SHALL list it publicly as a site we couldn't use, and SHALL NOT scrape it.

#### Scenario: Site blocks the crawler
- GIVEN Southbank Centre returns 403 to MuseNMingleBot
- WHEN it is recorded as declined with reason "bot_blocked"
- THEN it appears under "Sites we couldn't use" with that reason
- AND no source is created for it
