// Acceptance tests for spec change #318 (web-experience: "Visitors can hide
// events they have already checked"), one per scenario, through web.js's
// exported helpers. Run with `node --test tests/js/*.test.js`.
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

const SEEN = "0414a989-8ba8-41da-a6ac-b92720727755";
const NEW = "1f3632de-af3f-4c79-9ecb-f4bc48b0821f";
const OTHER = "6b0d3c55-2a7e-4e0c-9a51-1d6a3c1f0b42";

test("Scenario: Hidden by default", () => {
  // GIVEN a visitor hid an exhibition yesterday
  const s = memoryStorage();
  assert.equal(app.storeHidden(s, app.toggleHidden(app.loadHidden(s), SEEN)), true);
  // WHEN they open the home page again (a new page load reads the store)
  const view = app.hiddenView([SEEN, NEW, OTHER], app.loadHidden(s), false);
  // THEN it is not in the results, and the control says one is hidden
  assert.deepEqual(view.out, [SEEN]);
  assert.deepEqual(view.marked, []);
  assert.equal(view.count, 1);
  assert.equal(view.text, "1 hidden on this page");
});

test("Scenario: Showing hidden events", () => {
  // GIVEN events hidden on the current page
  const s = memoryStorage();
  let ids = app.toggleHidden(app.toggleHidden([], SEEN), OTHER);
  app.storeHidden(s, ids);
  const page = [SEEN, NEW, OTHER];
  assert.equal(app.hiddenView(page, app.loadHidden(s), false).text, "2 hidden on this page");
  // WHEN the visitor uses the control to show them
  const shown = app.hiddenView(page, app.loadHidden(s), true);
  // THEN they are listed again, marked as hidden
  assert.deepEqual(shown.out, []);
  assert.deepEqual(shown.marked.sort(), [OTHER, SEEN].sort());
  assert.equal(shown.toggle, "Hide them again");
  // AND each can be unhidden
  ids = app.toggleHidden(app.loadHidden(s), SEEN);
  app.storeHidden(s, ids);
  assert.equal(app.isHidden(app.loadHidden(s), SEEN), false);
  assert.deepEqual(app.hiddenView(page, app.loadHidden(s), false).out, [OTHER]);
});

test("Scenario: Saved events stay visible", () => {
  // GIVEN a visitor saved an event and hid nothing
  const s = memoryStorage();
  const saved = app.toggle([], { id: SEEN, title: "Concrete and Clay" }, "2026-10-10T10:00:00Z");
  assert.equal(app.storeSaved(s, saved), true);
  // WHEN they browse the home page
  const view = app.hiddenView([SEEN, NEW], app.loadHidden(s), false);
  // THEN the event is still listed (saving never hides, hiding never unsaves)
  assert.deepEqual(view.out, []);
  assert.equal(view.count, 0);
  app.storeHidden(s, app.toggleHidden([], SEEN));
  assert.ok(app.isSaved(app.loadSaved(s), SEEN));
});

test("Scenario: Hides stay on the device", () => {
  // GIVEN a visitor hides events
  const s = memoryStorage();
  const calls = [];
  const saved = globalThis.fetch;
  globalThis.fetch = (...a) => {
    calls.push(a);
    throw new Error("no requests expected");
  };
  try {
    app.storeHidden(s, app.toggleHidden(app.toggleHidden([], SEEN), NEW));
    app.hiddenView([SEEN, NEW, OTHER], app.loadHidden(s), false);
  } finally {
    globalThis.fetch = saved;
  }
  // WHEN the requests are inspected THEN none were made; the hides are only
  // in this browser's storage, under their own key
  assert.equal(calls.length, 0);
  assert.equal(app.HIDDEN_KEY, "musenmingle.hidden.v1");
  assert.deepEqual([...s.data.keys()], ["musenmingle.hidden.v1"]);
});
