// Unit tests for the pure helpers in src/web.js (the Saved-events script).
// Run with `node --test tests/js/*.test.js` (no dependencies; CI runs it).
"use strict";
const test = require("node:test");
const assert = require("node:assert/strict");
const app = require("../../src/web.js");

function memoryStorage() {
  const data = new Map();
  return {
    getItem: (k) => (data.has(k) ? data.get(k) : null),
    setItem: (k, v) => data.set(k, String(v)),
    removeItem: (k) => data.delete(k),
    data,
  };
}

const ID1 = "0414a989-8ba8-41da-a6ac-b92720727755";
const ID2 = "1f3632de-af3f-4c79-9ecb-f4bc48b0821f";

test("toggle saves with a snapshot, newest first, and unsaves", () => {
  let items = [];
  items = app.toggle(items, { id: ID1, title: "Concrete and Clay", venue: "Barbican",
    starts_at: "2026-10-01T18:00:00Z", ends_at: "" }, "2026-09-26T10:00:00Z");
  items = app.toggle(items, { id: ID2, title: "Life drawing" }, "2026-09-26T11:00:00Z");
  assert.deepEqual(items.map((i) => i.id), [ID2, ID1]);
  assert.deepEqual(items[1].snapshot, { title: "Concrete and Clay", venue: "Barbican",
    starts_at: "2026-10-01T18:00:00Z", ends_at: null, all_day: false });
  assert.ok(app.isSaved(items, ID1));
  items = app.toggle(items, { id: ID1, title: "x" }, "2026-09-26T12:00:00Z");
  assert.deepEqual(items.map((i) => i.id), [ID2]);
});

test("storage round-trips under the versioned key and tolerates failures", () => {
  const s = memoryStorage();
  const items = app.toggle([], { id: ID1, title: "A" }, "2026-09-26T10:00:00Z");
  assert.equal(app.storeSaved(s, items), true);
  assert.equal(app.STORAGE_KEY, "musenmingle.saved.v1");
  assert.ok(s.data.has("musenmingle.saved.v1"));
  assert.deepEqual(app.loadSaved(s), items);
  // Missing storage, corrupt JSON, junk entries, throwing storage.
  assert.deepEqual(app.loadSaved(null), []);
  assert.equal(app.storeSaved(null, items), false);
  s.setItem(app.STORAGE_KEY, "{not json");
  assert.deepEqual(app.loadSaved(s), []);
  s.setItem(app.STORAGE_KEY, JSON.stringify({ items: [{ id: "<script>" }, items[0]] }));
  assert.deepEqual(app.loadSaved(s), items);
  const throwing = { getItem() { throw new Error("denied"); }, setItem() { throw new Error("quota"); } };
  assert.deepEqual(app.loadSaved(throwing), []);
  assert.equal(app.storeSaved(throwing, items), false);
});

test("migrateStorage moves the legacy letsart key to the new key once", () => {
  const items = app.toggle([], { id: ID1, title: "A" }, "2026-09-26T10:00:00Z");
  const legacy = JSON.stringify({ items });
  assert.deepEqual(app.LEGACY_STORAGE_KEYS, ["letsart.saved.v1"]);

  // Old key only: copied to the new key, old key removed.
  const s = memoryStorage();
  s.setItem("letsart.saved.v1", legacy);
  assert.equal(app.migrateStorage(s), true);
  assert.equal(s.data.get("musenmingle.saved.v1"), legacy);
  assert.ok(!s.data.has("letsart.saved.v1"));
  assert.deepEqual(app.loadSaved(s), items);
  // Idempotent.
  assert.equal(app.migrateStorage(s), false);
  assert.equal(s.data.get("musenmingle.saved.v1"), legacy);

  // New key already present: never overwritten, old key left alone.
  const both = memoryStorage();
  both.setItem("musenmingle.saved.v1", JSON.stringify({ items: [] }));
  both.setItem("letsart.saved.v1", legacy);
  assert.equal(app.migrateStorage(both), false);
  assert.equal(both.data.get("musenmingle.saved.v1"), JSON.stringify({ items: [] }));
  assert.ok(both.data.has("letsart.saved.v1"));

  // Nothing stored, no storage, or throwing storage: no-op.
  const empty = memoryStorage();
  assert.equal(app.migrateStorage(empty), false);
  assert.equal(empty.data.size, 0);
  assert.equal(app.migrateStorage(null), false);
  const throwing = { getItem() { throw new Error("denied"); }, setItem() {}, removeItem() {} };
  assert.equal(app.migrateStorage(throwing), false);
});

test("when matches the server's wording in London time", () => {
  const now = "2026-10-01T12:00:00Z";
  assert.equal(app.when("2026-10-03T17:00:00Z", "2026-10-03T19:00:00Z", now), "Sat 3 Oct 2026, 18:00–20:00");
  assert.equal(app.when("2026-09-01T00:00:00Z", "2027-01-03T00:00:00Z", now), "Until Sun 3 Jan 2027");
  assert.equal(app.when("2026-11-01T00:00:00Z", "2027-01-03T00:00:00Z", now), "Sun 1 Nov 2026 – Sun 3 Jan 2027");
  assert.equal(app.when("2026-10-02T23:00:00Z", null, now), "Sat 3 Oct 2026");
  assert.equal(app.when("2026-09-28T10:09:00Z", null, now), "Mon 28 Sep 2026, 11:09");
  assert.equal(app.when("2026-10-02T23:00:00Z", null, now, true), "Sat 3 Oct 2026, all day");
  assert.equal(app.when("2026-11-01T00:00:00Z", "2027-01-03T00:00:00Z", now, true), "Sun 1 Nov 2026 – Sun 3 Jan 2027");
});

test("dayOfMonth is the London day, two digits (the placeholder numeral, like web::blank)", () => {
  assert.equal(app.dayOfMonth("2026-10-02T23:00:00Z"), "03"); // 00:00 BST on the 3rd
  assert.equal(app.dayOfMonth("2026-12-31T12:00:00Z"), "31");
});

test("price matches the server's wording", () => {
  assert.equal(app.price({ is_free: true }), "Free");
  assert.equal(app.price({ is_free: false, price_min: "5.00", price_max: "12.50", currency: "GBP" }), "£5–£12.50");
  assert.equal(app.price({ is_free: false, price_min: "8", price_max: "8" }), "£8");
  assert.equal(app.price({ is_free: false, price_max: "20", currency: "EUR" }), "Up to €20");
  assert.equal(app.price({ is_free: false }), null);
});

test("toICS escapes text, uses UTC and folds long lines", () => {
  const ics = app.toICS(
    [
      { id: ID1, title: "Talk; with, commas\nand lines", venue: "Barbican", starts_at: "2026-10-01T17:00:00Z",
        ends_at: "2026-10-01T19:00:00Z" },
      { id: ID2, title: "x".repeat(120), venue: null, starts_at: "2026-10-02T09:00:00Z", ends_at: null },
      { id: ID2, title: "no start", starts_at: null },
    ],
    "2026-09-26T10:00:00.123Z",
    "https://musenmingle.interstellarai.net"
  );
  const lines = ics.split("\r\n");
  const unfolded = ics.replace(/\r\n /g, "").split("\r\n");
  assert.equal(lines[0], "BEGIN:VCALENDAR");
  assert.ok(ics.endsWith("END:VCALENDAR\r\n"));
  assert.ok(unfolded.includes("SUMMARY:Talk\\; with\\, commas\\nand lines"));
  assert.ok(unfolded.includes("DTSTART:20261001T170000Z"));
  assert.ok(unfolded.includes("DTEND:20261001T190000Z"));
  assert.ok(unfolded.includes("DTSTAMP:20260926T100000Z"));
  assert.ok(unfolded.includes(`URL:https://musenmingle.interstellarai.net/events/${ID1}`));
  assert.ok(unfolded.includes(`UID:${ID1}@musenmingle.interstellarai.net`));
  assert.equal(ics.split("BEGIN:VEVENT").length - 1, 2);
  assert.ok(lines.every((l) => l.length <= 75), "folded");
  assert.ok(lines.some((l) => l.startsWith(" x")), "continuation line");
});

test("toICS writes all-day events as London dates", () => {
  const ics = (e) => app.toICS([{ id: ID1, title: "x", ...e }], "2026-09-26T10:00:00Z", "https://m.example")
    .split("\r\n");
  const range = ics({ starts_at: "2026-09-23T23:00:00Z", ends_at: "2026-10-04T23:00:00Z", all_day: true });
  assert.ok(range.includes("DTSTART;VALUE=DATE:20260924"));
  assert.ok(range.includes("DTEND;VALUE=DATE:20261006"), "DTEND is the day after the last");
  const oneDay = ics({ starts_at: "2026-12-01T00:00:00Z", ends_at: null, all_day: true });
  assert.ok(oneDay.includes("DTSTART;VALUE=DATE:20261201"));
  assert.ok(oneDay.includes("DTEND;VALUE=DATE:20261202"));
  const legacy = ics({ starts_at: "2026-10-02T23:00:00Z", ends_at: null });
  assert.ok(legacy.includes("DTSTART;VALUE=DATE:20261003"), "saves without all_day fall back to midnight");
  const timed = ics({ starts_at: "2026-10-02T23:00:00Z", ends_at: null, all_day: false });
  assert.ok(timed.includes("DTSTART:20261002T230000Z"));
  assert.ok(!timed.some((l) => l.startsWith("DTEND")));
});

test("fromSnapshot reads saves from before all_day as the .ics does", () => {
  const card = (snapshot) => app.fromSnapshot({ id: ID1, snapshot: { title: "x", ...snapshot } });
  const legacy = card({ starts_at: "2026-10-02T23:00:00Z", ends_at: null });
  assert.equal(legacy.all_day, true);
  assert.equal(app.when(legacy.starts_at, legacy.ends_at, "2026-10-01T12:00:00Z", legacy.all_day), "Sat 3 Oct 2026, all day");
  assert.equal(card({ starts_at: "2026-10-03T17:00:00Z", ends_at: null }).all_day, false);
  assert.equal(card({ starts_at: "2026-10-02T23:00:00Z", ends_at: null, all_day: false }).all_day, false);
});

test("londonDate follows Europe/London across both DST changes", () => {
  // 23:30 UTC Sat 24 Oct 2026 is 00:30 BST on Sun 25 Oct.
  assert.equal(app.londonDate("2026-10-24T23:30:00Z"), "2026-10-25");
  // After the clocks go back (GMT), 23:30 UTC on the 25th is still the 25th.
  assert.equal(app.londonDate("2026-10-25T23:30:00Z"), "2026-10-25");
  assert.equal(app.londonDate("2027-03-27T23:30:00Z"), "2027-03-27");
  // Spring forward: 23:30 UTC Sun 28 Mar 2027 is 00:30 BST on Mon 29 Mar.
  assert.equal(app.londonDate("2027-03-28T23:30:00Z"), "2027-03-29");
});

test("placeEvents: long-running events once in the strip, markers on their first and last day", () => {
  const events = [
    // 0: a talk on 7 Oct at 18:30 BST.
    { id: ID1, title: "Talk", starts_at: "2026-10-07T17:30:00Z", ends_at: null },
    // 1: an exhibition running all month (London-midnight dates).
    { id: ID2, title: "Show", starts_at: "2026-08-31T23:00:00Z", ends_at: "2027-01-30T23:00:00Z", all_day: true },
    // 2: opens 16 Oct, closes 25 Oct (the day the clocks go back).
    { id: ID1, title: "Short run", starts_at: "2026-10-15T23:00:00Z", ends_at: "2026-10-25T00:00:00Z", all_day: true },
    // 3: a three-day festival that started in September: on 1 Oct.
    { id: ID2, title: "Festival", starts_at: "2026-09-29T23:00:00Z", ends_at: "2026-10-01T23:00:00Z", all_day: true },
    // 4: in November: not shown.
    { id: ID1, title: "Later", starts_at: "2026-11-02T19:00:00Z", ends_at: null },
    // 5: no start: ignored.
    { id: ID2, title: "Broken", starts_at: null },
  ];
  const p = app.placeEvents(events, "2026-10-01", "2026-10-31");
  assert.deepEqual(p.ongoing, [1, 2]);
  assert.deepEqual(p.days, {
    "2026-10-07": [[0, "event"]],
    "2026-10-16": [[2, "opens"]],
    "2026-10-25": [[2, "last"]],
    "2026-10-01": [[3, "event"]],
  });
  // A week view in the middle of a long run: only in the strip.
  const w = app.placeEvents(events, "2026-10-19", "2026-10-25");
  assert.deepEqual(w.ongoing, [1, 2]);
  assert.deepEqual(w.days, { "2026-10-25": [[2, "last"]] });
});

// ------------------------------------------------------------ hand-offs (#76)

test("isApple is conservative", () => {
  assert.equal(app.isApple({ platform: "iPhone" }), true);
  const SAFARI = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/17.5 Safari/605.1.15";
  const CHROME = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/129.0 Safari/537.36";
  assert.equal(app.isApple({ platform: "MacIntel", userAgent: SAFARI }), true);
  assert.equal(app.isApple({ platform: "MacIntel", userAgent: CHROME }), false);
  assert.equal(app.isApple({ platform: "MacIntel", userAgent: CHROME, maxTouchPoints: 5 }), true);
  assert.equal(app.isApple({ platform: "iPhone", userAgent: "CriOS" }), true);
  assert.equal(app.isApple({ userAgentData: { platform: "macOS" }, platform: "", userAgent: CHROME }), false);
  assert.equal(app.isApple({ platform: "Linux armv8l" }), false);
  assert.equal(app.isApple({ userAgentData: { platform: "Android" }, platform: "Linux" }), false);
  assert.equal(app.isApple({ platform: "Win32" }), false);
  assert.equal(app.isApple(undefined), false);
});

test("shareMode prefers the native sheet, then the clipboard, else none", () => {
  assert.equal(app.shareMode({ share: () => Promise.resolve(), clipboard: { writeText() {} } }), "share");
  assert.equal(app.shareMode({ clipboard: { writeText() {} } }), "copy");
  assert.equal(app.shareMode({}), null);
});

const DATA = { title: "Concrete and Clay", text: "Barbican · Sat 3 Oct", url: "https://musenmingle.interstellarai.net/events/x" };

test("shareEvent uses navigator.share with the event data", async () => {
  let got = null;
  const nav = { share: (d) => { got = d; return Promise.resolve(); } };
  assert.equal(await app.shareEvent(nav, DATA), "shared");
  assert.deepEqual(got, DATA);
});

test("shareEvent: a dismissed sheet is not an error; a failing one copies", async () => {
  const abort = Object.assign(new Error("x"), { name: "AbortError" });
  assert.equal(await app.shareEvent({ share: () => Promise.reject(abort) }, DATA), "cancelled");
  let copied = null;
  const nav = {
    share: () => Promise.reject(new Error("NotAllowedError")),
    clipboard: { writeText: (u) => { copied = u; return Promise.resolve(); } },
  };
  assert.equal(await app.shareEvent(nav, DATA), "copied");
  assert.equal(copied, DATA.url);
});

test("shareEvent falls back to copying the link, then to failing", async () => {
  let copied = null;
  const nav = { clipboard: { writeText: (u) => { copied = u; return Promise.resolve(); } } };
  assert.equal(await app.shareEvent(nav, DATA), "copied");
  assert.equal(copied, DATA.url);
  assert.equal(await app.shareEvent({ clipboard: { writeText: () => Promise.reject(new Error("denied")) } }, DATA), "failed");
  assert.equal(await app.shareEvent({}, DATA), "failed");
});

test("roundPosition keeps about 100 m of precision", () => {
  assert.equal(app.roundPosition(51.53214, -0.12449), "51.532,-0.124");
  assert.equal(app.roundPosition(51.5, -0.0001), "51.500,0.000");
  assert.equal(app.roundPosition(51.5325, -0.1), "51.533,-0.100");
});

test("transitOrigin snaps to the server's ~200 m grid", () => {
  assert.equal(app.transitOrigin(51.53212, -0.12348), "51.532,-0.123");
  assert.equal(app.transitOrigin(51.5074, -0.1278), "51.508,-0.129");
  assert.equal(app.transitOrigin(0.0004, -0.0001), "0.000,0.000");
});

test("walkEstimate matches the map's walking pace", () => {
  const w = app.walkEstimate(51.5074, -0.1278, 51.532, -0.106);
  assert.equal(Math.round(w.km * 10) / 10, 3.1);
  assert.equal(w.minutes, Math.round(((w.km * 1.3) / 5) * 60));
});

test("transitView shows transit only when there is a journey", () => {
  const ok = app.transitView({
    status: "ok",
    walk: { minutes: 34, km: 2.2 },
    transit: {
      minutes: 18,
      summary: "Overground + 5 min walk",
      provider: "TfL",
      links: [
        { label: "Plan on TfL", url: "https://tfl.gov.uk/plan-a-journey/results?x=1" },
        { label: "Bad", url: "javascript:alert(1)" },
      ],
    },
  });
  assert.equal(ok.walk, "≈ 34 min walk · 2.2 km");
  assert.equal(ok.transit, "🚇 18 min by public transport");
  assert.equal(ok.detail, "Overground + 5 min walk · via TfL");
  assert.deepEqual(
    ok.links.map((l) => l.label),
    ["Plan on TfL"]
  );
  const walkOnly = app.transitView({ status: "not_faster", walk: { minutes: 12, km: 0.9 }, transit: null });
  assert.deepEqual(walkOnly, { walk: "≈ 12 min walk · 0.9 km", transit: "", detail: "", links: [] });
  // The server didn't answer: the browser's own walking estimate.
  const fallback = app.transitView(null, { minutes: 20, km: 1.33 });
  assert.equal(fallback.walk, "≈ 20 min walk · 1.3 km");
  assert.equal(app.transitView(null, null), null);
});

test("transitBadge", () => {
  assert.equal(app.transitBadge({ transit: { minutes: 18 } }), "🚇 18 min");
  assert.equal(app.transitBadge({ status: "short_walk", transit: null }), "");
  assert.equal(app.transitBadge(null), "");
});
