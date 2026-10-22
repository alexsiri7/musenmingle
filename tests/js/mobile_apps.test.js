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

// Splits JS-like source (JS, TS, Java, Kotlin, Swift) into code, literal
// (string, template, regex) and comment spans. A `/` starts a regex only
// where a value is expected (after an operator, punctuation or a keyword that
// takes an operand), so `/^https?:\/\//` is not a comment.
const VALUE_EXPECTED =
  /(^|[(,=:[!&|?{};+\-*%<>~^]|\b(?:return|typeof|case|else|delete|void|throw|do|instanceof|new|yield|in|of|extends|await|default))\s*$/;

function literalEnd(src, i) {
  const close = src[i] === "/" ? "/" : src[i];
  let inClass = false;
  let j = i + 1;
  for (; j < src.length && (inClass || src[j] !== close); j++) {
    if (src[j] === "\\") j++;
    else if (close === "/" && src[j] === "\n") break;
    else if (close === "/") inClass = src[j] === "[" || (inClass && src[j] !== "]");
  }
  return j + 1;
}

function spans(src) {
  const out = [];
  let start = 0;
  const cut = (kind, end) => {
    end = Math.min(end, src.length);
    if (end > start) out.push({ kind, text: src.slice(start, end) });
    start = end;
  };
  for (let i = 0; i < src.length; i = Math.max(i + 1, start)) {
    let kind;
    let end;
    if (src.startsWith("//", i)) {
      [kind, end] = ["comment", src.indexOf("\n", i)];
    } else if (src.startsWith("/*", i)) {
      [kind, end] = ["comment", src.indexOf("*/", i + 2) + 2];
    } else if ("\"'`".includes(src[i]) || (src[i] === "/" && VALUE_EXPECTED.test(src.slice(0, i)))) {
      [kind, end] = ["literal", literalEnd(src, i)];
    } else {
      continue;
    }
    cut("code", i);
    cut(kind, end <= i ? src.length : end);
  }
  cut("code", src.length);
  return out;
}

const blank = (text) => text.replace(/[^\n]/g, " ");

// Source text with its comments blanked (offsets kept), so a commented-out
// line never counts.
const code = (src) => spans(src).map((s) => (s.kind === "comment" ? blank(s.text) : s.text)).join("");

// Files under `dir` ending in one of `exts`, skipping dependencies and build output.
function sources(dir, exts) {
  return fs
    .readdirSync(path.join(ROOT, dir), { recursive: true })
    .filter((f) => exts.some((x) => f.endsWith(x)))
    .filter((f) => !f.split(path.sep).some((d) => ["node_modules", "build", ".gradle", "Pods"].includes(d)))
    .map((f) => read(path.join(dir, f)));
}

// The `{ … }` block starting at the first `{` at or after `from`, matched
// past literals and comments.
function blockAt(src, from) {
  const bare = spans(src).map((s) => (s.kind === "code" ? s.text : blank(s.text))).join("");
  const start = bare.indexOf("{", from);
  let depth = 0;
  for (let i = start; i < bare.length; i++) {
    if (bare[i] === "{") depth++;
    else if (bare[i] === "}" && --depth === 0) return src.slice(start, i + 1);
  }
  throw new Error(`unbalanced block at ${from}`);
}

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

test("code() keeps a same-line regex after any keyword that takes an operand", () => {
  const keywords = ["return", "typeof", "case", "else", "delete", "void", "throw", "do", "instanceof", "new",
    "yield", "in", "of", "extends", "await", "default"];
  for (const kw of keywords) {
    const src = `${kw} /^https?:\\/\\//i.test(u); App.addListener("backButton", f);`;
    assert.ok(code(src).includes("backButton"), `${kw} hid the listener`);
  }
  assert.ok(!code("a = b / c; // backButton").includes("backButton"));
});

test("Scenario: Android install", () => {
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

test("Scenario: iPhone install", () => {
  // WHEN a person searches the App Store for Muse & Mingle on an iPhone
  // THEN they can install the app and open it from their home screen
  // Operator check (#365): found by searching the App Store, installs, opens
  // from the home screen with the Muse & Mingle icon.
  assert.equal(config().appName, "Muse & Mingle");
  assert.equal(plistString(infoPlist(), "CFBundleDisplayName"), "Muse &amp; Mingle");
  assert.ok(fs.existsSync(path.join(ROOT, "mobile/ios/App/App/Assets.xcassets/AppIcon.appiconset")));
});

test("Scenario: Browsing in the app", () => {
  // WHEN a person opens the app and filters by today and free
  // THEN they see the same events the website shows for today and free
  // The app shows the live site, so the filter runs the website's query on
  // the website's data (tested in Rust, tests/web_pages.rs).
  const { server = {} } = config();
  assert.equal(server.url, SITE);
  assert.notEqual(server.cleartext, true);
});

test("Scenario: New website feature", () => {
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

test("Scenario: No connection", () => {
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

test("The app shows the site's own HTTP error pages, not the offline page", () => {
  // Capacitor's BridgeWebViewClient loads errorPath for main-frame HTTP errors
  // too; MainActivity installs a subclass that leaves a 404 or 400 to the site.
  const java = sources("mobile/android/app/src/main/java", [".java"]).map(code).join("\n");
  const cls = java.match(/class\s+(\w+)\s+extends\s+BridgeWebViewClient\b/);
  assert.ok(cls, "no BridgeWebViewClient subclass");
  const body = blockAt(java, cls.index);
  const override = body.indexOf("void onReceivedHttpError(");
  assert.ok(override >= 0, "onReceivedHttpError is not overridden");
  assert.doesNotMatch(blockAt(body, override), /onReceivedHttpError|loadUrl|getErrorUrl/);
  const onCreate = java.indexOf("void onCreate(");
  assert.ok(onCreate >= 0, "MainActivity.onCreate not found");
  assert.match(blockAt(java, onCreate), new RegExp(`\\.setWebViewClient\\(\\s*new\\s+${cls[1]}\\(`));
});

test("The iPhone app opens calendar files in Safari, not the offline page", () => {
  // Capacitor's iOS delegate loads errorPath for every failed load. WebKit
  // can't show the site's .ics files, so the app's own delegate sends them to
  // Safari and keeps that interruption (and cancelled loads) from Capacitor.
  const swift = sources("mobile/ios/App/App", [".swift"]).map(code).join("\n");
  const cls = swift.match(/class\s+(\w+)\s*:\s*NSObject\s*,\s*WKNavigationDelegate\b/);
  assert.ok(cls, "no WKNavigationDelegate of the app's own");
  assert.match(swift, new RegExp(`=\\s*${cls[1]}\\(capacitor:`));
  assert.match(swift, /webView\.navigationDelegate\s*=/);
  const body = blockAt(swift, cls.index);
  const response = body.indexOf("decidePolicyFor navigationResponse");
  assert.ok(response >= 0, "navigation responses are not decided");
  assert.match(blockAt(body, response), /canShowMIMEType[\s\S]*UIApplication\.shared\.open\([\s\S]*\(\.cancel\)/);
  for (const fail of ["didFail navigation", "didFailProvisionalNavigation navigation"]) {
    const at = body.indexOf(fail);
    assert.ok(at >= 0, `${fail} is not overridden`);
    assert.match(blockAt(body, at), /if\s+!Self\.isInterruption\(error\)/);
  }
  assert.match(body, /NSURLErrorCancelled/);
  assert.match(body, /"WebKitErrorDomain"\s*&&\s*error\.code\s*==\s*102/);
});

test("Scenario: Back gesture", () => {
  // WHEN a person opens an event and uses the back gesture
  // THEN they return to the list they came from
  // Operator check on both phones: open an event, go back, the same list at
  // the same scroll position.
  // Android: with @capacitor/app and no backButton listener, back goes back
  // in the WebView's history. The site runs inside the app's bridge, so its
  // scripts must not take back over either.
  assert.ok(shellDeps().includes("@capacitor/app"));
  const scripts = [...sources("mobile", [".js", ".mjs", ".ts", ".java", ".kt"]), read("src/web.js"), read("src/map.mjs")];
  for (const src of scripts.map(code)) {
    assert.doesNotMatch(src, /addListener\(\s*['"]backButton['"]/);
    assert.doesNotMatch(src, /\bonBackPressed\b|OnBackPressedCallback/);
  }
  // iOS: Capacitor has no setting for the swipe; the app's own Swift turns it on.
  const swift = sources("mobile/ios/App/App", [".swift"]).map(code);
  assert.ok(swift.some((s) => /allowsBackForwardNavigationGestures\s*=\s*true/.test(s)));
});

test("Scenario: Going to the venue", () => {
  // WHEN a person taps the link to an event's source page
  // THEN the venue's page opens in the phone's browser
  // Capacitor opens every host it is not allowed to navigate to in the
  // system browser, so only our own host may be allowed.
  // Operator check: tapping "See it on …" on a device opens the phone's browser.
  const allowed = config().server.allowNavigation || [];
  for (const host of allowed) assert.equal(host, new URL(SITE).host);
});

test("Scenario: Saving in the app", () => {
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
  // Nor does either phone copy the WebView's storage to a backup.
  assert.match(androidManifest(), /android:allowBackup="false"/);
  const swift = sources("mobile/ios/App/App", [".swift"]).map(code);
  assert.ok(swift.some((s) => /\.isExcludedFromBackup\s*=\s*true/.test(s)));
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

test("Scenario: Location refused", async () => {
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
  const src = code(read("src/map.mjs"));
  assert.equal(src.split("locate(navigator.geolocation)").length, 2);
  const listener = src.indexOf('locateBtn.addEventListener("click"');
  assert.notEqual(listener, -1);
  assert.ok(blockAt(src, listener).includes("locate(navigator.geolocation)"));
  // The app may ask while in use, never in the background.
  const manifest = androidManifest();
  assert.match(manifest, /android\.permission\.ACCESS_(COARSE|FINE)_LOCATION/);
  assert.ok(!manifest.includes("android.permission.ACCESS_BACKGROUND_LOCATION"));
  const plist = infoPlist();
  assert.ok(plistString(plist, "NSLocationWhenInUseUsageDescription"));
  assert.ok(!plist.includes("<key>NSLocationAlwaysAndWhenInUseUsageDescription</key>"));
});
