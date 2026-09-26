# Thaleia HTTP API

All responses are JSON with snake_case field names. Field names are stable:
they are not renamed or removed without notice; new fields may be added.

- Timestamps are RFC 3339 in UTC (`"2026-10-01T18:00:00Z"`).
- Prices are decimal **strings** (`"12.50"`, `"0"`), in `currency`.
- Read-API errors are `{"error": "<message>"}` with status 400 (bad
  parameters), 404 (not found) or 500.

The same process also serves human-facing HTML pages (not part of this
API's stability promise): `GET /` (upcoming events with a filter form that
takes `from`, `to`, `category`, `free`, `near=<area>`, `source` and
`cursor`),
`GET /events/{id}`, `GET /sources`, `POST /suggest` (form-encoded `url`,
`note`; same rules and status codes as `POST /v1/suggestions`) and
`GET /static/style.css`. HTML responses carry a strict
`Content-Security-Policy`. See `src/web.rs`.

Browsers may call the API only from the origins listed in `CORS_ORIGINS`
(comma-separated, e.g. `https://thaleia.example,http://localhost:5173`).
Unset means no cross-origin access.

## `GET /v1/events`

Lists events. Every parameter is optional; they combine freely.

| Parameter | Example | Meaning |
|---|---|---|
| `from` | `2026-10-01` | Events still on at or after the start of this London date |
| `to` | `2026-10-05` | Events starting on or before this London date (inclusive) |
| `category` | `category=talk&category=workshop` | Any of the given categories: `exhibition`, `expo`, `community`, `talk`, `workshop`. Repeatable |
| `free` | `true` | Free events only (`false` = no filter) |
| `source` | `source=barbican&source=ticketmaster` | Events listed by any of the given sources (keys as in `GET /v1/sources`). Repeatable; an event found by several sources appears under each |
| `near` | `51.508,-0.128` | Events within `radius_km` of `<lat>,<lng>`, nearest first. Events without coordinates are left out |
| `radius_km` | `2.5` | Radius for `near` (default 5, max 100). Only with `near` |
| `limit` | `20` | Page size, 1–100 (default 50) |
| `cursor` | `next_cursor` of the previous page | Next page |

Date window: an event with an end (`ends_at`, e.g. an exhibition) matches
when `[starts_at, ends_at]` overlaps the window; an event without an end
matches when `starts_at` is inside the window. Dates are Europe/London
calendar days, so `from=2026-10-01` starts at `2026-09-30T23:00:00Z` (BST).
An exhibition whose last day is 1 October matches `from=2026-10-01`.

Order: by `starts_at` (then `id`); with `near`, by distance (then `id`).

Pagination: `next_cursor` is `null` on the last page. Otherwise, repeat the
request with the **same filters** plus `cursor=<next_cursor>`. Cursors are
opaque; one issued without `near` is rejected with `near` and vice versa.
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
      "is_free": false,
      "price_min": "0",
      "price_max": "12.50",
      "currency": "GBP",
      "url": "https://www.barbican.org.uk/life-drawing",
      "image_url": null,
      "category": "talk",
      "tags": ["art", "drawing"],
      "distance_km": 2.7318,
      "sources": [
        {
          "source": "barbican",
          "url": "https://www.barbican.org.uk/life-drawing",
          "first_seen_at": "2026-09-01T00:00:00Z",
          "last_seen_at": "2026-09-26T06:00:00Z"
        },
        {
          "source": "ticketmaster",
          "url": "https://www.ticketmaster.co.uk/x",
          "first_seen_at": "2026-09-02T00:00:00Z",
          "last_seen_at": "2026-09-26T06:00:00Z"
        }
      ]
    }
  ],
  "next_cursor": "643a343030..."
}
```

Event fields: `description`, `venue_name`, `address`, `lat`, `lng`,
`ends_at`, `price_min`, `price_max`, `currency`, `url` and `image_url` may be
`null`. `distance_km` is present only with `near`. `sources` lists every
place the event was found (oldest first); `url` there is the listing on that
source and may be `null`.

## `GET /v1/events/{id}`

One event, same shape as a list item (without `distance_km`). An unknown or
malformed id is `404 {"error": "not found"}`.

## `GET /v1/sources`

Every configured source, ordered by `key`.

```json
{
  "sources": [
    {
      "key": "barbican",
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
      "status": "healthy",
      "issue_url": null
    },
    {
      "key": "ticketmaster",
      "kind": "api",
      "interval_minutes": 360,
      "enabled": true,
      "last_run": {
        "started_at": "2026-09-26T06:00:00Z",
        "events_found": 0,
        "errors": 1,
        "duration_ms": 1500,
        "ok": false
      },
      "status": "broken",
      "issue_url": "https://github.com/alexsiri7/thaleia/issues/31"
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
    "reason_text": "it returns 403 to the ThaleiaBot User-Agent; we don't evade blocks",
    "checked_on": "2026-09-25",
    "issue_url": "https://github.com/alexsiri7/thaleia/issues/6"
  }
]
```

`reason_code` is one of `robots_disallowed`, `bot_blocked`,
`no_event_data`, `terms`, `js_only`, `other`; `checked_on` is a date; `issue_url` may be
`null`.

- `kind`: `api` or `scraper`.
- `last_run`: the most recent run, or `null` if the source never ran.
- `status`: `broken` while a `scraper-broken` issue is open (its link is in
  `issue_url`, otherwise `null`); else `degraded` when the last run failed or
  had errors; else `healthy` (including a source that never ran).

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
`Retry-After` header).
