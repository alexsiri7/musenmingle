# Venues and Places

## Purpose

People choose events by where they are as much as by what they are. Venues and places gives every event a known venue, location, borough and kind of venue where possible, deterministically and without AI, so events can be found by area, mapped, and grouped by the place that hosts them.

## Requirements

### Requirement: Venues are first-class places

The system SHALL recognise a venue across its different spellings and SHALL give each venue one record with a page on the site showing its address, area, type, usual opening hours, map links and upcoming events. Known spellings of the same place, and names that denote an area rather than a venue, SHALL be declared once and applied to every event. An event whose source gives no coordinates SHALL take its venue's; a venue's missing coordinates SHALL be looked up from its UK postcode.

#### Scenario: Two spellings of one venue
- GIVEN events at "Tate Modern" and at "Tate Modern, Bankside"
- WHEN venues are resolved
- THEN both events belong to one venue page

#### Scenario: Missing coordinates
- GIVEN a listing at a known venue without coordinates
- WHEN it is stored
- THEN it takes the venue's coordinates and appears on the map

### Requirement: Every located event has a borough

The system SHALL give each event with coordinates inside Greater London the London borough (or the City of London) containing them, by point-in-polygon against published boundaries, refreshed after each ingest run. Events without coordinates or outside Greater London SHALL have no borough and SHALL NOT match a borough filter. Named areas (Central & South Bank, East, King's Cross, South Kensington) SHALL be shortcuts for groups of whole boroughs.

#### Scenario: Area preset
- GIVEN an event in Southwark and one in Hackney
- WHEN events are listed for the "Central & South Bank" area
- THEN only the Southwark event is listed

### Requirement: Every event has one venue type

The system SHALL give every event exactly one venue type (museum, commercial gallery, artist-run, community or other), deciding by a manual override for the venue, else the default of its source, else keywords in the venue name, else other.

#### Scenario: Artlogic gallery
- GIVEN an event from an Artlogic commercial gallery source with no override
- WHEN venue types are applied
- THEN its venue type is commercial gallery

### Requirement: Music events carry subtags

Every music event SHALL carry the subtags that apply among classical, contemporary, experimental, jazz and sound art, taken from its source's tags and from title keywords (for example "string quartet" is classical, "sound installation" is sound art), and they SHALL be filterable.

#### Scenario: Jazz night
- GIVEN a music event titled "Late-night jazz trio"
- WHEN subtags are applied
- THEN it carries jazz and appears when filtering music by jazz

### Requirement: Opening hours are known for runs

For multi-day runs such as exhibitions, the system SHALL keep weekly opening hours in London time, from the listing's own statement of hours or structured data, else the venue's usual hours, and SHALL show whether the event is open now and until when.

#### Scenario: Open now
- GIVEN an exhibition open Wednesday to Sunday 11:00–18:00
- WHEN it is shown on a Thursday at 14:00
- THEN it is marked open now until 18:00
