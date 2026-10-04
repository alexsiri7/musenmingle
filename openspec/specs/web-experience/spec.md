# Web Experience

## Purpose

People, not only programs, use Muse & Mingle. The website lets them browse, filter, save and plan around London's creative events in any browser, fast, privately and without depending on JavaScript or third parties.

## Requirements

### Requirement: Browsing works as shareable links

The home page SHALL list upcoming events from today (London) with a plain filter form (search, dates, category, free, time of day, price, area or borough, source, tags, venue type, music subtag, order) and quick-pick chips (Tonight, This weekend, Free, Openings this week, Last chance, Hands-on, Talks, Open now) showing live counts, with empty chips hidden. Every filtered view SHALL be a shareable URL, and results SHALL page with a "More" link. Event cards SHALL lead to Muse & Mingle's own event page and keep a "See it on <venue>" link.

#### Scenario: Shared filter
- GIVEN a visitor filters to free talks this weekend
- WHEN they share the page's address
- THEN the recipient sees the same filtered list

### Requirement: Event pages hand off to the person's tools

Each event SHALL have a page with its facts, excerpt, credited thumbnail, AI note (labelled), every source link, "More like this", and hand-offs: share, directions in map apps, add to Google Calendar and a one-event calendar file. The page SHALL carry sharing previews for social sites.

#### Scenario: Add to calendar
- GIVEN an all-day exhibition's page
- WHEN the visitor downloads its calendar file
- THEN it holds one all-day entry spanning the exhibition's London dates

### Requirement: Calendar views and a subscribable feed

The site SHALL offer month, week and agenda calendar views with the listing's filters, where long-running events (four or more London days) appear once in an "Ongoing across London" strip with opening and last-day markers, and other events on their start day. The same filters SHALL be available as a subscribable calendar feed of the next 90 days that apps refresh hourly, with one stable entry per event and no AI text.

#### Scenario: Long exhibition in month view
- GIVEN a ten-week exhibition
- WHEN the month calendar is shown
- THEN it appears once in the ongoing strip, not on every day

### Requirement: Saved events stay on the visitor's device

Visitors SHALL be able to save events without an account, with saves kept only in their own browser and never sent to or stored by the server. A Saved page and a personal calendar view SHALL show them, mark events no longer listed, and export them as a calendar file. Saves SHALL NOT be subscribable.

#### Scenario: Event removed from listings
- GIVEN a visitor saved an event that has since been removed
- WHEN they open their Saved page
- THEN it is marked "No longer listed"

### Requirement: Near me, right now

A map page SHALL list events on now or starting within the next few hours (or later today), nearest first from an area, as a list that works without JavaScript, and with JavaScript SHALL draw a map from Muse & Mingle's own self-hosted London map tiles. A visitor MAY use their exact position; it SHALL stay on their device, with walking distances computed in the browser. Only this page SHALL ask for location access.

#### Scenario: Exact position
- GIVEN a visitor allows location access on the map page
- WHEN distances are shown
- THEN their coordinates are not sent to the server

### Requirement: Pages are self-contained, private and work without JavaScript

Every page SHALL be server-rendered, escape all data, and work fully without JavaScript, with script only as enhancement. Pages SHALL load no third-party scripts, fonts, images or embeds, SHALL enforce a strict content security policy with no inline scripts or styles, SHALL set no cookies, and SHALL NOT link into the private code repository. Links SHALL be http(s) only.

#### Scenario: JavaScript disabled
- GIVEN a browser with JavaScript off
- WHEN a visitor filters, pages through and opens events
- THEN everything works and script-only controls are not shown

### Requirement: The former name still leads here

Requests to the former Thaleia host SHALL be permanently redirected to the same path and query on the Muse & Mingle host, except health checks, and saves made under the former name SHALL carry over.

#### Scenario: Old bookmark
- GIVEN a bookmark to an event page on thaleia.interstellarai.net
- WHEN it is opened
- THEN the visitor is redirected to the same page on musenmingle.interstellarai.net
