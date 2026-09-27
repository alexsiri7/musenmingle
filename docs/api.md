# Muse & Mingle HTTP API

All responses are JSON with snake_case field names. Field names are stable:
they are not renamed or removed without notice; new fields may be added.

- Timestamps are RFC 3339 in UTC (`"2026-10-01T18:00:00Z"`).
- Prices are decimal **strings** (`"12.50"`, `"0"`), in `currency`.
- Read-API errors are `{"error": "<message>"}` with status 400 (bad
  parameters), 404 (not found) or 500.

The same process also serves human-facing HTML pages (not part of this
API's stability promise): `GET /` (upcoming events with a filter form that
takes `q`, `from`, `to`, `category`, `free`, `when`, `price_max`, `near=<area or borough key>`, `sort`, `pick`,
`source`, `medium`, `format`, `good_for`, `venue_type`, `music` and `cursor`, and shows the
`counts`/facet counts next to its options),
`GET /events/{id}` (with Open Graph tags, and Share / Directions / Add-to-calendar hand-offs from `src/share.rs`), `GET /events/{id}.ics` (the event as a one-event iCalendar file: exact UTC times, or an all-day span for date-only events; `text/calendar`, downloaded as `<slug>.ics`), `GET /sources`, `GET /saved`, `GET /about`, `GET`/`POST /contact` (venue contact form; see `docs/venue-requests.md`), `POST /suggest` (form-encoded `url`,
`note`; same rules and status codes as `POST /v1/suggestions`) and
`GET /static/style.css` / `GET /static/app.js`. HTML responses carry a strict
`Content-Security-Policy` (`img-src 'self'`). See `src/web.rs`.

## Content policy

The service is a free, for-fun aggregator: it links out to venues and
does not republish their content. So, in every response:

- `description` is at most a **300-character excerpt** (cut at a sentence
  or word boundary, ending in `…`), and `null` for sources whose terms don't
  allow us to keep descriptions (currently Ticketmaster, Serpentine
  Galleries and Sir John Soane's Museum). Follow the source link for the full text.
- The source's own image URL is **never exposed** (`image_url` was removed
  on 2026-09-26). Instead `thumbnail_url` points to a small copy we host
  (made once per image, ≤ 480 px, JPEG), and `image_credit` names its
  owner and links to the event's page there. Show the credit whenever you
  show the thumbnail, and don't hotlink the venue's image.

## `GET /thumbs/{event_id}-{hash}.jpg`

A thumbnail as linked from `thumbnail_url`: `image/jpeg`, `ETag`,
`Cache-Control: public, max-age=604800, immutable` (the hash changes when the
image does). `GET /thumbs/{event_id}` serves the current thumbnail without
`immutable`. `If-None-Match` is answered with 304. Unknown ids, stale
hashes and images from sources that no longer allow them are 404.

Browsers may call the API only from the origins listed in `CORS_ORIGINS`
(comma-separated, e.g. `https://musenmingle.example,http://localhost:5173`).
Unset means no cross-origin access.

## `GET /v1/events`

Lists events. Every parameter is optional; they combine freely.

| Parameter | Example | Meaning |
|---|---|---|
| `q` | `whitechapel painting` | Full-text search (see below), at most 200 characters; blank = no search. Sorts by `relevance` unless `sort` says otherwise |
| `from` | `2026-10-01` | Events still on at or after the start of this London date |
| `to` | `2026-10-05` | Events starting on or before this London date (inclusive) |
| `at` | `now` | `now`: events still on now or starting within `within_hours`; `today`: still on now or starting later today (London). Not combinable with `from`, `to` or `when` (see below). Used by `/map` |
| `within_hours` | `3` | Window for `at=now`, 1–24 (default 3). Only with `at=now` |
| `open_now` | `true` | Events happening now and, when they have `opening_hours`, open now (see "Opening hours" below). Not combinable with `at`, `open_at`, `from`, `to` or `when` |
| `open_at` | `2026-10-18T11:30` | As `open_now`, at another instant: RFC 3339 (`2026-10-18T10:30:00Z`) or London wall-clock time (`YYYY-MM-DDTHH:MM`) |
| `open_on` | `sun` | Events whose London dates (clipped to `from`/`to`) include that day of the week and, when they have `opening_hours`, open that day: `mon`…`sun` (or the full name) |
| `category` | `category=talk&category=workshop` | Any of the given categories: `exhibition`, `expo`, `community`, `talk`, `workshop`, `music` (the 'arty' end: classical, contemporary, experimental, jazz, sound art). Repeatable |
| `free` | `true` | Free events only (`false` = no filter) |
| `price_max` | `10` | Free events and events whose lowest price (`price_min`) is at most this many pounds. Events with an unknown price, or priced in another currency, are left out |
| `when` | `evening` | One of `evening`, `after_work`, `weekend`, `daytime` (see below) |
| `source` | `source=barbican&source=ticketmaster` | Events listed by any of the given sources (keys as in `GET /v1/sources`). Repeatable; an event found by several sources appears under each |
| `ids` | `ids=1f3632de-…,0414a989-…` | Only these events (comma-separated UUIDs, at most 100; unknown ids are simply absent). Combine with `limit=100` to get them all in one page. Used by the Saved page |
| `medium` | `medium=photography&medium=painting` | Events with any of these medium tags: `photography`, `painting`, `drawing`, `sculpture`, `installation`, `design`, `architecture`, `illustration`, `textiles_craft`, `ceramics`, `film_video`, `performance`, `sound_music`, `writing_poetry`, `digital_new_media`, `printmaking`. Repeatable |
| `format` | `format=talk` | Events with any of these format tags: `hands_on`, `talk`, `social`, `opening`, `late`, `family_friendly`, `course`, `tour`, `screening`, `fair_market`. Repeatable |
| `good_for` | `good_for=kids` | Events tagged as good for any of: `solo`, `date`, `friends`, `kids`, `first_timers`, `deep_dive`. Repeatable |
| `venue_type` | `venue_type=museum` | Events at any of these kinds of venue: `museum` (museums and public, non-commercial galleries and arts centres), `commercial_gallery`, `artist_run`, `community`, `other`. Every event has exactly one (see "Venue type" below). Repeatable |
| `borough` | `borough=southwark&borough=city-of-london` | Events in any of these London boroughs (see "Borough" below): `barking-and-dagenham`, `barnet`, `bexley`, `brent`, `bromley`, `camden`, `city-of-london`, `croydon`, `ealing`, `enfield`, `greenwich`, `hackney`, `hammersmith-and-fulham`, `haringey`, `harrow`, `havering`, `hillingdon`, `hounslow`, `islington`, `kensington-and-chelsea`, `kingston-upon-thames`, `lambeth`, `lewisham`, `merton`, `newham`, `redbridge`, `richmond-upon-thames`, `southwark`, `sutton`, `tower-hamlets`, `waltham-forest`, `wandsworth`, `westminster`. Events without a known borough never match. Repeatable |
| `music` | `music=jazz` | Music events with any of these subtags: `classical`, `contemporary`, `experimental`, `jazz`, `sound_art` (see "Music" below). Repeatable |
| `facets` | `true` | Also return `facets`: tag counts (see below) |
| `near` | `51.508,-0.128` | Events within `radius_km` of `<lat>,<lng>` (whatever the sort; nearest first when `sort` and `q` are absent). Events without coordinates are left out |
| `radius_km` | `2.5` | Radius for `near` (default 5, max 100). Only with `near` |
| `within_walk_min` | `20` | Instead of `radius_km`: events within this many minutes' walk of `near`, 1–60, estimated from straight-line distance at 5 km/h with a 1.3 detour factor (the same estimate as the map's "≈ N min walk"). Only with `near`; not with `radius_km` |
| `pick` | `openings` | A quick pick (the home page's chips), relative to today in London: `tonight`, `openings`, `last_chance` or `hands_on` (see below) |
| `sort` | `ending` | Order: `soonest`, `nearest`, `ending`, `added`, `surprise`, `relevance` or `richest` (see below). Default `relevance` with `q`, else `nearest` with `near`, else `soonest` (the home page defaults to `richest`) |
| `limit` | `20` | Page size, 1–100 (default 50) |
| `cursor` | `next_cursor` of the previous page | Next page |

Date window: an event with an end (`ends_at`, e.g. an exhibition) matches
when `[starts_at, ends_at]` overlaps the window; an event without an end
matches when `starts_at` is inside the window. Dates are Europe/London
calendar days, so `from=2026-10-01` starts at `2026-09-30T23:00:00Z` (BST).
An exhibition whose last day is 1 October matches `from=2026-10-01`.

"Now" (`at`): an event is still on when its end is in the future. A
date-only (all-day) event ends at London midnight after its last day (so
an exhibition is on for all of its final day, and DST days are 23 or 25
hours); an event with `ends_at` ends then; an event with a start time but
no end counts as on for 60 minutes after it starts. It must also start
before the window ends: `within_hours` from now for `at=now`, the next
London midnight for `at=today`. `at=now&near=…` lists what is on nearby,
nearest first.

Time of day (`when`), in Europe/London local time. An event starting at
exactly London midnight is treated as **untimed** (a date-only listing,
such as an exhibition run without opening hours):

- `evening`: starts at 18:00 or later; untimed events only if the source
  states late opening hours (they carry the tag `late opening`).
- `after_work`: starts Monday–Friday between 17:30 and 20:30 (inclusive);
  untimed events only with late opening, as for `evening`.
- `daytime`: starts before 18:00, and every untimed event.
- `weekend`: the event's London dates (`starts_at` to `ends_at`), clipped to
  the `from`/`to` window, include a Saturday or Sunday. An exhibition that
  runs through a weekend matches unless the window holds only weekdays.

`evening`, `after_work` and `daytime` look at the start time only, except
for events with `opening_hours`, which are judged by them instead:
`evening` = open after 18:00 on some day, `after_work` = open between 17:30
and 20:30 on a weekday, `daytime` = open before 18:00, `weekend` = open on a
Saturday or Sunday within the clipped dates. The `tonight` pick likewise
means "open after 18:00 today" for them.

Opening hours: some all-day runs (mostly exhibitions) have
`opening_hours`, a weekly schedule in Europe/London time read from the
listing's own text when it states one (e.g. "Wednesdays, Thursdays,
Fridays & Sundays from 11am-3pm"), else the venue's usual hours for an
exhibition. Days not listed are closed. When an event has them, `at`,
`open_now`, `open_at`, `open_on` and `when` use them: `at=now` lists it
only while it is open or if it opens later today before the window ends
(so an exhibition closed on Mondays is not "on now" on a Monday evening).
Events without `opening_hours` keep the date-only behaviour above.

Search (`q`): matches the title (strongest), the venue name, then the
description excerpt, category and tags, with English stemming (`painting`
finds "Paintings") and accents ignored (`Sami` finds "Sámi"). The last word
also matches as a prefix (`whitech`). Web-search syntax works: `"life
drawing"` (phrase), `print or photo`, `ceramics -glaze`. A word that
matches no upcoming event is corrected to the nearest word in upcoming
events' titles and venue names (1 edit for 4–5 letters, 2 for longer), and
events matching the corrected query are included; the response then has
`"search": {"q": "Whitechaple", "corrected": "whitechapel"}` (`corrected`
is `null` otherwise; `search` is absent without `q`). A query of stop words
only (`the`) matches titles and venue names containing it. `counts` and
`facets` apply the search too.

Quick picks (`pick`), judged on London dates, "today" being the day of the
request:

- `tonight`: runs today and either starts today at 17:00 or later, or is
  untimed and tagged `late opening`.
- `openings`: starts within the next 7 days (today included) and is an
  exhibition, has `is_opening: true` or the `opening` format tag, or its
  title says private view, opening reception, opening night, preview
  evening, launch or "PV" (whole words; "PV" in capitals only).
- `last_chance`: runs on more than one London day, its last day is within
  the next 7, and it has not ended (the `ending` sort's notion of an end).
  The home page pairs it with `sort=ending`.
- `hands_on`: category `workshop`, the `hands_on` format tag, or a title
  saying class, course, drop-in, life drawing, masterclass or workshop.

The home page shows these (and "This weekend", "Free" and "Talks", which
are plain `from`/`to`, `free` and `category` links) as chips with counts
from one query, cached for 5 minutes; chips with no events are hidden.

Order (`sort`), ties broken by `id`:

- `soonest` ("Starting soonest"): `starts_at` ascending, so events already
  running come first.
- `nearest` ("Closest to me"): distance from `near` ascending. Without
  `near` it falls back to `soonest`; the response then says
  `"sort": "soonest"` and `"sort_fallback": {"requested": "nearest", ...}`.
- `ending` ("Last chance"): effective end ascending, and only events whose
  effective end is still ahead. The effective end is `ends_at`; for an
  all-day event (`all_day`) the London midnight after its last (or only)
  day; for a timed event without `ends_at`, `starts_at` + 3 hours.
- `added` ("Just added"): when Muse & Mingle first saw the event (its
  earliest `sources[].first_seen_at`), newest first. This is "new to us",
  not "recently opened".
- `surprise` ("Surprise me"): a random order (`md5(id || London date)`),
  the same all day and reshuffled each London day.
- `relevance` ("Best match"): the search rank (`ts_rank`) of `q`, best
  first. The default whenever `q` is set (with or without `near`). Without
  `q` it falls back to `soonest`, with `"sort_fallback": {"requested":
  "relevance", ...}`.

- `richest` ("Soonest, fullest listings first"; the home page's default
  without a search or Near me, never the API's): day by day (the London day
  an event starts, or today for one already running), and within a day
  listings with a picture and a description first, interleaved with
  facts-only ones: two rich listings, then one facts-only, each kind by
  start time. A listing is rich when its score is at least 5 of 8: a
  thumbnail we may show 3, a description of 120+ characters 2 (40+: 1), an
  AI note 1, opening hours or a known price 1 (`repo::richness_sql`).
  Interleaving rather than a hard sort keeps venues whose terms allow only
  facts visible, and the day comes first so a rich event later in the week
  never buries a facts-only one tonight.

The response's `sort` says which sort was applied. There is no popularity
sort: saves stay in the visitor's browser and the server does not track them.

Pagination: `next_cursor` is `null` on the last page. Otherwise, repeat the
request with the **same filters and sort** plus `cursor=<next_cursor>`.
Cursors are opaque and only valid for the sort that issued them (anything
else is a 400). A `surprise` cursor carries its day's shuffle (a `richest` one
its day), so paging across midnight keeps the order.
Unknown parameters are rejected with 400.

```http
GET /v1/events?from=2026-10-01&to=2026-10-05&category=talk&near=51.508,-0.128&radius_km=3&limit=1
```

```json
{
  "events": [
    {
      "id": "1f3632de-af3f-4c79-9ecb-f4bc48b0821f",
      "title": "Life drawing",
      "description": null,
      "venue_name": "Barbican",
      "address": null,
      "lat": 51.5202,
      "lng": -0.0938,
      "starts_at": "2026-10-01T18:00:00Z",
      "ends_at": "2026-10-01T20:00:00Z",
      "all_day": false,
      "is_free": false,
      "price_min": "0",
      "price_max": "12.50",
      "currency": "GBP",
      "url": "https://www.barbican.org.uk/life-drawing",
      "thumbnail_url": "/thumbs/1f3632de-af3f-4c79-9ecb-f4bc48b0821f-9c1f4e2ab07d3355.jpg",
      "image_credit": {
        "name": "Barbican",
        "url": "https://www.barbican.org.uk/life-drawing"
      },
      "category": "talk",
      "tags": ["art", "drawing"],
      "medium_tags": ["drawing"],
      "format_tags": ["hands_on"],
      "good_for": ["solo", "first_timers"],
      "vibe_tags": ["intimate"],
      "music_tags": [],
      "is_opening": false,
      "ai": {
        "label": "AI-generated",
        "whats_cool": "A clothed model and an evening of quick poses: two hours of drawing from life in the Barbican's own studio.",
        "one_liner": "Evening life-drawing session",
        "grounding": "listing",
        "model": "anthropic/claude-opus-5-5",
        "generated_at": "2026-09-26T14:00:00Z"
      },
      "distance_km": 2.7318,
      "sources": [
        {
          "source": "barbican",
          "display_name": "Barbican",
          "url": "https://www.barbican.org.uk/life-drawing",
          "first_seen_at": "2026-09-01T00:00:00Z",
          "last_seen_at": "2026-09-26T06:00:00Z"
        },
        {
          "source": "ticketmaster",
          "display_name": "Ticketmaster",
          "url": "https://www.ticketmaster.co.uk/x",
          "first_seen_at": "2026-09-02T00:00:00Z",
          "last_seen_at": "2026-09-26T06:00:00Z"
        }
      ]
    }
  ],
  "next_cursor": "643a343030...",
  "sort": "nearest",
  "counts": {
    "when": { "evening": 12, "after_work": 9, "weekend": 30, "daytime": 41 },
    "price": { "free": 18, "max_10": 25, "max_20": 33, "unknown": 7 }
  }
}
```

`counts` says how many events each `when` and price option would list,
over all pages (`cursor` and `limit` are ignored). Each `when` count applies
every other filter in the request, including `free`/`price_max`, but not
`when` itself; each price count applies every other filter, including
`when`, but not `free`/`price_max`. `price.free` counts `free=true`,
`max_10`/`max_20` count `price_max=10`/`20`, and `unknown` counts events that
are neither free nor priced (which any `price_max` leaves out).

Event fields: `description`, `venue_name`, `address`, `lat`, `lng`,
`ends_at`, `price_min`, `price_max`, `currency`, `url`, `thumbnail_url` and
`image_credit` may be `null` (`thumbnail_url` and `image_credit` are both
set or both `null`). `thumbnail_url` is a path on this server (see
[Content policy](#content-policy)); `image_credit` is `{"name", "url"}`: who
the image belongs to and the event's page there. `distance_km` is present
only with `near`. `sources` lists every place the event was found (oldest
first); `display_name` is the source's human-readable name and `url` is the
listing on that source (may be `null`). `venue_slug` (absent when the event
has no venue, e.g. an area name) names the venue's page on this site,
`/venues/<venue_slug>` (HTML: address, area, type, usual hours, map links and
upcoming events).

`all_day` is `true` when the source gave dates but no time of day:
`starts_at` and `ends_at` are then London midnight of the first and last day
(the last day inclusive), so display dates without times.

`opening_hours` (absent when unknown; only on all-day runs of more than one
day) is a list of `{"days": ["wed", "thu", "fri", "sun"], "opens": "11:00",
"closes": "15:00"}` in London time (`opens` before `closes`; each day in at
most one entry). `hours_note` (absent when none) is the listing's own
sentence about its hours, kept only for sources whose descriptions we may
store.

Tags and AI notes (see `README.md`, "AI enrichment"): `medium_tags`,
`format_tags`, `good_for` and `vibe_tags` use the fixed vocabularies in the
parameter table (`vibe_tags`: `contemplative`, `playful`, `provocative`,
`immersive`, `lively`, `intimate`, `experimental`, `crafty`). They come from
the AI enrichment; `medium_tags` also carries the listing sources' default
tags (e.g. `design` for the Design Museum), so it may be set without `ai`.
`is_opening` is `true` for private views / openings / launches, `false`
when checked and not one, `null` when not checked yet. `ai` is `null`
until the event is enriched (and again after its facts change, until it is
re-enriched); `ai.whats_cool` (≤ 220 characters) and `ai.one_liner` (≤ 90)
may be `null` when the listing said too little. **Label AI text as
AI-generated wherever you show it**: it is our model's summary of the
listing, not the venue's words.

With `facets=true` the response also has

```json
"facets": {
  "medium": { "photography": 12, "painting": 7 },
  "format": { "talk": 9 },
  "good_for": { "friends": 14, "kids": 3 },
  "venue_type": { "museum": 120, "commercial_gallery": 64 },
  "borough": { "westminster": 80, "southwark": 31, "unknown": 12 },
  "music": { "jazz": 4, "classical": 2 }
}
```

counting the events that match every other filter (dates, category, free,
source, area and the other facets), ignoring that facet's own
selection, so each number says how many events choosing that tag would
show. Tags with no events are omitted. `borough.unknown` counts the
events without a known borough (no coordinates, or outside Greater
London); it is not a `borough=` value.

### Borough

`borough` is deterministic (no AI): the London borough (or the City of
London) containing the event's coordinates, a point-in-polygon test in
`src/borough.rs` against simplified ONS boundaries
(`src/london_boroughs.geojson`, see `docs/boroughs.md`). It is set when a
listing is stored and refreshed for every event after each ingest run.
Events without coordinates, or outside Greater London, have none.

The home page's area presets (`near=<key>`) are shortcuts for groups of
whole boroughs (a group is wider than its name):

| Key | Label | Boroughs |
| --- | --- | --- |
| `central` | Central & South Bank | Westminster, City of London, Lambeth, Southwark |
| `east` | East (City, Shoreditch, Whitechapel) | City of London, Hackney, Tower Hamlets |
| `kings-cross` | King's Cross | Camden, Islington |
| `south-kensington` | South Kensington & Hyde Park | Kensington and Chelsea, Westminster |

The page's `near=` also takes a borough key. Either way it sends
`borough=` to the listing (no distance and no nearest-first sort; "Near
me" is the one that measures distance). `/map` keeps using the presets'
centres.

### Venue type

`venue_type` is deterministic (no AI), from `src/venue_type.rs`, and is
refreshed after every ingest run: a manual override for the venue
(`events.venues.venue_type`, matched on the normalised venue name), else
the default of the event's sources (museum sources `museum`, Artlogic
galleries `commercial_gallery`, Luma calendars `community`, …), else a
keyword in the venue name ("project space", "studios", "artist-run" →
`artist_run`; "museum" → `museum`; "community", "library", "arts centre"
→ `community`), else `other`.

### Music

The `music` category (issue #209) covers the 'arty' end of live music.
`music_tags` (empty for other categories) holds its subtags: deterministic
(no AI), from `src/music.rs`, refreshed after every ingest run from the
event's source tags and title (a source may put a subtag itself, e.g.
`jazz`, in `tags`; keywords such as "string quartet" → `classical`,
"free improvisation" → `experimental`, "sound installation" → `sound_art`
catch the rest).

**Breaking change (2026-09-26):** `image_url` was removed from event
objects; use `thumbnail_url` + `image_credit`.

## `GET /v1/events/{id}`

One event, same shape as a list item (without `distance_km`). An unknown or
malformed id is `404 {"error": "not found"}`.

## `GET /v1/events/{id}/similar`

"More like this": up to 6 events still on today or later whose embeddings
are nearest to this event's (cosine similarity), most similar first. Empty
when the event has no embedding yet or embeddings are off.

```json
{ "similar": [ { "id": "…", "title": "…", "venue_name": "…", "starts_at": "…",
  "ends_at": null, "all_day": false, "category": "talk", "similarity": 0.83,
  "shared_tags": ["photography", "talk"] } ] }
```

`shared_tags` are the tag values the two events have in common (the
"similar because" hint).

## `GET /v1/transit`

`?from=<lat>,<lng>&event=<id>`: the walking time and, when it's quicker,
the public-transport time from `from` to the event's venue. The server
rounds `from` to ~200 m before using it and never logs or stores it; the
journey planner (TfL in London) is only ever called by the server. 400 for
a bad `from`/`event`, 404 for an unknown event, otherwise 200
(`Cache-Control: private, max-age=300`):

```json
{ "status": "ok",
  "walk": { "minutes": 34, "km": 2.2 },
  "transit": { "minutes": 18, "summary": "Overground + 5 min walk",
    "modes": ["walking", "overground"], "provider": "TfL",
    "links": [ { "label": "Plan on TfL", "url": "https://tfl.gov.uk/plan-a-journey/results?…" },
               { "label": "Citymapper", "url": "https://citymapper.com/directions?…" } ] } }
```

`transit` is null unless `status` is `ok`: `short_walk` (under 1.5 km;
the planner isn't asked), `not_faster` (walking is as quick),
`unavailable` (planner failed, timed out after 3 s, found no route, or the
lookup was throttled), `outside_area` (no planner for these places) or
`no_location` (the event has no coordinates; `walk` is null too). Answers
are cached for ~15 minutes per (rounded origin, venue, 15-minute departure
bucket). See `docs/map.md` for the provider design.

## `GET /v1/sources`

Every configured source, ordered by `key`.

```json
{
  "sources": [
    {
      "key": "barbican",
      "display_name": "Barbican",
      "kind": "scraper",
      "interval_minutes": 1440,
      "enabled": true,
      "last_run": {
        "started_at": "2026-09-26T06:00:00Z",
        "events_found": 40,
        "errors": 0,
        "duration_ms": 1500,
        "ok": true
      },
      "skip": null,
      "status": "healthy",
      "issue_url": null,
      "qa": {
        "rule_flags": 0,
        "last_check": {
          "checked_at": "2026-09-26T06:04:00Z",
          "status": "ok",
          "problems": 0
        }
      }
    },
    {
      "key": "serpentine-galleries",
      "display_name": "Serpentine Galleries",
      "kind": "scraper",
      "interval_minutes": 1440,
      "enabled": true,
      "last_run": {
        "started_at": "2026-09-26T06:00:00Z",
        "events_found": 0,
        "errors": 1,
        "duration_ms": 1500,
        "ok": false
      },
      "skip": null,
      "status": "broken",
      "issue_url": "https://github.com/alexsiri7/musenmingle/issues/31",
      "qa": { "rule_flags": 1, "last_check": null }
    },
    {
      "key": "ticketmaster",
      "kind": "api",
      "interval_minutes": 360,
      "enabled": true,
      "last_run": null,
      "skip": {
        "at": "2026-09-26T06:00:00Z",
        "reason": "TICKETMASTER_API_KEY not set"
      },
      "status": "unconfigured",
      "issue_url": null,
      "qa": { "rule_flags": 0, "last_check": null }
    }
  ]
}
```

The response also has `refused`: sites we checked and decided not to
scrape, most recently checked first:

```json
"refused": [
  {
    "name": "Southbank Centre",
    "domain": "southbankcentre.co.uk",
    "url": "https://www.southbankcentre.co.uk/whats-on",
    "reason_code": "bot_blocked",
    "reason_text": "it returns 403 to our crawler's User-Agent; we don't evade blocks",
    "checked_on": "2026-09-25",
    "issue_url": "https://github.com/alexsiri7/musenmingle/issues/6"
  }
]
```

`reason_code` is one of `robots_disallowed`, `bot_blocked`,
`no_event_data`, `terms`, `js_only`, `owner_request`, `other`; `checked_on` is a date; `issue_url` may be
`null`.

- `display_name`: human-readable name (falls back to the title-cased key).
- `kind`: `api` or `scraper`.
- `last_run`: the most recent real run, or `null` if the source never ran.
  Skips never appear here.
- `skip`: set when the latest ingest attempt could not build the source
  (missing credentials, an invalid `base_url`, or no implementation for the
  key), with the time and reason; `null` otherwise. The next run clears it.
  Skipped sources are retried every ingest tick.
- `status`, first match wins: `unconfigured` while `skip` is set; `broken`
  while a `scraper-broken` issue is open; `pending` when the source never ran
  (regardless of `enabled`); `degraded` when the last run failed or had
  errors; otherwise `healthy`.
- `issue_url`: link to the open `scraper-broken` issue, or `null`. Set
  whenever an issue is open, even if `status` is `unconfigured`.
- `qa`: the scraper QA checks (see the README's "Scraper QA"). `rule_flags`
  is how many automatic rules (e.g. events ending before they start) the
  last run hit. `last_check` is the latest AI comparison of the source's
  pages with what we stored that reached a verdict, or `null`: `status` is
  `ok`, `issues` (then `problems` counts wrong fields plus missed events),
  `no_pages` or `invalid` (both inconclusive). QA does not change `status`
  and never links to an issue.

## `GET /calendar.ics`

A subscribable iCalendar (RFC 5545) feed of upcoming events: London today
plus 89 days (events overlapping that window, at most 1000), for the
calendar's filters `category`, `free=true`, `near=<area or borough key>` (the page's
preset areas, e.g. `east`) and `source` (repeatable). The calendar page
`/calendar` links to it as `webcal://musenmingle.interstellarai.net/calendar.ics?…`.

- `Content-Type: text/calendar; charset=utf-8`, `Cache-Control: public,
  max-age=3600`; the calendar asks apps to refresh hourly
  (`REFRESH-INTERVAL`, `X-PUBLISHED-TTL` `PT1H`).
- One `VEVENT` per event, `UID:<event id>@musenmingle.interstellarai.net`
  (the same UID as the Saved page's `.ics` export). Timed events are in UTC
  (`DTSTART:20261007T173000Z`); date-only and long-running events (4 or more
  London days) are all-day spans of their London dates (`VALUE=DATE`, `DTEND`
  exclusive).
- `SUMMARY` title, `LOCATION` venue and address, `URL` the venue's own page
  (else ours), `DESCRIPTION` the stored excerpt (none for facts-only sources;
  never AI text) and "via Muse & Mingle: <our event page>".
- Unknown filter values are a plain-text `400`.

Saved events live only in the visitor's browser, so they have no feed: the
Saved page exports them as a one-off `.ics` file instead.

## `GET /healthz`

`200 {"status": "ok", "db": "ok", "version": "..."}` when the database
answers, `503` with `"status": "unavailable"` otherwise.

## `POST /v1/suggestions`

Suggest a site to scrape: body `{"url": "https://...", "note": "optional"}`.
Responses: `201` `{"status": "accepted", "domain", "github_issue"}`, `200`
`already_suggested`, `409` `already_covered` (with `source`), `409`
`refused` for a site in `refused` above (with `message`, e.g. "We looked at
Southbank Centre on 25 September 2026 and couldn't include it: …", and the
`refused` entry; nothing is filed), `400` `invalid`
(with `error`), `429` `rate_limited` (with `retry_after_secs` and a
`Retry-After` header). `github_issue` is `null` when filing waits for a
later ingest run (GitHub unavailable, or the site's daily issue cap reached).
