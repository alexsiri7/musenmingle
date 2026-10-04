# Event Ordering

## Purpose

Order decides what a person sees first. Muse & Mingle is a visual discovery app, so when no order is asked for it shows what is soonest and, within each day, the listings we know most about; an order the person asks for always wins.

## Requirements

### Requirement: Default order groups by day, richest first

When a request for events asks for no order, uses no search and no proximity point, results SHALL be grouped by Europe/London day, soonest first, and within each day ordered by how much we know about the event, highest first. Richness SHALL weigh, in order: a thumbnail we may show (outweighing all the others combined), a description, a price (including free), and a known venue or coordinates. A multi-day event SHALL belong to the first day it is on within the requested window (today, when no window is given and it is already open); a multi-session event to its next session's day. Ties SHALL fall back to start time, then a fixed identifier.

#### Scenario: Timeliness beats richness
- GIVEN a text-only talk tonight and a well-illustrated exhibition opening next week
- WHEN events are listed with no order
- THEN the talk comes before the exhibition

#### Scenario: Image wins within a day
- GIVEN two events today, one with a thumbnail only and one with a description, price and venue but no thumbnail
- WHEN events are listed with no order
- THEN the event with the thumbnail comes first

#### Scenario: Exhibition that opened last month
- GIVEN an exhibition that opened last month and is still running
- WHEN events are listed with no order and no window
- THEN it is grouped under today
- AND with a window starting next Saturday it is grouped under next Saturday

### Requirement: An explicit order always wins

When the request names an order, or gives a proximity point without one, that order SHALL be applied unchanged and richness SHALL play no part. The available orders SHALL be: soonest, nearest (distance from the given point), ending (effective end ascending, only events not yet ended), recently added to Muse & Mingle, surprise (a random order stable for a London day), best search match (the default with a search), and richest. The response SHALL say which order was applied, and when a requested order cannot apply (nearest without a point, best match without a search) SHALL fall back to soonest and say so.

#### Scenario: Proximity filter
- GIVEN events near a point with different richness and dates
- WHEN events are listed near that point with no order
- THEN they are strictly nearest first

#### Scenario: Nearest without a point
- GIVEN a request for nearest first with no point
- WHEN events are listed
- THEN they are ordered soonest first
- AND the response says nearest was requested and soonest applied

### Requirement: The home page leads with rich listings without hiding facts-only venues

When a person browses the home page without a search, proximity or chosen order, the page SHALL group events by London day, soonest first, and within each day SHALL lead with listings that have a picture and a description while interleaving facts-only listings (two rich, then one facts-only), so venues whose terms allow only facts stay visible.

#### Scenario: Facts-only venue on a busy day
- GIVEN five illustrated events and two facts-only events today
- WHEN the home page is opened with no order
- THEN a facts-only event appears third, after two illustrated ones

### Requirement: Orders are stable across pages

Every order SHALL be deterministic for the same data, so the same request gives the same order across requests and pages and paging never repeats or skips an event. A day-based or daily-shuffled order SHALL keep the day it started with when paging across midnight.

#### Scenario: Paging across midnight
- GIVEN a person paging through the default order at 23:59 London time
- WHEN they request the next page at 00:01
- THEN the next page continues the same order without repeats or gaps
