// Acceptance tests for spec change #322 (web-experience: swipe left to hide,
// right to save), through web.js's exported helpers. Run with
// `node --test tests/js/*.test.js`.
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

const LEFT = "0414a989-8ba8-41da-a6ac-b92720727755";
const RIGHT = "1f3632de-af3f-4c79-9ecb-f4bc48b0821f";
const WIDTH = 360; // a phone-width card, in px

test("Scenario: Swipe to sort through events", () => {
  // GIVEN a visitor on a phone browsing the home page
  const s = memoryStorage();
  // WHEN they swipe one event card left and another right
  assert.equal(app.swipeAxis(-60, 8), "x");
  assert.equal(app.swipeAction(-0.6 * WIDTH, WIDTH, 400), "hide");
  assert.equal(app.swipeAction(0.6 * WIDTH, WIDTH, 400), "save");
  app.storeHidden(s, app.toggleHidden(app.loadHidden(s), LEFT));
  app.storeSaved(s, app.toggle(app.loadSaved(s), { id: RIGHT, title: "Life drawing" }, "2026-10-10T10:00:00Z"));
  // THEN the first is hidden and the second is saved
  assert.deepEqual(app.hiddenView([LEFT, RIGHT], app.loadHidden(s), false).out, [LEFT]);
  assert.ok(app.isSaved(app.loadSaved(s), RIGHT));
  assert.ok(!app.isHidden(app.loadHidden(s), RIGHT));
  // AND each swipe can be undone
  app.storeHidden(s, app.toggleHidden(app.loadHidden(s), LEFT));
  app.storeSaved(s, app.toggle(app.loadSaved(s), { id: RIGHT, title: "Life drawing" }, "2026-10-10T10:01:00Z"));
  assert.deepEqual(app.loadHidden(s), []);
  assert.deepEqual(app.loadSaved(s), []);
  // Scrolling and small drags are not swipes.
  assert.equal(app.swipeAxis(6, 70), "y");
  assert.equal(app.swipeAxis(3, 2), null);
  assert.equal(app.swipeAction(-40, WIDTH, 600), null);
});
