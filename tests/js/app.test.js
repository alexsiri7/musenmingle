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
    starts_at: "2026-10-01T18:00:00Z", ends_at: null });
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
