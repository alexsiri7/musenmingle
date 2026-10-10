# Memberships

## Purpose

Many people who go to exhibitions hold a membership, such as the National Art Pass, that gets them in free or for less at some venues. Memberships lets them find the events their membership helps with, from facts we hold, without scraping the membership provider and without storing anything about the visitor.

## Requirements

### Requirement: Memberships are a fixed, known list

The system SHALL know a fixed list of memberships, each with a key, a display name and a link to the provider. The list SHALL start with the National Art Pass (key `art_pass`). Adding a membership SHALL be a code and migration change, never something a visitor or a source can do.

#### Scenario: The National Art Pass is known
- WHEN the memberships are listed
- THEN the National Art Pass is among them with the key art_pass, its display name and its provider link

### Requirement: Participating venues are kept by hand with their offer and source

The system SHALL keep, per membership, the venues where it applies, each with the offer in words (for example "Free entry" or "50% off exhibitions") and where that fact came from (the venue's own page, or the author). These entries SHALL be added by migration. The system SHALL NOT build or refresh them by scraping the membership provider's website or any site whose terms forbid it.

#### Scenario: A participating venue
- GIVEN Tate Modern is recorded for the National Art Pass with the offer "50% off exhibitions" and its source
- WHEN the venue's memberships are read
- THEN the National Art Pass is returned with that offer and source

### Requirement: Events match a membership through their venue or their own text

An event SHALL match a membership when its venue is a participating venue for it, or when its own stored text (source tags, price notes or stored excerpt) names the membership, matched deterministically by the membership's known names. Matching SHALL use no AI and SHALL be recomputed after each ingest run. An event matched through its venue SHALL carry that venue's offer; one matched only by its text SHALL carry no offer unless the text states one.

#### Scenario: Matched by venue
- GIVEN an exhibition at Tate Modern
- AND Tate Modern participates in the National Art Pass with "50% off exhibitions"
- WHEN memberships are synced after an ingest run
- THEN the exhibition matches art_pass with the offer "50% off exhibitions"

#### Scenario: Matched by the event's own text
- GIVEN an event at a venue not on any list
- AND its price notes say "Free for National Art Pass holders"
- WHEN memberships are synced
- THEN the event matches art_pass

#### Scenario: No match
- GIVEN an event at a venue not on any list whose text never names a membership
- WHEN memberships are synced
- THEN it matches no membership

### Requirement: Membership is a filter

The website and the read API SHALL accept `membership=<key>` to list only events matching that membership, combinable with every other filter and kept in shareable links like them. An unknown key SHALL be rejected by the API and ignored by the website. The filter SHALL show how many current events match, and matching events SHALL show the membership and its offer.

#### Scenario: Filtering for the Art Pass
- GIVEN two upcoming events matching art_pass and one that does not
- WHEN the events are listed with membership=art_pass
- THEN only the two matching events are returned, each showing the National Art Pass and its offer

#### Scenario: Combined with other filters
- WHEN the home page is opened with membership=art_pass and pick=open_now
- THEN only events matching both are shown and both stay in the page's links

#### Scenario: Unknown membership in the API
- WHEN the API is asked for membership=unknown
- THEN it answers with a client error naming the parameter

### Requirement: Memberships hold no visitor data

The chosen membership SHALL live only in the URL, like every other filter. The system SHALL NOT store, log against a visitor, or ask for a visitor's membership.

#### Scenario: Choosing a membership
- WHEN a visitor filters by membership=art_pass
- THEN nothing about that choice is stored beyond the request URL in ordinary access logs
