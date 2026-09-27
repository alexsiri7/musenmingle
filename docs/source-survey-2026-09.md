# Source survey — 2026-09 (LetsArt / Thaleia)

Checked on **2026-09-26** with `User-Agent: ThaleiaBot/0.1 (+https://github.com/alexsiri7/thaleia; contact via repo issues)`, ≤1 request per 2 s per host, robots.txt + at most 3 listing/detail pages per site, redirects followed manually (one hop = one request), no retries with other UAs. robots.txt was read by eye for every VIABLE site (Python's robotparser ignores wildcards/longest-match). A 403/429/Cloudflare challenge is recorded as blocked. Already covered and not re-surveyed: ticketmaster, serpentine-galleries, barbican, whitechapel-gallery, design-museum, somerset-house.

## Summary

- VIABLE (issues filed): 15
- VIABLE backlog (not filed, to keep the factory queue small): 8
- MAYBE: 25 rows / 31 sites (three rows each group three sites)
- REFUSED: 31 (incl. the 2 refused earlier; tracked in #47)

## VIABLE — issues filed

Issues:
- `conway-hall` → #32
- `vam` → #33
- `wellcome-collection` → #34
- `british-library` → #35
- `london-review-bookshop` → #36
- `camden-art-centre` → #37
- `lisson-gallery` → #38
- `city-lit` → #39
- `horniman` → #40
- `estorick-collection` → #41
- `south-london-gallery` → #42
- `ica` → #43
- `rbkc-museums` → #44
- `photographers-gallery` → #45
- `soane-museum` → #46

### Conway Hall
- URL: https://www.conwayhall.org.uk/whats-on/
- Categories: talk, community, workshop (+ concerts to skip)
- robots: User-agent: * Disallow: /wp-admin/ only — allowed
- Bot response: 200 (LiteSpeed), no challenge
- Data: JSON-LD `Event` blocks on the LISTING page (34 on 2026-09-26): name, startDate, endDate, location{Place, PostalAddress}, image, description, eventStatus, eventAttendanceMode; no offers. Some blocks contain raw control characters (newlines inside strings) that strict JSON parsers reject; one detail page's block failed to parse the same way.
- Est. events: ~34 upcoming
- Notes: Times are London wall-clock without offset (`2026-09-27 15:00:00`). Concerts (Sunday chamber series) should be skipped. Ticket links go to conwayhall.ticketsolve.com.
- **Verdict: VIABLE** — JSON-LD from the listing only (1 request/run); sanitise control chars before serde_json; parse_london_wall_clock; interval 1440

### V&A (South Kensington + V&A East)
- URL: https://www.vam.ac.uk/whatson
- Categories: exhibition, talk, workshop, community (lates)
- robots: Allowed; notable: `Disallow: /*page=`, `/*p=`, `/*q=*`, `/whatson/20*/**/**/m*`; `Crawl-delay: 2`
- Bot response: 200 (nginx)
- Data: Listing has schema.org MICRODATA `Event` ×89 with itemprops name, startDate (date), endDate, location→Place/address, offers→Offer price/priceCurrency, image, description. Detail pages `/event/<id>/<slug>` carry JSON-LD `Event` with startDate/endDate incl. offset (`2026-10-02 13:00:00 +0100`), location, offers, image, url.
- Est. events: ~89 on first listing page
- Notes: Pagination via `page=` is robots-disallowed, so use the first listing page (+ optional capped detail fetches for times). Includes online livestreams (skip) and non-London sites (V&A Dundee, Wedgwood) — keep location London only.
- **Verdict: VIABLE** — microdata from /whatson listing + JSON-LD on capped detail pages (≤20); interval 1440

### Wellcome Collection
- URL: https://api.wellcomecollection.org/content/v0/events
- Categories: exhibition, talk, workshop, community
- robots: api.wellcomecollection.org has no robots.txt (404 = allow). Website robots: `Allow: /`, `Disallow: /account`
- Bot response: 200 JSON, no auth
- Data: Public Content API, no key (developer docs are CC-BY 4.0; API terms not separately stated): `GET /content/v0/events` → `{type:ResultList,totalResults:687,nextPage,results:[{id,uid,title,image,times:[{startDateTime,endDateTime,isFullyBooked}],format{label},locations{isOnline,attendance},isExhibition,isAvailableOnline,audiences,interpretations,series}]}`. Params: timespan=future, format, sort=times.startDateTime, sortOrder, page, pageSize≤100.
- Est. events: 687 total incl. past; tens future
- Notes: Permanent exhibitions have endDateTime 2090 → skip. Online-only events skip. Event URL = https://wellcomecollection.org/events/<uid> (exhibitions: /exhibitions/<uid>).
- **Verdict: VIABLE** — API (kind=api, no key): timespan=future&pageSize=100, 1–2 requests/run; interval 720

### British Library events
- URL: https://events.bl.uk/
- Categories: talk, workshop, exhibition, community
- robots: events.bl.uk robots: `User-agent: *` disallows only /cpresources/, /vendor/, /.env, /cache/
- Bot response: 200 (Cloudflare, no challenge)
- Data: RSS `https://events.bl.uk/feed.rss` (50 items: title, link, guid, description; pubDate is NOT the event date). Detail pages carry JSON-LD `Event`: name, startDate with offset (`2026-09-28T18:15:00+01:00`), location{Place}, image[], description; no endDate/offers.
- Est. events: ~50+ (paginated ?page=2)
- Notes: Some items are online or business/IP-centre sessions (skip online). Price must come from page HTML or stay unknown.
- **Verdict: VIABLE** — RSS for discovery + JSON-LD on capped detail pages (≤30 per run); interval 1440

### London Review Bookshop
- URL: https://www.londonreviewbookshop.co.uk/events
- Categories: talk (author events), community
- robots: `User-agent: * Allow: / Disallow: /account/`
- Bot response: 200
- Data: Listing has MICRODATA `Event` ×11 with itemprop name, startDate, image; CSS `section.event-preview`, `span.event-preview--date`, `h2.event-preview--title`, `span.event-preview--price`, `div.event-preview--desc`.
- Est. events: ~11
- Notes: Venue is the shop (14 Bury Place WC1A 2JL) for most; some are at other venues — check detail. Podcast items (PodcastEpisode microdata) skip.
- **Verdict: VIABLE** — microdata + CSS on listing, detail pages optional (cap 15); interval 1440

### Camden Art Centre
- URL: https://camdenartcentre.org/whats-on/in-the-building
- Categories: exhibition, talk, workshop
- robots: `User-agent: *` disallows only /cms/wp-admin/ (second group `Disallow:` empty)
- Bot response: 200 (Apache)
- Data: Detail pages carry JSON-LD `Event`: name, description, url, startDate/endDate (date-only), eventAttendanceMode, location{Place, PostalAddress}, performer. Listing has only Yoast WebPage graph.
- Est. events: ~5–15
- Notes: Small programme; also /whats-on/offsite and /on-demand (skip on-demand).
- **Verdict: VIABLE** — listing links + JSON-LD detail pages (cap 20); interval 1440

### Lisson Gallery
- URL: https://lisson.com/exhibitions
- Categories: exhibition
- robots: `User-Agent: * Disallow: /spotlight/* /guestbooks/* /*/draft` (GPTBot/Linguee blocked, not us)
- Bot response: 200 (lissongallery.com redirects to lisson.com)
- Data: Detail pages carry JSON-LD `ExhibitionEvent`: name, url, startDate, endDate (date-only), location{Place name, address: "London"}, image (bare asset id, not a URL).
- Est. events: ~23 listed worldwide; 2 London spaces
- Notes: International gallery: keep only location address London. Image field is not a URL — take og:image instead.
- **Verdict: VIABLE** — listing links + JSON-LD detail pages (cap 25); interval 1440

### City Lit
- URL: https://www.citylit.ac.uk/courses
- Categories: workshop (short courses: art & design, writing, craft)
- robots: Allowed for /courses; disallows `/searchcourse/`, `*?q=`, `/*?query=`, `/*?*limit=`, `/*?*order=`, etc.
- Bot response: 200
- Data: Listing carries JSON-LD `ItemList` of `Course` with `hasCourseInstance` → `CourseInstance`: startDate/endDate (`2026-09-26 00:00:00`), courseMode (onsite/online), location (string: "Keeley Street"), courseSchedule(repeatCount, repeatFrequency), offers{price, priceCurrency}. 28 per page.
- Est. events: hundreds (catalogue)
- Notes: Not schema Event: map Course+CourseInstance → workshop. Scope to one-day/short onsite courses in art/design/writing subject listing pages; skip multi-week and online. Needs a CourseInstance branch in the extractor (not an Event type).
- **Verdict: VIABLE** — JSON-LD Course/CourseInstance from 1–3 subject listing pages; interval 1440

### Horniman Museum
- URL: https://www.horniman.ac.uk/whats-on/
- Categories: workshop, exhibition, community, talk
- robots: `User-agent: * Crawl-delay: 5` (nothing disallowed)
- Bot response: 200 (nginx)
- Data: No Event JSON-LD (Yoast graph only); no Events Calendar REST (`/wp-json/tribe/...` 404). Server-rendered cards `article.item` with `div.date`, `div.time`, `div.excerpt`; detail pages have "Dates / Times / Tickets (Adult £65) / Location" blocks.
- Est. events: ~17 on listing
- Notes: Honour Crawl-delay 5 (FetchContext does). Many family events (keep as community) and permanent attractions (Aquarium — skip open-ended).
- **Verdict: VIABLE** — CSS on listing + detail (cap 20); interval 1440

### Estorick Collection
- URL: https://www.estorickcollection.com/events
- Categories: talk, workshop (life drawing, adult art classes), exhibition
- robots: `User-agent: * Disallow:` (all allowed)
- Bot response: 200 (nginx) — first-pass 'challenge' flag was a reCAPTCHA false positive
- Data: No JSON-LD. Server-rendered /events list: title, `27 September 2026`, `10:00 - 12:00`, type tags (`c-tag`: SPECIAL EVENT, TALK, FAMILIES…). /exhibitions has date ranges.
- Est. events: ~30 events + 2–3 exhibitions
- Notes: Stable utility-class CSS (`o-grid__item`, `c-tag`) — select by structure, not by generated colour classes.
- **Verdict: VIABLE** — CSS on /events and /exhibitions (2 requests, no detail pages needed); interval 1440

### South London Gallery
- URL: https://www.southlondongallery.org/whats-on/events/events-film-talks/
- Categories: talk, film (skip), workshop, exhibition
- robots: `User-agent: * Disallow:` (all allowed)
- Bot response: 200 (LiteSpeed) — 'challenge' flag was reCAPTCHA false positive
- Data: No Event JSON-LD. WordPress; server-rendered `div.post-summary` with `h2.post-title`, `span.datetime` (`WED 7 OCT 2026, 6:30-8:00pm`, sometimes no year: `THU 1 OCT, 6-9PM`). Category RSS feed exists (`…/events-film-talks/feed/`) but its dates are publish dates.
- Est. events: ~5 events + 2 exhibitions
- Notes: Year-less dates need inference (next occurrence). Exhibitions at /whats-on/exhibitions/.
- **Verdict: VIABLE** — CSS on 2 listings (+ detail pages for descriptions, cap 10); interval 1440

### ICA
- URL: https://www.ica.art/upcoming
- Categories: talk, exhibition, community (+ films to skip)
- robots: Allowed; disallows only /views/, /open-records-generator/, dev paths
- Bot response: 200 (Apache)
- Data: No JSON-LD. Server-rendered daily programme: `div.item <type>`, `div.title`, `div.time-slot`, `div.docket-date`; sections /talks, /exhibitions, /live.
- Est. events: ~200 items (mostly film screenings)
- Notes: Skip `films` items; use /talks and /exhibitions listings instead of /upcoming to keep volume small.
- **Verdict: VIABLE** — CSS on /talks + /exhibitions (+ capped detail); interval 1440

### Leighton House & Sambourne House (RBKC museums)
- URL: https://www.rbkc.gov.uk/museums/whats-on
- Categories: talk, workshop, course, exhibition, tour
- robots: rbkc.gov.uk Drupal robots: only core/profiles/admin paths disallowed
- Bot response: 200 (Cloudflare, no challenge)
- Data: No JSON-LD. Drupal cards: `div.card`, `a.card__heading-link`, `span.tag--<type>` (workshop/talk/exhibition/tour/course), `span.card__date`, `span.card__price`.
- Est. events: ~10 per page (paginated)
- Notes: Date strings like `24 September 2026 / Weekly on Sunday and Thursday` — take first date; recurring ones → skip or single occurrence. Old URL /museums/leighton-house/whats-on is 404.
- **Verdict: VIABLE** — CSS on listing (1–2 pages); interval 1440

### The Photographers' Gallery
- URL: https://thephotographersgallery.org.uk/whats-on
- Categories: exhibition, talk, workshop (courses)
- robots: Drupal robots: only core/profiles/admin paths disallowed
- Bot response: 200 (nginx)
- Data: No JSON-LD. Server-rendered teasers `article.o-event.o-teaser` with `p.o-teaser__date` (`Wed 24 Jun 2026 - Sun 27 Sep 2026`), `h3.o-teaser__title`, `h3.o-teaser__pre-title` (type), body text.
- Est. events: ~12
- Notes: Ticketing via ticketsolve (don't fetch). Open calls appear in listing — skip.
- **Verdict: VIABLE** — CSS on listing (+ capped detail); interval 1440

### Sir John Soane's Museum
- URL: https://www.soane.org/whats-on
- Categories: exhibition, talk, workshop, course, tour, lates
- robots: Drupal robots: only core/profiles/admin paths disallowed
- Bot response: 200 (Cloudflare, no challenge)
- Data: No JSON-LD. Server-rendered listing: type label, date range (`17 Oct 2025 - 31 Dec 2026`), `Tickets: £25`, teaser; filters `?type=<id>` (e.g. `?type=10` = Tours; read the other IDs from the listing's filter links).
- Est. events: ~30
- Notes: Online-only exhibitions and year-long daily tours should be skipped (open-ended/online).
- **Verdict: VIABLE** — CSS on listing (1–2 filtered pages); interval 1440

## VIABLE — backlog

### National Gallery (exhibitions)
- URL: https://www.nationalgallery.org.uk/exhibitions
- Categories: exhibition
- robots: Allowed (`Disallow: /custom/popups`, `/external`)
- Bot response: 200 (Cloudflare, no challenge)
- Data: No JSON-LD; exhibition cards with `3 October 2026 – 31 January 2027` ranges server-rendered. /events is JS-loaded ('Loading…').
- Est. events: ~6 exhibitions
- Notes: Events programme needs JS → exhibitions only.
- **Verdict: VIABLE-backlog** — CSS on /exhibitions; interval 1440

### White Cube (London)
- URL: https://www.whitecube.com/exhibitions/london
- Categories: exhibition
- robots: Allowed (Craft CMS paths only)
- Bot response: 200 (Cloudflare, no challenge)
- Data: No Event JSON-LD; detail: `Dates 16 September – 8 November 2026 Location White Cube Bermondsey …`.
- Est. events: ~4 London
- Notes: Commercial gallery; small.
- **Verdict: VIABLE-backlog** — CSS; interval 1440

### Chisenhale Gallery
- URL: https://chisenhale.org.uk/whats-on/
- Categories: exhibition, talk
- robots: Allowed; `Crawl-delay: 20`
- Bot response: 200 (nginx)
- Data: No JSON-LD; server-rendered `2 October – 6 December`, `3 October, 3–5pm` (year-less).
- Est. events: ~5
- Notes: Crawl-delay 20 → 1–2 requests only.
- **Verdict: VIABLE-backlog** — CSS on 1 page; interval 1440

### D&AD events
- URL: https://www.dandad.org/events
- Categories: talk (creative industry)
- robots: Allowed (disallows /search/, /basket/, /account/)
- Bot response: 200
- Data: No JSON-LD; server-rendered 'Upcoming events' with Date/Location/Price/Speakers.
- Est. events: ~1–5
- Notes: Small but squarely creative-industry.
- **Verdict: VIABLE-backlog** — CSS; interval 1440

### Garden Museum
- URL: https://www.gardenmuseum.org.uk/whats-on/
- Categories: exhibition, talk, workshop, community
- robots: `Disallow:` empty
- Bot response: 200 (nginx)
- Data: No Event JSON-LD; no Events Calendar REST; detail shows `18 Oct 2026, 11am - 4pm`, price.
- Est. events: ~10
- Notes: —
- **Verdict: VIABLE-backlog** — CSS listing + detail; interval 1440

### Goldsmiths CCA
- URL: https://goldsmithscca.art/
- Categories: exhibition
- robots: Allowed
- Bot response: 200 (/whats-on/ redirects to home)
- Data: No JSON-LD; homepage lists exhibitions with `25 September–13 December 2026`.
- Est. events: ~2–3
- Notes: —
- **Verdict: VIABLE-backlog** — CSS on home; interval 1440

### Courtauld Gallery
- URL: https://courtauld.ac.uk/whats-on/
- Categories: exhibition, talk
- robots: Allowed
- Bot response: 200
- Data: WordPress; no Event JSON-LD; `2 October 2026 – 10 January 2027` in cards; RSS is posts only.
- Est. events: ~5
- Notes: —
- **Verdict: VIABLE-backlog** — CSS; interval 1440

### Mall Galleries
- URL: https://www.mallgalleries.org.uk/exhibitions-events
- Categories: exhibition (art society annuals)
- robots: Allowed
- Bot response: 200 (/whats-on meta-refreshes here)
- Data: No JSON-LD; `16 Sep 2026 - 26 Sep 2026 | North, West and East Galleries`.
- Est. events: ~3–6
- Notes: —
- **Verdict: VIABLE-backlog** — CSS; interval 1440

## MAYBE

### ArtRabbit (aggregator)
- URL: https://www.artrabbit.com/all-shows/united-kingdom/london
- Categories: exhibition, talk, community (open-submission contemporary art)
- robots: Allowed for /all-shows and /events (disallows /ajax/*.tpl, /account/, /*.php …)
- Bot response: 200 (openresty)
- Data: Detail pages JSON-LD `Event`: name, startDate/endDate, eventStatus, eventAttendanceMode, location{Place, full PostalAddress}, image, description, organizer, performer. Listing: 349 current London events, 21 per page.
- Est. events: ~349
- Notes: Terms (artrabbit.com/about/terms) prohibit 'reproducing, copying … or incorporating into any other materials, any of the Website' — a general IP clause, no explicit scraping ban. Highest-volume source found. Needs owner decision (facts + link only, or ask support@artrabbit.com).
- **Verdict: MAYBE** — owner decision on terms; then JSON-LD detail pages with cap

### Art Fund (National Art Pass)
- URL: https://www.artfund.org/explore/exhibitions
- Categories: exhibition
- robots: No robots.txt (404 → allow)
- Bot response: 200
- Data: Detail JSON-LD `Event` with startDate/endDate/offers/image but location name/address EMPTY; national scope.
- Est. events: hundreds UK-wide
- Notes: Venue would need CSS; London filtering only via search (JS). Mostly duplicates institutions we scrape directly.
- **Verdict: MAYBE** — low priority

### Culture Calling
- URL: https://www.culturecalling.com/london
- Categories: exhibition, community
- robots: `User-agent: *` no rules; sitemaps incl. /sitemap/events
- Bot response: 200 (Cloudflare, no challenge)
- Data: Detail JSON-LD `Event` but sampled one is a venue ('Wellcome Collection', 2026-03-26→2027-05-30) — pseudo-events.
- Est. events: unknown
- Notes: Quality/duplication concerns; mostly aggregates venues we cover.
- **Verdict: MAYBE** — needs quality review

### Tate (Modern/Britain)
- URL: https://www.tate.org.uk/whats-on
- Categories: exhibition, talk, workshop, lates
- robots: Allowed (`Disallow: /search`)
- Bot response: 200 (gunicorn)
- Data: No JSON-LD; detail shows only 'Until 3 January 2027' (no start date) — open-ended per project rules. Listing filters via query string.
- Est. events: large
- Notes: Would need start dates from another page element; worth a closer look.
- **Verdict: MAYBE** — needs start-date source

### Natural History Museum
- URL: https://www.nhm.ac.uk/whats-on.html
- Categories: exhibition, talk, lates
- robots: Allowed; `Crawl-delay: 8`; `Disallow: /visit/whats-on/programs/*`
- Bot response: 200 (Cloudflare, no challenge)
- Data: No JSON-LD; __NEXT_DATA__ present; exhibition detail shows `16 October 2026 - 11 July 2027`. Events on ticketing.nhm.ac.uk.
- Est. events: ~10
- Notes: Science-leaning; lower fit.
- **Verdict: MAYBE** — CSS on exhibitions only

### Royal Institution
- URL: https://www.rigb.org/whats-on
- Categories: talk, workshop
- robots: Drupal robots: allowed
- Bot response: 200 (nginx)
- Data: Listing is JS-loaded (no events in HTML).
- Est. events: unknown
- Notes: Detail pages untested (budget).
- **Verdict: MAYBE** — check detail pages / Drupal JSON

### Science Museum (Lates)
- URL: https://www.sciencemuseum.org.uk/see-and-do
- Categories: exhibition, lates
- robots: Allowed; `Crawl-Delay: 20`
- Bot response: 200 (Cloudflare, no challenge)
- Data: JSON-LD Organization only; listing mostly JS.
- Est. events: unknown
- Notes: —
- **Verdict: MAYBE** — low priority

### Museum of the Home
- URL: https://museumofthehome.org.uk/whats-on/
- Categories: exhibition, workshop, community
- robots: `Disallow:` empty
- Bot response: 200 (nginx)
- Data: Yoast graph only; detail page thin (2.7k chars text); no tribe REST.
- Est. events: ~5
- Notes: —
- **Verdict: MAYBE** — CSS, low content

### Quentin Blake Centre for Illustration
- URL: https://qbcentre.org.uk/whats-on
- Categories: exhibition, talk, workshop, courses
- robots: No robots.txt (404 → allow)
- Bot response: 200
- Data: No JSON-LD; exhibitions shown as 'From Friday 23 October' (open-ended); events listing JS-filtered.
- Est. events: ~10
- Notes: New venue (opened 2026); strong fit — re-check detail pages.
- **Verdict: MAYBE** — re-survey detail pages

### Zabludowicz Collection
- URL: https://www.zabludowiczcollection.com/archive/events
- Categories: exhibition, talk, workshop
- robots: No robots.txt (404 → allow)
- Bot response: 200
- Data: No JSON-LD; server-rendered archive; 'There are no current events' (London programme dormant).
- Est. events: 0 current
- Notes: —
- **Verdict: MAYBE** — revisit if London programme resumes

### Delfina Foundation
- URL: https://www.delfinafoundation.com/whats-on/
- Categories: talk, exhibition
- robots: `Disallow:` empty
- Bot response: 200 (Apache)
- Data: Yoast only; listing is filter UI, items not visible in HTML.
- Est. events: unknown
- Notes: —
- **Verdict: MAYBE** — check detail pages

### Institute of Making
- URL: https://www.instituteofmaking.org.uk/events
- Categories: workshop, talk
- robots: robots.txt returns an HTML 404 page (→ allow)
- Bot response: 200
- Data: No JSON-LD; many events members-only (UCL).
- Est. events: ~5
- Notes: —
- **Verdict: MAYBE** — members-only concern

### Autograph
- URL: https://autograph.org.uk/exhibitions
- Categories: exhibition
- robots: Allowed
- Bot response: 200
- Data: No JSON-LD; gallery closed until 8 Oct 2026.
- Est. events: ~2
- Notes: —
- **Verdict: MAYBE** — re-check after reopening

### Royal Society of Literature
- URL: https://rsliterature.org/events-from-rsl/
- Categories: talk
- robots: Allowed
- Bot response: 200 (/events/ redirects)
- Data: Page mixes past events/articles; WP RSS is posts.
- Est. events: unknown
- Notes: —
- **Verdict: MAYBE** — no clear upcoming list

### London Museum (Docklands)
- URL: https://www.londonmuseum.org.uk/whats-on/
- Categories: exhibition, community
- robots: Allowed
- Bot response: 200
- Data: No JSON-LD in listing; West Smithfield opens 28 Nov 2026.
- Est. events: unknown
- Notes: —
- **Verdict: MAYBE** — re-survey after opening

### London Craft Week
- URL: https://londoncraftweek.com/events/
- Categories: workshop, exhibition, expo (annual, May)
- robots: Allowed (WooCommerce paths disallowed)
- Bot response: 200
- Data: WordPress/WooCommerce; events are `/product/` pages; no Event JSON-LD.
- Est. events: ~200 during festival
- Notes: Annual; revisit in spring 2027.
- **Verdict: MAYBE** — seasonal

### Open House Festival
- URL: https://programme.openhouse.org.uk/
- Categories: expo, talk, workshop, tour (annual, Sept)
- robots: Robots all commented out (allow)
- Bot response: 200 (Heroku)
- Data: Server-rendered programme, no JSON-LD; 2026 festival (12–20 Sep) just ended.
- Est. events: ~800 during festival
- Notes: Revisit summer 2027.
- **Verdict: MAYBE** — seasonal

### London Art Fair
- URL: https://www.londonartfair.co.uk/
- Categories: expo (annual, Jan)
- robots: Allowed
- Bot response: 200 (Cloudflare, no challenge)
- Data: Homepage JSON-LD `Event` for the fair itself (start/end with offset, offers, location).
- Est. events: 1
- Notes: One event per year — could be part of a small 'fairs' source.
- **Verdict: MAYBE** — fairs bundle

### Clerkenwell Design Week
- URL: https://www.clerkenwelldesignweek.com/
- Categories: expo (annual, May)
- robots: `Allow: /`
- Bot response: 200
- Data: Homepage JSON-LD `Event` for the festival (2027-05-25→27, Z times).
- Est. events: 1
- Notes: As above.
- **Verdict: MAYBE** — fairs bundle

### Affordable Art Fair (Battersea/Hampstead)
- URL: https://affordableartfair.com/fairs/london-battersea-autumn/
- Categories: expo
- robots: `Disallow:` empty
- Bot response: 200
- Data: No Event JSON-LD; `14 – 18 October 2026` text; an invite.ics upload exists.
- Est. events: 2–3 per year
- Notes: —
- **Verdict: MAYBE** — fairs bundle

### Victoria Miro
- URL: https://www.victoria-miro.com/exhibitions
- Categories: exhibition
- robots: Allowed
- Bot response: 200 (gunicorn); detail /exhibitions/685/ 301
- Data: No JSON-LD
- Est. events: ~5 London
- Notes: —
- **Verdict: MAYBE** — CSS, commercial

### Gagosian / Pace / Thaddaeus Ropac
- URL: https://gagosian.com/exhibitions/
- Categories: exhibition
- robots: Allowed (Gagosian disallows archive query forms)
- Bot response: 200
- Data: No JSON-LD; international lists needing London filtering; Gagosian is Next.js.
- Est. events: few London each
- Notes: —
- **Verdict: MAYBE** — CSS, commercial, London filter

### Bishopsgate Institute / Morley College / RIBA
- URL: https://www.bishopsgate.org.uk/whats-on
- Categories: talk, workshop, courses
- robots: Allowed (Morley, RIBA: no robots / redirect)
- Bot response: redirect chains (bishopsgate → /whats-on/search; morley → apex; architecture.com → riba.org) not resolved within the 3-page budget
- Data: not evaluated
- Est. events: 
- Notes: Strong fits for talks/workshops — re-survey with correct URLs.
- **Verdict: MAYBE** — re-survey

### Design Council / City of London libraries / Guildhall Art Gallery
- URL: https://www.designcouncil.org.uk/
- Categories: talk, community
- robots: Allowed
- Bot response: guessed events URLs 404
- Data: not found
- Est. events: 
- Notes: No public events listing found.
- **Verdict: MAYBE** — find listing URL

### Idler
- URL: https://www.idler.co.uk/shop/events-talks/
- Categories: talk, workshop (mostly online courses)
- robots: Allowed
- Bot response: /courses/ redirects to /my/courses/ (account); /shop/events-talks/ 301
- Data: not evaluated
- Est. events: 
- Notes: Mostly online.
- **Verdict: MAYBE** — low fit

## REFUSED

| name | url | domain | reason_code | reason_text |
|---|---|---|---|---|
| CreativeMornings London | https://creativemornings.com/cities/lon | creativemornings.com | robots_disallowed | robots.txt disallows the bot; empty 202 to bot requests (earlier survey, issue #4) |
| Southbank Centre | https://www.southbankcentre.co.uk/whats-on | southbankcentre.co.uk | bot_blocked | 403 Cloudflare to the bot (earlier survey, issue #6) |
| Hayward Gallery | https://www.southbankcentre.co.uk/venues/hayward-gallery | southbankcentre.co.uk | bot_blocked | Part of Southbank Centre site: 403 + Cloudflare 'Just a moment' challenge (cf-mitigated: challenge) |
| Royal Academy of Arts | https://www.royalacademy.org.uk/exhibitions-and-events | royalacademy.org.uk | bot_blocked | 403 + Cloudflare 'Just a moment' challenge (cf-mitigated: challenge); robots itself allows, Crawl-delay 10 |
| National Portrait Gallery | https://www.npg.org.uk/whats-on | npg.org.uk | bot_blocked | 403 from Cloudflare on the listing page |
| Dulwich Picture Gallery | https://www.dulwichpicturegallery.org.uk/whats-on/ | dulwichpicturegallery.org.uk | bot_blocked | 403 + Cloudflare challenge (cf-mitigated: challenge) |
| RSA (Royal Society of Arts) | https://www.thersa.org/events | thersa.org | bot_blocked | 403 + Cloudflare challenge (cf-mitigated: challenge) |
| Frieze London / Masters | https://www.frieze.com/fairs/frieze-london | frieze.com | bot_blocked | 403 from Cloudflare |
| Photo London | https://www.photolondon.org/ | photolondon.org | bot_blocked | 403 + Cloudflare challenge (cf-mitigated: challenge) |
| Crafts Council / Collect | https://www.craftscouncil.org.uk/whats-on | craftscouncil.org.uk | bot_blocked | 403 + Cloudflare challenge (cf-mitigated: challenge) |
| Londonist | https://londonist.com/things-to-do | londonist.com | bot_blocked | 403 + Cloudflare challenge (cf-mitigated: challenge) |
| Visit London | https://www.visitlondon.com/things-to-do/whats-on/art-and-exhibitions | visitlondon.com | bot_blocked | 403 + Cloudflare challenge (cf-mitigated: challenge) |
| The School of Life (London) | https://www.theschooloflife.com/london/ | theschooloflife.com | bot_blocked | 403 + Cloudflare challenge (cf-mitigated: challenge) |
| Society of Authors | https://societyofauthors.org/events/ | societyofauthors.org | bot_blocked | 403 Forbidden (Cloudflare) |
| Hauser & Wirth | https://www.hauserwirth.com/hauser-wirth-exhibitions/ | hauserwirth.com | bot_blocked | 429 Too Many Requests (Vercel) on the first request, including robots.txt |
| Poetry Society / Poetry Café | https://poetrysociety.org.uk/events/ | poetrysociety.org.uk | bot_blocked | 403 on robots.txt itself |
| Gresham College | https://www.gresham.ac.uk/whats-on | gresham.ac.uk | bot_blocked | 403 on robots.txt itself |
| Royal Society | https://royalsociety.org/science-events-and-lectures/ | royalsociety.org | bot_blocked | 403 on robots.txt itself |
| Kings Place | https://www.kingsplace.co.uk/whats-on/ | kingsplace.co.uk | bot_blocked | 403 on robots.txt itself |
| Foyles | https://www.foyles.co.uk/events | foyles.co.uk | bot_blocked | 403 on robots.txt itself |
| Building Centre | https://www.buildingcentre.co.uk/whats-on | buildingcentre.co.uk | bot_blocked | 403 on robots.txt itself |
| London Transport Museum | https://www.ltmuseum.co.uk/whats-on | ltmuseum.co.uk | bot_blocked | 403 Cloudflare block page ('Attention Required'-style, no cf-mitigated header) |
| British Museum | https://www.britishmuseum.org/exhibitions-events | britishmuseum.org | bot_blocked | Listing 200 but exhibition detail page 403 + Cloudflare challenge; robots Crawl-delay 20 |
| Eventbrite | https://www.eventbrite.co.uk/d/united-kingdom--london/arts/ | eventbrite.co.uk | terms | Terms of Service forbid scraping/crawling/automated extraction (listing does carry JSON-LD Event x20). Public event-search API removed ~2020 |
| Meetup | https://www.meetup.com/find/ | meetup.com | terms | Terms forbid scraping (listing carries JSON-LD Event); GraphQL API needs OAuth client, creatable only with Meetup Pro (paid) |
| Time Out London | https://www.timeout.com/london/art | timeout.com | no_event_data | Editorial listicles, no structured event data (Article/Person JSON-LD only); robots disallows */rss/Events and paginate paths |
| DICE | https://dice.fm/browse/london | dice.fm | other | Music/nightlife ticketing, out of scope; listing client-rendered (Next.js), no Event JSON-LD; robots disallows /api/ |
| London Design Festival | https://www.londondesignfestival.com/events | londondesignfestival.com | js_only | Client-rendered Next.js app: HTML body is only 'Loading…', no JSON-LD or embedded data |
| Makerversity | https://makerversity.org/ | makerversity.org | js_only | Client-rendered SPA (≈100 chars of HTML text); no public events listing found |
| London Writers' Salon | https://londonwriterssalon.com/ | londonwriterssalon.com | other | Events live on community.londonwriterssalon.com (Circle community, login wall); /events 404; mostly online |
| Art Night | https://artnight.london/ | artnight.london | other | TLS certificate hostname mismatch on both artnight.london and www — site unreachable securely on 2026-09-26; re-check later |
