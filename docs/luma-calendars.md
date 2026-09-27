# Luma calendars

Muse & Mingle reads a small, curated set of Luma (luma.com) calendars run by
London creative communities, through each calendar's **official iCal
subscription feed** only (`https://api2.luma.com/ics/get?entity=calendar&id=<cal-id>`,
the link behind "Add iCal subscription"). We never read luma.com pages:
Luma's terms (https://luma.com/terms) allow reuse only "through our publicly
supported interfaces", so these sources are **facts + link only**
(`store_description = FALSE`, `store_image = FALSE`). Implementation:
`src/sources/luma.rs`; seed rows: `migrations/*_seed_luma_calendars.sql`.

Additions are deliberate: add a row here with a one-line reason in the same
PR as its seed migration (see "Adding a Luma calendar" in
[adding-a-scraper.md](adding-a-scraper.md)).

## Included

| Source key | Calendar | Calendar id | Config | Why |
|---|---|---|---|---|
| `luma-new-media-london` | New Media London | `cal-fMoq9nuFiKXYCzi` | default `community` | Audiovisual / new-media art meetups and workshops (TouchDesigner, Pure Data) at London galleries and warehouses. |
| `luma-creative-ai-meetup` | Creative AI Meetup (Luba Elliott) | `cal-bDC6E5p1xVynAEf` | default `talk` | Long-running London talks series on AI in art, music and writing, held at venues such as arebyte and IDEALondon. |
| `luma-mason-and-fifth` | Mason & Fifth | `cal-mJXPpBb7tgosK3a` | no default; skips screenings, listening parties, sound baths, parties, retail pop-ups, fitness | Westbourne Park (W9) creative space: author talks, craft and writing workshops. It also runs music, film, wellness and retail events, so only titles that match a talk/workshop/exhibition keyword are kept. |
| `luma-for-writers` | For Writers (Second Brain HQ) | `cal-rc3wsjuGa6p18Et` | default `workshop` | Writing and dramaturgy drop-ins in Brixton. |

## Looked at and left out (2026-09-27)

- London Live Coding (`cal-qaBuxr3sRTGy0Ol`): mostly algoraves and club nights (music), outside our categories.
- Nature, Reprinted (`cal-JonhtmDF94kBR6T`): a one-person, one-off morning series; not a community calendar.
- Kindred Community, Localloo, Led by Community: international or startup/community-building calendars with few London creative events.
- Art Club, Long Now London, London Creative Coding, `creativeai` (`cal-SmJoBmqDlX9mBnx`): no upcoming events (stale calendars).
- Black Hippie Art, Collective Z Gallery, The Love Potion Library, Wonder Studios: not in London, or mostly online.
- "Personal" calendars (individual users' calendars that appear on Luma's discover pages): never included.
- London Writers' Salon: `lu.ma/user/lws` is a user profile, not a calendar with a feed.
- `luma.com/london` "popular events": mostly startup/tech; not used.

Only four calendars qualified on the first pass (the issue hoped for 10–20);
more should be added as they are found, each with a reason above.
