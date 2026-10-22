// Acceptance tests for spec change #361 (mobile-apps: Muse & Mingle on
// Android and iPhone), one per scenario of openspec/specs/mobile-apps/spec.md,
// through the app shell's checked-in surface (mobile/, the Capacitor project
// of #363/#364) and the website code it wraps. What only a device or a store
// can show is an operator check, noted per test. Run with
// `node --test tests/js/*.test.js`.
"use strict";
const test = require("node:test");
const assert = require("node:assert/strict");
const fs = require("node:fs");
const path = require("node:path");
const app = require("../../src/web.js");

const ROOT = path.join(__dirname, "..", "..");
// DEFAULT_SITE_HOST in src/web.rs.
const SITE = "https://musenmingle.interstellarai.net";

const ANDROID = "until #363 builds the Android app";
const IOS = "until #364 builds the iPhone app";
const STORES = "until #365 publishes both apps";

const read = (rel) => fs.readFileSync(path.join(ROOT, rel), "utf8");
const config = () => JSON.parse(read("mobile/capacitor.config.json"));
const androidManifest = () => read("mobile/android/app/src/main/AndroidManifest.xml");
const androidStrings = () => read("mobile/android/app/src/main/res/values/strings.xml");
const infoPlist = () => read("mobile/ios/App/App/Info.plist");

function plistString(xml, key) {
  const m = xml.match(new RegExp(`<key>${key}</key>\\s*<string>([^<]*)</string>`));
  return m ? m[1] : null;
}

function shellDeps() {
  const pkg = JSON.parse(read("mobile/package.json"));
  return Object.keys({ ...pkg.dependencies, ...pkg.devDependencies });
}

const webDir = () => path.join("mobile", config().webDir);
const bundledFiles = () =>
  fs
    .readdirSync(path.join(ROOT, webDir()), { recursive: true, withFileTypes: true })
    .filter((e) => e.isFile())
    .map((e) => e.name);

const TRACKERS =
  /analytics|firebase|crashlytics|sentry|segment|amplitude|mixpanel|admob|ads|facebook|appsflyer|adjust|branch|onesignal|bugsnag|datadog|posthog/i;

function memoryStorage() {
  const data = new Map();
  return {
    getItem: (k) => (data.has(k) ? data.get(k) : null),
    setItem: (k, v) => data.set(k, String(v)),
    removeItem: (k) => data.delete(k),
    data,
  };
}

test("Scenario: Android install", { todo: ANDROID }, () => {
  // WHEN a person searches Google Play for Muse & Mingle on an Android phone
  // THEN they can install the app and open it from their home screen
  // Operator check (#365): found by searching Google Play, installs, opens
  // from the home screen with the Muse & Mingle icon.
  const c = config();
  assert.equal(c.appName, "Muse & Mingle");
  assert.match(c.appId, /^[a-z][a-z0-9_]*(\.[a-z][a-z0-9_]*)+$/);
  assert.ok(androidStrings().includes('<string name="app_name">Muse &amp; Mingle</string>'));
  assert.ok(fs.existsSync(path.join(ROOT, "mobile/android/app/src/main/res/mipmap-xxxhdpi/ic_launcher.png")));
});

test("Scenario: iPhone install", { todo: IOS }, () => {
  // WHEN a person searches the App Store for Muse & Mingle on an iPhone
  // THEN they can install the app and open it from their home screen
  // Operator check (#365): found by searching the App Store, installs, opens
  // from the home screen with the Muse & Mingle icon.
  assert.equal(config().appName, "Muse & Mingle");
  assert.equal(plistString(infoPlist(), "CFBundleDisplayName"), "Muse &amp; Mingle");
  assert.ok(fs.existsSync(path.join(ROOT, "mobile/ios/App/App/Assets.xcassets/AppIcon.appiconset")));
});

test("Scenario: Browsing in the app", { todo: ANDROID }, () => {
  // WHEN a person opens the app and filters by today and free
  // THEN they see the same events the website shows for today and free
  // The app shows the live site, so the filter runs the website's query on
  // the website's data (tested in Rust, tests/web_pages.rs).
  const { server = {} } = config();
  assert.equal(server.url, SITE);
  assert.notEqual(server.cleartext, true);
});

test("Scenario: New website feature", { todo: ANDROID }, () => {
  // WHEN a feature ships on the website
  // THEN it is available in the apps without waiting for a separate app redesign
  assert.equal(config().server.url, SITE);
  // Features come from the server: the bundle holds the offline page and its
  // assets, never a copy of the site's scripts.
  for (const name of bundledFiles()) {
    assert.match(name, /\.(html|css|svg|png|woff2)$/, name);
    assert.ok(!["web.js", "map.mjs"].includes(name), name);
  }
});

test("Scenario: No connection", { todo: ANDROID }, () => {
  // WHEN a person opens the app with no internet connection
  // THEN the app shows a Muse & Mingle message saying it is offline, not a
  // browser error page
  // Operator check: in airplane mode on a device the app shows this page, not
  // the WebView's error.
  const { errorPath } = config().server;
  assert.equal(typeof errorPath, "string");
  assert.ok(errorPath.length > 0);
  const page = read(path.join(webDir(), errorPath));
  assert.ok(page.includes("Muse &amp; Mingle") || page.includes("Muse & Mingle"));
  assert.match(page, /offline|no (internet )?connection/i);
  // It is shown exactly when the network is down: no remote assets.
  assert.doesNotMatch(page, /(src|href)\s*=\s*["']?(https?:)?\/\//i);
});

test("Scenario: Back gesture", { todo: "until #363 and #364 handle back" }, () => {
  // WHEN a person opens an event and uses the back gesture
  // THEN they return to the list they came from
  // Operator check on both phones: open an event, go back, the same list at
  // the same scroll position.
  // Android: with @capacitor/app and no backButton listener, back goes back
  // in the WebView's history.
  assert.ok(shellDeps().includes("@capacitor/app"));
  // iOS: Capacitor has no setting for the swipe; the app's own Swift turns it on.
  const swift = fs
    .readdirSync(path.join(ROOT, "mobile/ios/App/App"), { recursive: true })
    .filter((f) => f.endsWith(".swift"))
    .map((f) => read(path.join("mobile/ios/App/App", f)));
  assert.ok(swift.some((s) => /allowsBackForwardNavigationGestures\s*=\s*true/.test(s)));
});

test("Scenario: Going to the venue", { todo: ANDROID }, () => {
  // WHEN a person taps the link to an event's source page
  // THEN the venue's page opens in the phone's browser
  // Capacitor opens every host it is not allowed to navigate to in the
  // system browser, so only our own host may be allowed.
  // Operator check: tapping "See it on …" on a device opens the phone's browser.
  const allowed = config().server.allowNavigation || [];
  for (const host of allowed) assert.equal(host, new URL(SITE).host);
});

test("Scenario: Saving in the app", { todo: ANDROID }, () => {
  // WHEN a person saves an event in the app
  // THEN the save is kept on the phone and nothing about it is sent to Muse &
  // Mingle or anyone else
  // The website keeps saves in the browser's storage, which the app inherits.
  const s = memoryStorage();
  const id = "0414a989-8ba8-41da-a6ac-b92720727755";
  app.storeSaved(s, app.toggle(app.loadSaved(s), { id, title: "Life drawing" }, "2026-10-22T10:00:00Z"));
  assert.ok(app.isSaved(app.loadSaved(s), id));
  // The shell adds nothing that could send it: Capacitor's own packages only,
  // so any new SDK has to be argued for here.
  for (const dep of shellDeps()) {
    assert.doesNotMatch(dep, TRACKERS);
    assert.ok(dep.startsWith("@capacitor/") || dep === "typescript", dep);
  }
});

test("Scenario: Store privacy label", { todo: STORES }, () => {
  // WHEN a person reads the app's privacy details in either store
  // THEN they state that no data is collected for tracking
  // Operator check (#365): Google Play's Data safety and the App Store's
  // privacy details both say no data is collected for tracking, and the
  // privacy policy links to /about.
  for (const dep of shellDeps()) assert.doesNotMatch(dep, TRACKERS);
  assert.ok(!androidManifest().includes("com.google.android.gms.permission.AD_ID"));
  assert.ok(!infoPlist().includes("<key>NSUserTrackingUsageDescription</key>"));
  assert.match(read("mobile/ios/App/App/PrivacyInfo.xcprivacy"), /<key>NSPrivacyTracking<\/key>\s*<false\/>/);
});

test("Scenario: Location refused", { todo: "until #363 and #364 ask for location" }, async () => {
  // WHEN a person refuses location permission
  // THEN they can still browse, filter and save events, and "Near me"
  // explains that it needs location
  // Operator check on both phones: refuse the prompt; browsing, filters and
  // saving still work, and Near me shows the message.
  // The website asks only when "use my location" is clicked, and explains a refusal.
  const m = await import("../../src/map.mjs");
  const refuse = { getCurrentPosition: (_ok, fail) => fail({ code: 1 }) };
  await assert.rejects(m.locate(refuse), (e) => e.reason === "denied");
  assert.match(m.locateMessage("denied", "Central London"), /^Location permission was not given/);
  const src = read("src/map.mjs");
  const ask = src.indexOf("locate(navigator.geolocation)");
  assert.equal(src.lastIndexOf("locate(navigator.geolocation)"), ask);
  const click = src.lastIndexOf('addEventListener("click", async () => {', ask);
  assert.ok(click !== -1 && click < ask);
  // The app may ask while in use, never in the background.
  const manifest = androidManifest();
  assert.match(manifest, /android\.permission\.ACCESS_(COARSE|FINE)_LOCATION/);
  assert.ok(!manifest.includes("android.permission.ACCESS_BACKGROUND_LOCATION"));
  const plist = infoPlist();
  assert.ok(plistString(plist, "NSLocationWhenInUseUsageDescription"));
  assert.ok(!plist.includes("<key>NSLocationAlwaysAndWhenInUseUsageDescription</key>"));
});
