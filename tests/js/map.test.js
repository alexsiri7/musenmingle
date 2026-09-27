// Unit tests for the pure helpers in src/map.mjs (the /map script).
// Run with `node --test tests/js/*.test.js` (no dependencies; CI runs it).
"use strict";
const test = require("node:test");
const assert = require("node:assert/strict");

const load = () => import("../../src/map.mjs");

test("distances and walking labels", async () => {
  const m = await load();
  // Somerset House to the Barbican: about 1.8 km as the crow flies.
  const km = m.haversineKm({ lat: 51.511, lng: -0.1171 }, { lat: 51.5201, lng: -0.0955 });
  assert.ok(km > 1.7 && km < 1.9, String(km));
  assert.equal(m.walkMinutes(0), 1);
  assert.equal(m.walkMinutes(1), 16); // 1.3 km walked at 5 km/h
  assert.equal(m.walkLabel(0.62), "≈ 10 min walk · 0.6 km");
  assert.equal(m.areaLabel(0.05), "At the area centre");
  assert.equal(m.areaLabel(1.26), "1.3 km from area centre");
});

test("live labels match the server's (London time)", async () => {
  const m = await load();
  const now = "2026-10-03T13:00:00Z"; // Sat 14:00 BST
  const cases = [
    ["2026-09-30T23:00:00Z", "2026-10-19T23:00:00Z", true, "Open today · check opening hours"],
    ["2025-02-10T10:00:00Z", "2027-01-01T10:00:00Z", false, "Open today · check opening hours"],
    ["2026-10-02T23:00:00Z", null, true, "Today · check opening hours"],
    ["2026-10-03T15:30:00Z", "2026-10-03T17:00:00Z", false, "Starts 16:30–18:00"],
    ["2026-10-03T15:30:00Z", null, false, "Starts 16:30"],
    ["2026-10-03T12:30:00Z", "2026-10-03T14:00:00Z", false, "On now until 15:00"],
    ["2026-10-03T12:30:00Z", null, false, "Started 13:30"],
    ["2026-10-03T17:00:00Z", "2026-11-03T18:00:00Z", false, "Opens 18:00"],
  ];
  for (const [s, e, allDay, want] of cases) assert.equal(m.liveLabel(s, e, allDay, now), want, s);
  // Past London midnight the weekday is named.
  const late = "2026-10-03T22:00:00Z"; // 23:00 BST
  assert.equal(m.liveLabel("2026-10-04T00:00:00Z", null, false, late), "Starts Sun 01:00");
  assert.equal(m.liveLabel("2026-10-03T23:00:00Z", null, true, late), "Sun · check opening hours");
  assert.equal(
    m.liveLabel("2026-10-03T23:00:00Z", "2026-10-29T00:00:00Z", true, late),
    "Opens Sun · check opening hours"
  );
});

const EVENT = {
  id: "0414a989-8ba8-41da-a6ac-b92720727755",
  title: "Concrete and Clay",
  venue_name: "Barbican",
  lat: 51.5201,
  lng: -0.0955,
  starts_at: "2026-10-03T12:00:00Z",
  ends_at: "2026-10-03T14:00:00Z",
  all_day: false,
  is_free: false,
  price_min: "5.00",
  price_max: "12.50",
  currency: "GBP",
  category: "talk",
  thumbnail_url: "/thumbs/0414a989-8ba8-41da-a6ac-b92720727755-abc.jpg",
  image_credit: { name: "Barbican", url: "https://www.barbican.org.uk/x" },
  sources: [
    { source: "x", display_name: "Bad", url: "javascript:alert(1)" },
    { source: "barbican", display_name: "Barbican", url: "https://www.barbican.org.uk/x" },
  ],
};

test("events from JSON keep only safe links and our own thumbnails", async () => {
  const m = await load();
  const e = m.eventFromJson(EVENT, "2026-10-03T12:30:00Z");
  assert.equal(e.cta.name, "Barbican");
  assert.equal(e.cta.url, "https://www.barbican.org.uk/x");
  assert.equal(e.price, "£5–£12.50");
  assert.equal(e.category, "Talk");
  assert.equal(e.status, "On now until 15:00");
  assert.equal(e.thumb.src, EVENT.thumbnail_url);
  const hotlink = m.eventFromJson(
    Object.assign({}, EVENT, { thumbnail_url: "https://venue.example/i.jpg", sources: [] }),
    "2026-10-03T12:30:00Z"
  );
  assert.equal(hotlink.thumb, null);
  assert.equal(hotlink.cta, null);
  assert.equal(m.priceLabel({ is_free: true }), "Free");
  assert.equal(m.priceLabel({ price_max: 20 }), "Up to £20");
  assert.equal(m.priceLabel({}), null);
});

test("server cards parse from their data attributes", async () => {
  const m = await load();
  const e = m.eventFromDataset({
    eventId: EVENT.id,
    n: "3",
    lat: "51.5",
    lng: "-0.1",
    title: "T",
    thumb: "/thumbs/x.jpg",
    creditName: "Barbican",
    creditUrl: "https://www.barbican.org.uk/",
    ctaUrl: "javascript:void(0)",
  });
  assert.equal(e.n, 3);
  assert.equal(e.lat, 51.5);
  assert.equal(e.thumb.credit.name, "Barbican");
  assert.equal(e.cta, null);
  assert.equal(m.eventFromDataset({ eventId: "x" }).lat, null);
});

test("nearest first, then soonest", async () => {
  const m = await load();
  const here = { lat: 51.511, lng: -0.1171 };
  const evs = [
    { id: "far", lat: 51.55, lng: -0.1, starts_at: "2026-10-03T13:00:00Z" },
    { id: "none", lat: null, lng: null, starts_at: "2026-10-03T13:00:00Z" },
    { id: "near-later", lat: 51.512, lng: -0.117, starts_at: "2026-10-03T16:00:00Z" },
    { id: "mid-ongoing", lat: 51.52, lng: -0.12, starts_at: "2026-09-01T00:00:00Z" },
  ];
  const sorted = m.byDistanceFrom(here, evs);
  assert.deepEqual(sorted.map((e) => e.id), ["near-later", "mid-ongoing", "far"]);
  const soon = m.sortEvents(sorted, "soon", "2026-10-03T12:00:00Z");
  assert.deepEqual(soon.map((e) => e.id), ["mid-ongoing", "far", "near-later"]);
  assert.deepEqual(m.sortEvents(soon, "near", "2026-10-03T12:00:00Z").map((e) => e.id), [
    "near-later",
    "mid-ongoing",
    "far",
  ]);
});

function geolocation(outcome) {
  return {
    asked: 0,
    getCurrentPosition(ok, fail, opts) {
      this.asked++;
      this.opts = opts;
      if (outcome.coords) ok({ coords: outcome.coords });
      else fail({ code: outcome.code });
    },
  };
}

test("location flow: asks once, maps failures to a fallback message", async () => {
  const m = await load();
  const g = geolocation({ coords: { latitude: 51.5, longitude: -0.12 } });
  assert.deepEqual(await m.locate(g), { lat: 51.5, lng: -0.12 });
  assert.equal(g.asked, 1);
  assert.ok(g.opts.timeout > 0);
  for (const [code, reason] of [
    [1, "denied"],
    [2, "unavailable"],
    [3, "timeout"],
  ]) {
    await assert.rejects(m.locate(geolocation({ code })), (e) => e.reason === reason);
  }
  await assert.rejects(m.locate(undefined), (e) => e.reason === "unsupported");
  assert.equal(
    m.locateMessage("denied", "Central & South Bank"),
    "Location permission was not given. Showing Central & South Bank instead; pick another area below."
  );
});

test("London-wide fetch pages by cursor and never sends a location", async () => {
  const m = await load();
  const urls = [];
  const pages = [
    { events: [{ id: "a" }], next_cursor: "c1" },
    { events: [{ id: "b" }], next_cursor: null },
  ];
  const fetchFn = async (url) => {
    urls.push(url);
    return { ok: true, json: async () => pages[urls.length - 1] };
  };
  const events = await m.fetchLondon(fetchFn, "today");
  assert.deepEqual(events.map((e) => e.id), ["a", "b"]);
  assert.deepEqual(urls, ["/v1/events?at=today&limit=100", "/v1/events?at=today&limit=100&cursor=c1"]);
  for (const u of urls) assert.ok(!/near|lat|lng/.test(u), u);
  await assert.rejects(m.fetchLondon(async () => ({ ok: false, status: 500 }), "now"));
});

test("transit origin and card suffix", async () => {
  const m = await load();
  assert.equal(m.transitOrigin({ lat: 51.5074, lng: -0.1278 }), "51.508,-0.129");
  assert.equal(m.transitSuffix({ status: "ok", transit: { minutes: 18 } }), " · 🚇 18 min");
  assert.equal(m.transitSuffix({ status: "unavailable", transit: null }), "");
  assert.equal(m.transitSuffix(null), "");
  assert.equal(m.MIN_TRANSIT_KM, 1.5);
});
