// Muse & Mingle: "Saved" events, stored only in this browser (localStorage).
// Served from /static/app.js; no third-party code, no inline scripts.
// Progressive enhancement: pages render and work without this file; the
// save buttons are rendered `hidden` and only shown from here.
//
// Pure helpers are exported for `node --test tests/js/` (see the bottom).
(function (root) {
  "use strict";

  var STORAGE_KEY = "musenmingle.saved.v1";
  // Keys used by earlier versions of this script (the site was "LetsArt" and
  // then "Thaleia" before it was Muse & Mingle); moved to STORAGE_KEY once.
  var LEGACY_STORAGE_KEYS = ["letsart.saved.v1"];
  var MAX_ITEMS = 500;
  var IDS_PER_REQUEST = 100; // GET /v1/events?ids= cap (listing::MAX_IDS)
  var TZ = "Europe/London";

  // ------------------------------------------------------------ storage

  function getStorage() {
    try {
      return root.localStorage || null;
    } catch (e) {
      return null; // blocked (privacy settings, sandboxed frame, ...)
    }
  }

  function isItem(x) {
    return (
      x &&
      typeof x.id === "string" &&
      /^[0-9a-f-]{36}$/i.test(x.id) &&
      typeof x.saved_at === "string" &&
      x.snapshot &&
      typeof x.snapshot.title === "string"
    );
  }

  /**
   * Moves saved items from a legacy key to STORAGE_KEY: when STORAGE_KEY is
   * absent and a legacy key is present, its value is copied over and the
   * legacy key removed. Never overwrites existing data under STORAGE_KEY
   * (legacy keys are then left alone). true if anything was moved.
   */
  function migrateStorage(storage) {
    if (!storage) return false;
    try {
      if (storage.getItem(STORAGE_KEY) !== null) return false;
      for (var i = 0; i < LEGACY_STORAGE_KEYS.length; i++) {
        var old = storage.getItem(LEGACY_STORAGE_KEYS[i]);
        if (old === null) continue;
        storage.setItem(STORAGE_KEY, old);
        storage.removeItem(LEGACY_STORAGE_KEYS[i]);
        return true;
      }
    } catch (e) {
      // Blocked or over quota: keep the legacy key; try again next load.
    }
    return false;
  }

  /** Saved items (newest first); [] when storage is missing or corrupt. */
  function loadSaved(storage) {
    if (!storage) return [];
    try {
      var raw = storage.getItem(STORAGE_KEY);
      if (!raw) return [];
      var data = JSON.parse(raw);
      var items = data && Array.isArray(data.items) ? data.items.filter(isItem) : [];
      return items.sort(function (a, b) {
        return a.saved_at < b.saved_at ? 1 : a.saved_at > b.saved_at ? -1 : 0;
      });
    } catch (e) {
      return [];
    }
  }

  /** true if written (false in private mode / over quota). */
  function storeSaved(storage, items) {
    if (!storage) return false;
    try {
      storage.setItem(STORAGE_KEY, JSON.stringify({ items: items.slice(0, MAX_ITEMS) }));
      return true;
    } catch (e) {
      return false;
    }
  }

  function isSaved(items, id) {
    return items.some(function (i) {
      return i.id === id;
    });
  }

  /** Items with `event` added (at the front) or removed. */
  function toggle(items, event, nowIso) {
    if (isSaved(items, event.id)) {
      return items.filter(function (i) {
        return i.id !== event.id;
      });
    }
    var item = {
      id: event.id,
      saved_at: nowIso,
      snapshot: {
        title: String(event.title || ""),
        venue: event.venue || null,
        starts_at: event.starts_at || null,
        ends_at: event.ends_at || null,
        all_day: event.all_day === true,
      },
    };
    if (Array.isArray(event.sessions) && event.sessions.length > 1) {
      item.snapshot.sessions = event.sessions;
    }
    return [item].concat(items);
  }

  // ------------------------------------------------------------ formatting

  var dateParts = null;
  function parts(iso) {
    if (!dateParts) {
      dateParts = new Intl.DateTimeFormat("en-GB", {
        timeZone: TZ,
        weekday: "short",
        day: "numeric",
        month: "numeric",
        year: "numeric",
        hour: "2-digit",
        minute: "2-digit",
        hourCycle: "h23",
      });
    }
    var out = {};
    dateParts.formatToParts(new Date(iso)).forEach(function (p) {
      out[p.type] = p.value;
    });
    return out;
  }

  // Fixed names: Intl's en-GB short month for September is "Sept", the
  // server (chrono) prints "Sep".
  var MONTHS = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];

  function fmtDate(iso) {
    var p = parts(iso);
    return p.weekday + " " + Number(p.day) + " " + MONTHS[Number(p.month) - 1] + " " + p.year;
  }

  /** "04": the London day of the month (the placeholder's numeral). */
  function dayOfMonth(iso) {
    var d = String(Number(parts(iso).day));
    return d.length < 2 ? "0" + d : d;
  }

  function fmtTime(iso) {
    var p = parts(iso);
    return p.hour + ":" + p.minute;
  }

  function isMidnight(iso) {
    return fmtTime(iso) === "00:00";
  }

  function fmtDateTime(iso) {
    return isMidnight(iso) ? fmtDate(iso) : fmtDate(iso) + ", " + fmtTime(iso);
  }

  /** Same wording as the server (web::when). */
  function when(startsIso, endsIso, nowIso, allDay) {
    if (!startsIso) return "";
    var start = new Date(startsIso);
    var end = endsIso ? new Date(endsIso) : null;
    if (end && end > start && fmtDate(endsIso) !== fmtDate(startsIso)) {
      return start <= new Date(nowIso)
        ? "Until " + fmtDate(endsIso)
        : fmtDate(startsIso) + " – " + fmtDate(endsIso);
    }
    if (allDay) return fmtDate(startsIso) + ", all day";
    if (end && end > start && !isMidnight(startsIso)) {
      return fmtDateTime(startsIso) + "–" + fmtTime(endsIso);
    }
    return fmtDateTime(startsIso);
  }

  // ------------------------------------------------------------ sessions (#207)

  /** A session's end: its ends_at, else its start + 60 minutes (the server's LIVE_GRACE_MINUTES). */
  function sessionEnd(s) {
    var start = Date.parse(s.starts_at);
    var end = s.ends_at ? Date.parse(s.ends_at) : NaN;
    return end > start ? end : start + 60 * 60 * 1000;
  }

  /** The first session not over at `nowIso` (model::next_session); null if none. */
  function nextSession(sessions, nowIso) {
    if (!Array.isArray(sessions) || sessions.length < 2) return null;
    var now = Date.parse(nowIso);
    for (var i = 0; i < sessions.length; i++) {
      var s = sessions[i];
      if (s && s.starts_at && !Number.isNaN(Date.parse(s.starts_at)) && sessionEnd(s) > now) return s;
    }
    return null;
  }

  /** [`when`] for an event: "Next session: Tue 20 Oct, 16:30 · 6 sessions" (web::event_when). */
  function eventWhen(e, nowIso) {
    var s = nextSession(e.sessions, nowIso);
    if (!s) return when(e.starts_at, e.ends_at, nowIso, e.all_day);
    var p = parts(s.starts_at);
    var day = p.weekday + " " + Number(p.day) + " " + MONTHS[Number(p.month) - 1];
    if (!isMidnight(s.starts_at)) day += ", " + fmtTime(s.starts_at);
    return "Next session: " + day + " · " + e.sessions.length + " sessions";
  }

  function money(amount, currency) {
    var n = Number(amount);
    var s = Number.isInteger(n) ? String(n) : n.toFixed(2);
    if (!currency || currency === "GBP") return "£" + s;
    if (currency === "EUR") return "€" + s;
    if (currency === "USD") return "$" + s;
    return s + " " + currency;
  }

  /** Same wording as the server (web::price); null when unknown. */
  function price(e) {
    if (e.is_free) return "Free";
    var min = e.price_min, max = e.price_max, c = e.currency;
    if (min != null && max != null && Number(min) !== Number(max)) {
      return money(min, c) + "–" + money(max, c);
    }
    if (min != null) return money(min, c);
    if (max != null) return "Up to " + money(max, c);
    return null;
  }

  function titleCase(s) {
    return s ? s.charAt(0).toUpperCase() + s.slice(1) : "";
  }

  function safeLink(u) {
    return typeof u === "string" && /^https?:\/\//i.test(u) ? u : null;
  }

  // ------------------------------------------------------------ iCalendar

  function icsEscape(s) {
    return String(s)
      .replace(/\\/g, "\\\\")
      .replace(/;/g, "\\;")
      .replace(/,/g, "\\,")
      .replace(/\r?\n/g, "\\n");
  }

  function icsTime(iso) {
    return new Date(iso).toISOString().replace(/[-:]/g, "").replace(/\.\d{3}/, "");
  }

  /** Saves from before `all_day` existed: London midnight means date-only. */
  function isAllDay(e) {
    if (typeof e.all_day === "boolean") return e.all_day;
    return isMidnight(e.starts_at) && (!e.ends_at || isMidnight(e.ends_at));
  }

  /** A saved item as the event fields the card needs, from its snapshot. */
  function fromSnapshot(item) {
    return {
      id: item.id,
      title: item.snapshot.title,
      venue_name: item.snapshot.venue,
      starts_at: item.snapshot.starts_at,
      ends_at: item.snapshot.ends_at,
      all_day: isAllDay(item.snapshot),
      sessions: item.snapshot.sessions || null,
    };
  }

  /** The London date of `iso` plus `addDays`, as an iCalendar DATE. */
  function icsDate(iso, addDays) {
    var p = parts(iso);
    var d = new Date(Date.UTC(Number(p.year), Number(p.month) - 1, Number(p.day) + addDays));
    return d.toISOString().slice(0, 10).replace(/-/g, "");
  }

  /** Fold lines longer than 75 characters (RFC 5545 3.1). */
  function fold(line) {
    var out = [];
    while (line.length > 75) {
      out.push(line.slice(0, 75));
      line = " " + line.slice(75);
    }
    out.push(line);
    return out.join("\r\n");
  }

  /**
   * A VCALENDAR for `events` ({id, title, venue, starts_at, ends_at, all_day,
   * url}). All-day events become DATE entries, whose DTEND is exclusive.
   * `origin` builds each event's Muse & Mingle link.
   */
  function toICS(events, nowIso, origin) {
    var lines = [
      "BEGIN:VCALENDAR",
      "VERSION:2.0",
      "PRODID:-//Muse & Mingle//Saved events//EN",
      "CALSCALE:GREGORIAN",
    ];
    // A multi-session event (#207): one VEVENT per session, with the
    // server's UIDs (share::ics_with_sessions); an all-day session starts
    // at London midnight.
    var entries = [];
    events.forEach(function (e) {
      if (Array.isArray(e.sessions) && e.sessions.length > 1) {
        e.sessions.forEach(function (s) {
          var untimed = isMidnight(s.starts_at);
          entries.push({
            uid: e.id + "-" + icsTime(s.starts_at),
            e: e,
            starts_at: s.starts_at,
            ends_at: untimed ? null : s.ends_at || null,
            all_day: untimed,
          });
        });
      } else {
        entries.push({ uid: e.id, e: e, starts_at: e.starts_at, ends_at: e.ends_at, all_day: isAllDay(e) });
      }
    });
    entries.forEach(function (x) {
      var e = x.e;
      if (!x.starts_at) return;
      lines.push("BEGIN:VEVENT");
      lines.push("UID:" + x.uid + "@musenmingle.interstellarai.net");
      lines.push("DTSTAMP:" + icsTime(nowIso));
      if (x.all_day) {
        lines.push("DTSTART;VALUE=DATE:" + icsDate(x.starts_at, 0));
        lines.push("DTEND;VALUE=DATE:" + icsDate(x.ends_at || x.starts_at, 1));
      } else {
        lines.push("DTSTART:" + icsTime(x.starts_at));
        if (x.ends_at) lines.push("DTEND:" + icsTime(x.ends_at));
      }
      lines.push("SUMMARY:" + icsEscape(e.title));
      if (e.venue) lines.push("LOCATION:" + icsEscape(e.venue));
      lines.push("URL:" + origin + "/events/" + e.id);
      lines.push("END:VEVENT");
    });
    lines.push("END:VCALENDAR");
    return lines.map(fold).join("\r\n") + "\r\n";
  }

  // ------------------------------------------------------------ calendar

  // Same rules as src/calendar.rs: an event running on this many London
  // days or more is long-running (the "ongoing" strip plus "Opens" / "Last
  // day" markers); any other event sits on its first visible day.
  var LONG_RUN_MIN_DAYS = 4;

  function pad2(n) {
    var s = String(Number(n));
    return s.length < 2 ? "0" + s : s;
  }

  /** "2026-10-25": the London date of an instant. */
  function londonDate(iso) {
    var p = parts(iso);
    return p.year + "-" + pad2(p.month) + "-" + pad2(p.day);
  }

  /** Whole days from London date `a` to `b` ("YYYY-MM-DD"). */
  function daysBetween(a, b) {
    var ua = Date.UTC(+a.slice(0, 4), +a.slice(5, 7) - 1, +a.slice(8, 10));
    var ub = Date.UTC(+b.slice(0, 4), +b.slice(5, 7) - 1, +b.slice(8, 10));
    return Math.round((ub - ua) / 86400000);
  }

  /** [first, last] London dates an event runs on (an end before its start is ignored). */
  function spanOf(e) {
    var first = londonDate(e.starts_at);
    var last = e.ends_at ? londonDate(e.ends_at) : first;
    return [first, last > first ? last : first];
  }

  function isLongRunning(first, last) {
    return daysBetween(first, last) + 1 >= LONG_RUN_MIN_DAYS;
  }

  /**
   * Where events go in a calendar showing London dates first..last:
   * { ongoing: [index], days: { "YYYY-MM-DD": [[index, "event"|"opens"|"last"]] } }.
   * Pass events sorted by start.
   */
  function placeEvents(events, first, last) {
    var out = { ongoing: [], days: {} };
    function add(day, i, mark) {
      (out.days[day] = out.days[day] || []).push([i, mark]);
    }
    events.forEach(function (e, i) {
      if (!e.starts_at) return;
      // A multi-session event (#207) sits on each of its session days.
      if (Array.isArray(e.sessions) && e.sessions.length > 1) {
        e.sessions.forEach(function (x) {
          var d = x && x.starts_at ? londonDate(x.starts_at) : null;
          if (d && d >= first && d <= last) add(d, i, "event");
        });
        return;
      }
      var s = spanOf(e);
      if (s[1] < first || s[0] > last) return;
      if (isLongRunning(s[0], s[1])) {
        out.ongoing.push(i);
        if (s[0] >= first) add(s[0], i, "opens");
        if (s[1] <= last) add(s[1], i, "last");
      } else {
        add(s[0] < first ? first : s[0], i, "event");
      }
    });
    return out;
  }

  // ------------------------------------------------------------ hand-offs

  /**
   * Apple device whose maps links should open Apple Maps: iPhone/iPad
   * (any browser: they hand off to the Maps app), or Safari on a Mac.
   * Conservative: userAgentData's platform when present, else
   * navigator.platform ("iPhone", "iPad", "MacIntel"; iPadOS reports
   * MacIntel with touch points).
   */
  function isApple(nav) {
    if (!nav) return false;
    var p = (nav.userAgentData && nav.userAgentData.platform) || nav.platform || "";
    if (/^(iPhone|iPad|iPod)/.test(p)) return true;
    if (!/^(Mac|macOS)/.test(p)) return false;
    if (nav.maxTouchPoints > 1) return true; // iPadOS
    var ua = nav.userAgent || "";
    return /Safari\//.test(ua) && !/Chrome|Chromium|CriOS|Edg|Firefox|FxiOS|OPR/.test(ua);
  }

  /** "share" (native sheet), "copy" (clipboard) or null (keep hidden). */
  function shareMode(nav) {
    if (!nav) return null;
    if (typeof nav.share === "function") return "share";
    if (nav.clipboard && typeof nav.clipboard.writeText === "function") return "copy";
    return null;
  }

  /**
   * Share `data` ({title, text, url}) with the native sheet, else copy the
   * link. Resolves to "shared", "copied", "cancelled" or "failed".
   */
  function shareEvent(nav, data) {
    var mode = shareMode(nav);
    if (mode === "share") {
      return Promise.resolve()
        .then(function () {
          return nav.share(data);
        })
        .then(
          function () {
            return "shared";
          },
          function (err) {
            if (err && err.name === "AbortError") return "cancelled";
            return copyLink(nav, data.url);
          }
        );
    }
    if (mode === "copy") return copyLink(nav, data.url);
    return Promise.resolve("failed");
  }

  function copyLink(nav, url) {
    if (!nav.clipboard || typeof nav.clipboard.writeText !== "function") {
      return Promise.resolve("failed");
    }
    return Promise.resolve()
      .then(function () {
        return nav.clipboard.writeText(url);
      })
      .then(
        function () {
          return "copied";
        },
        function () {
          return "failed";
        }
      );
  }

  // ------------------------------------------------------------ near me

  /**
   * "lat,lng" rounded to 3 decimals (about 100 m), the precision the home
   * page's "Near me" filter puts in the address (the server rounds too).
   */
  function roundPosition(lat, lng) {
    var r = function (x) {
      var s = (Math.round(x * 1000) / 1000).toFixed(3);
      return s === "-0.000" ? "0.000" : s;
    };
    return r(lat) + "," + r(lng);
  }

  // ------------------------------------------------------------ transit

  /**
   * "lat,lng" snapped to the ~200 m grid the server uses for public-transport
   * lookups (transit::GRID_LAT / GRID_LNG), so the exact position never
   * leaves the browser.
   */
  function transitOrigin(lat, lng) {
    var snap = function (x, g) {
      var s = (Math.round(x / g) * g).toFixed(3);
      return s === "-0.000" ? "0.000" : s;
    };
    return snap(lat, 0.002) + "," + snap(lng, 0.003);
  }

  /** Straight-line walk estimate, as the map and transit::walk_minutes do. */
  function walkEstimate(lat1, lng1, lat2, lng2) {
    var rad = Math.PI / 180;
    var dLat = (lat2 - lat1) * rad;
    var dLng = (lng2 - lng1) * rad;
    var h =
      Math.pow(Math.sin(dLat / 2), 2) +
      Math.cos(lat1 * rad) * Math.cos(lat2 * rad) * Math.pow(Math.sin(dLng / 2), 2);
    var km = 2 * 6371 * Math.asin(Math.sqrt(h));
    return { minutes: Math.max(1, Math.round(((km * 1.3) / 5) * 60)), km: km };
  }

  function isHttpUrl(u) {
    return typeof u === "string" && /^https?:\/\//i.test(u);
  }

  /**
   * What the event page shows for a `/v1/transit` answer: the walk
   * ("≈ 34 min walk · 2.2 km"), the transit time ("🚇 18 min by public
   * transport", or "" for walking only), the detail
   * ("Overground + 5 min walk · TfL") and the plan links (http(s) only).
   * `fallbackWalk` ({minutes, km}) is used when the answer has no walk.
   */
  function transitView(r, fallbackWalk) {
    var walk = (r && r.walk) || fallbackWalk;
    if (!walk || typeof walk.minutes !== "number") return null;
    var line = "≈ " + walk.minutes + " min walk · " + Number(walk.km).toFixed(1) + " km";
    var t = r && r.transit;
    if (!t || typeof t.minutes !== "number") return { walk: line, transit: "", detail: "", links: [] };
    var detail = typeof t.summary === "string" ? t.summary : "";
    if (typeof t.provider === "string" && t.provider) detail += (detail ? " · " : "") + "via " + t.provider;
    var links = (Array.isArray(t.links) ? t.links : []).filter(function (l) {
      return l && typeof l.label === "string" && isHttpUrl(l.url);
    });
    return {
      walk: line,
      transit: "🚇 " + t.minutes + " min by public transport",
      detail: detail,
      links: links,
    };
  }

  /** A card's badge text ("🚇 18 min"), or "" to keep showing walking only. */
  function transitBadge(r) {
    var t = r && r.transit;
    return t && typeof t.minutes === "number" ? "🚇 " + t.minutes + " min" : "";
  }

  var api = {
    roundPosition: roundPosition,
    transitOrigin: transitOrigin,
    walkEstimate: walkEstimate,
    transitView: transitView,
    transitBadge: transitBadge,
    isApple: isApple,
    shareMode: shareMode,
    shareEvent: shareEvent,
    STORAGE_KEY: STORAGE_KEY,
    londonDate: londonDate,
    placeEvents: placeEvents,
    LEGACY_STORAGE_KEYS: LEGACY_STORAGE_KEYS,
    migrateStorage: migrateStorage,
    loadSaved: loadSaved,
    storeSaved: storeSaved,
    isSaved: isSaved,
    toggle: toggle,
    when: when,
    eventWhen: eventWhen,
    nextSession: nextSession,
    dayOfMonth: dayOfMonth,
    price: price,
    toICS: toICS,
    fromSnapshot: fromSnapshot,
  };
  if (typeof module === "object" && module.exports) {
    module.exports = api;
  }

  // ------------------------------------------------------------ browser

  var doc = root.document;
  if (!doc) return;

  var storage = getStorage();
  migrateStorage(storage);

  function parseSessions(json) {
    if (!json) return null;
    try {
      var v = JSON.parse(json);
      return Array.isArray(v) ? v : null;
    } catch (err) {
      return null;
    }
  }

  function eventFromButton(b) {
    return {
      id: b.getAttribute("data-save-id"),
      title: b.getAttribute("data-title"),
      venue: b.getAttribute("data-venue") || null,
      starts_at: b.getAttribute("data-starts") || null,
      ends_at: b.getAttribute("data-ends") || null,
      all_day: b.getAttribute("data-all-day") === "true",
      sessions: parseSessions(b.getAttribute("data-sessions")),
    };
  }

  function refresh() {
    var items = loadSaved(storage);
    var buttons = doc.querySelectorAll("button.save[data-save-id]");
    for (var i = 0; i < buttons.length; i++) {
      var b = buttons[i];
      b.setAttribute("aria-pressed", isSaved(items, b.getAttribute("data-save-id")) ? "true" : "false");
      if (storage) b.hidden = false;
    }
    var counts = doc.querySelectorAll("[data-saved-count]");
    for (var j = 0; j < counts.length; j++) {
      counts[j].textContent = String(items.length);
      counts[j].hidden = items.length === 0;
    }
    return items;
  }

  var nav = root.navigator;

  /** Show share buttons the browser can serve; point maps links at Apple Maps on Apple devices. */
  function setUpHandoffs() {
    var mode = shareMode(nav);
    var shares = doc.querySelectorAll("button.share[data-share-url]");
    for (var i = 0; i < shares.length; i++) shares[i].hidden = !mode;
    if (isApple(nav)) {
      var links = doc.querySelectorAll("a[data-apple-href]");
      for (var j = 0; j < links.length; j++) {
        var u = safeLink(links[j].getAttribute("data-apple-href"));
        if (u) links[j].setAttribute("href", u);
      }
    }
  }

  var toastTimer = null;
  function toast(text) {
    var t = doc.getElementById("toast");
    if (!t) return say(text);
    t.textContent = text;
    t.hidden = false;
    say(text); // the always-present live region, so screen readers hear it
    if (toastTimer) clearTimeout(toastTimer);
    toastTimer = setTimeout(function () {
      t.hidden = true;
    }, 2500);
  }

  doc.addEventListener("click", function (ev) {
    var s = ev.target && ev.target.closest ? ev.target.closest("button.share[data-share-url]") : null;
    if (!s) return;
    shareEvent(nav, {
      title: s.getAttribute("data-share-title"),
      text: s.getAttribute("data-share-text"),
      url: s.getAttribute("data-share-url"),
    }).then(function (result) {
      if (result === "copied") toast("Link copied");
      else if (result === "failed") toast("Couldn't share: copy the address from the Details page.");
    });
  });

  doc.addEventListener("click", function (ev) {
    var b = ev.target && ev.target.closest ? ev.target.closest("button.save[data-save-id]") : null;
    if (!b) return;
    var items = toggle(loadSaved(storage), eventFromButton(b), new Date().toISOString());
    if (!storeSaved(storage, items)) {
      say("Couldn't save: this browser isn't letting the site store data.");
      return;
    }
    refresh();
    say(isSaved(items, b.getAttribute("data-save-id")) ? "Saved." : "Removed from saved.");
  });

  function say(text) {
    var s = doc.getElementById("save-status");
    if (s) s.textContent = text;
  }

  // Cards rendered by another script (the map's "near me" list).
  doc.addEventListener("musenmingle:cards", function () {
    refresh();
  });

  // Other tabs.
  if (root.addEventListener) {
    root.addEventListener("storage", function (e) {
      if (e.key === STORAGE_KEY) refresh();
    });
  }

  // ------------------------------------------------------------ /saved

  function slot(el, name) {
    return el.querySelector('[data-slot="' + name + '"]');
  }

  function removeSlot(el, name) {
    var s = slot(el, name);
    if (s) s.parentNode.removeChild(s);
  }

  function renderCard(tpl, item, event, nowIso, gone) {
    var node = tpl.content.firstElementChild.cloneNode(true);
    var e = event || fromSnapshot(item);
    var href = "/events/" + encodeURIComponent(e.id);
    var title = slot(node, "title");
    title.textContent = e.title;
    title.setAttribute("href", href);
    slot(node, "when").textContent = eventWhen(e, nowIso);
    var venue = slot(node, "venue");
    if (e.venue_name && e.venue_slug) {
      var va = venue.ownerDocument.createElement("a");
      va.className = "venue-page";
      va.setAttribute("href", "/venues/" + encodeURIComponent(e.venue_slug));
      va.textContent = e.venue_name;
      venue.appendChild(va);
    } else if (e.venue_name) venue.textContent = e.venue_name;
    else venue.hidden = true;
    var category = slot(node, "category");
    if (e.category) category.textContent = titleCase(e.category);
    else category.hidden = true;
    var p = event ? price(event) : null;
    var priceEl = slot(node, "price");
    if (p) {
      priceEl.textContent = p;
      if (event.is_free) priceEl.classList.add("free");
    } else priceEl.hidden = true;
    // Only our own thumbnails (never the source's image), with the credit.
    var figure = slot(node, "figure");
    var credit = event && event.image_credit;
    var creditUrl = credit ? safeLink(credit.url) : null;
    if (
      event &&
      typeof event.thumbnail_url === "string" &&
      event.thumbnail_url.indexOf("/thumbs/") === 0 &&
      credit &&
      creditUrl
    ) {
      slot(node, "image").setAttribute("src", event.thumbnail_url);
      var c = slot(node, "credit");
      c.setAttribute("href", creditUrl);
      c.textContent = credit.name;
      if (event) slot(node, "image-link").setAttribute("href", href);
      figure.hidden = false;
      removeSlot(node, "blank-link");
    } else {
      if (figure) figure.parentNode.removeChild(figure);
      // The decorative "Monograph Blank" (same as web::blank on the server).
      var blank = slot(node, "blank");
      if (blank) {
        var kind = slot(node, "blank-kind");
        if (e.category) kind.textContent = titleCase(e.category);
        else kind.hidden = true;
        slot(node, "blank-venue").textContent = e.venue_name || "London";
        slot(node, "blank-numeral").textContent = e.starts_at ? dayOfMonth(e.starts_at) : "";
        blank.hidden = false;
        if (event) slot(node, "blank-link").setAttribute("href", href);
      }
    }
    var details = slot(node, "details");
    slot(node, "details-title").textContent = ": " + e.title;
    var sources = slot(node, "sources");
    if (event) {
      details.setAttribute("href", href);
      var ics = slot(node, "ics");
      ics.setAttribute("href", href + ".ics");
      slot(node, "ics-title").textContent = "Add to calendar: " + e.title;
      ics.hidden = false;
      // The card leads to our own detail page; the source's page is the
      // link under it. Venue links keep the referrer (rel="noopener", no
      // "noreferrer").
      var primary = null;
      (event.sources || []).forEach(function (s) {
        var u = safeLink(s.url);
        if (!u) return;
        var name = s.display_name || s.source;
        if (!primary) {
          primary = u;
          var cta = slot(node, "cta");
          cta.setAttribute("href", u);
          cta.textContent = "See it on " + name + " \u2192";
          var vh = doc.createElement("span");
          vh.className = "vh";
          vh.textContent = ": " + e.title;
          cta.appendChild(vh);
          slot(node, "links").hidden = false;
          return;
        }
        if (u === primary) return;
        sources.appendChild(doc.createTextNode(" · "));
        var a = doc.createElement("a");
        a.setAttribute("href", u);
        a.setAttribute("rel", "noopener");
        a.textContent = "also on " + name;
        sources.appendChild(a);
      });
    } else {
      slot(node, "details-wrap").hidden = true;
      if (gone) slot(node, "gone").hidden = false;
    }
    var btn = node.querySelector("button.save");
    btn.setAttribute("data-save-id", e.id);
    btn.setAttribute("data-title", e.title);
    btn.setAttribute("data-venue", e.venue_name || "");
    btn.setAttribute("data-starts", e.starts_at || "");
    btn.setAttribute("data-ends", e.ends_at || "");
    btn.setAttribute("data-all-day", e.all_day ? "true" : "false");
    if (Array.isArray(e.sessions) && e.sessions.length > 1) {
      btn.setAttribute("data-sessions", JSON.stringify(e.sessions));
    }
    slot(node, "save-title").textContent = ": " + e.title;
    return node;
  }

  function fetchEvents(ids) {
    var chunks = [];
    for (var i = 0; i < ids.length; i += IDS_PER_REQUEST) {
      chunks.push(ids.slice(i, i + IDS_PER_REQUEST));
    }
    return Promise.all(
      chunks.map(function (chunk) {
        var url = "/v1/events?limit=" + IDS_PER_REQUEST + "&ids=" + chunk.join(",");
        return fetch(url, { headers: { Accept: "application/json" } }).then(function (r) {
          if (!r.ok) throw new Error("HTTP " + r.status);
          return r.json();
        });
      })
    ).then(function (pages) {
      var byId = {};
      pages.forEach(function (p) {
        (p.events || []).forEach(function (e) {
          byId[e.id] = e;
        });
      });
      return byId;
    });
  }

  function renderSaved() {
    var list = doc.getElementById("saved-list");
    var tpl = doc.getElementById("card-template");
    if (!list || !tpl || !tpl.content) return;
    var items = loadSaved(storage);
    var empty = doc.getElementById("saved-empty");
    var exportBtn = doc.getElementById("export-ics");
    var status = doc.getElementById("saved-status");
    if (!storage) {
      status.textContent = "This browser isn't letting the site store data, so saving is unavailable.";
      return;
    }
    if (items.length === 0) {
      empty.hidden = false;
      list.replaceChildren();
      exportBtn.hidden = true;
      return;
    }
    empty.hidden = true;
    status.textContent = "Loading your saved events…";
    var nowIso = new Date().toISOString();
    var show = function (byId, fetched) {
      list.replaceChildren();
      items.forEach(function (item) {
        list.appendChild(renderCard(tpl, item, byId[item.id] || null, nowIso, fetched && !byId[item.id]));
      });
      status.textContent = "";
      exportBtn.hidden = false;
      exportBtn.onclick = function () {
        var events = items.map(function (item) {
          var e = byId[item.id];
          return e
            ? {
                id: e.id,
                title: e.title,
                venue: e.venue_name,
                starts_at: e.starts_at,
                ends_at: e.ends_at,
                all_day: e.all_day,
                sessions: e.sessions || null,
              }
            : {
                id: item.id,
                title: item.snapshot.title,
                venue: item.snapshot.venue,
                starts_at: item.snapshot.starts_at,
                ends_at: item.snapshot.ends_at,
                all_day: isAllDay(item.snapshot),
                sessions: item.snapshot.sessions || null,
              };
        });
        var blob = new Blob([toICS(events, new Date().toISOString(), root.location.origin)], {
          type: "text/calendar",
        });
        var a = doc.createElement("a");
        a.href = URL.createObjectURL(blob);
        a.download = "musenmingle-saved.ics";
        doc.body.appendChild(a);
        a.click();
        setTimeout(function () {
          URL.revokeObjectURL(a.href);
          a.remove();
        }, 0);
      };
      refresh();
    };
    fetchEvents(
      items.map(function (i) {
        return i.id;
      })
    ).then(function (byId) {
      show(byId, true);
    }, function () {
      show({}, false);
      status.textContent = "Couldn't reach the server; showing what was saved.";
    });
  }

  // ------------------------------------------------------------ /saved/calendar

  function el(tag, cls, text) {
    var n = doc.createElement(tag);
    if (cls) n.className = cls;
    if (text != null) n.textContent = text;
    return n;
  }

  function calendarEntry(e, mark) {
    var li = el("li", mark === "opens" ? "cal-ev opens" : mark === "last" ? "cal-ev last-day" : "cal-ev");
    var a = el("a");
    a.setAttribute("href", "/events/" + encodeURIComponent(e.id));
    var meta = el("span", "cal-meta");
    if (mark === "opens") meta.appendChild(el("span", "cal-flag", "Opens"));
    else if (mark === "last") meta.appendChild(el("span", "cal-flag", "Last day"));
    else meta.appendChild(el("span", "cal-time", isAllDay(e) ? "All day" : fmtTime(e.starts_at)));
    if (e.is_free) meta.appendChild(el("span", "cal-free", "Free"));
    a.appendChild(meta);
    a.appendChild(el("span", "cal-ev-title", e.title));
    if (e.venue_name) a.appendChild(el("span", "cal-venue", e.venue_name));
    li.appendChild(a);
    return li;
  }

  function ongoingEntry(e) {
    var li = el("li");
    var a = el("a");
    a.setAttribute("href", "/events/" + encodeURIComponent(e.id));
    a.appendChild(el("span", "sq"));
    a.appendChild(el("span", "og-title", e.title));
    if (e.venue_name) a.appendChild(el("span", "og-venue", " · " + e.venue_name));
    a.appendChild(el("span", "og-until", " until " + fmtDate(e.ends_at || e.starts_at).replace(/^\S+ /, "")));
    if (e.is_free) a.appendChild(el("span", "cal-free", "Free"));
    li.appendChild(a);
    return li;
  }

  function renderSavedCalendar() {
    var box = doc.getElementById("saved-calendar");
    if (!box) return;
    var first = box.getAttribute("data-first");
    var last = box.getAttribute("data-last");
    var status = doc.getElementById("saved-cal-status");
    if (!storage) {
      status.textContent = "This browser isn't letting the site store data, so saving is unavailable.";
      return;
    }
    var items = loadSaved(storage);
    if (items.length === 0) {
      status.textContent = "Nothing saved yet. Use the Save button on any event, then come back here.";
      return;
    }
    status.textContent = "Loading your saved events…";
    var show = function (byId) {
      var events = items
        .map(function (item) {
          return byId[item.id] || fromSnapshot(item);
        })
        .filter(function (e) {
          return !!e.starts_at;
        })
        .sort(function (a, b) {
          return a.starts_at < b.starts_at ? -1 : a.starts_at > b.starts_at ? 1 : 0;
        });
      var placed = placeEvents(events, first, last);
      var shown = 0;
      Object.keys(placed.days).forEach(function (day) {
        var list = box.querySelector('ul[data-day="' + day + '"]');
        if (!list) return;
        list.replaceChildren();
        placed.days[day].forEach(function (entry) {
          list.appendChild(calendarEntry(events[entry[0]], entry[1]));
          shown++;
        });
        if (list.parentNode) list.parentNode.classList.remove("empty");
      });
      var strip = doc.getElementById("saved-ongoing-strip");
      var ongoing = doc.getElementById("saved-ongoing");
      if (strip && ongoing) {
        ongoing.replaceChildren();
        placed.ongoing.forEach(function (i) {
          ongoing.appendChild(ongoingEntry(events[i]));
        });
        strip.hidden = placed.ongoing.length === 0;
      }
      status.textContent =
        shown === 0 && placed.ongoing.length === 0
          ? "None of your saved events are on in these dates."
          : "";
    };
    fetchEvents(
      items.map(function (i) {
        return i.id;
      })
    ).then(show, function () {
      show({});
      status.textContent = "Couldn't reach the server; showing what was saved.";
    });
  }

  // ------------------------------------------------------------ / (Near me)

  // The filters' "Near me" group is rendered hidden (it needs the browser's
  // location). Tapping "Use my location" asks for it once, then submits the
  // filter form with the rounded position in `here=`, the area preset
  // cleared and (unless one was chosen) the closest-first sort.
  function setUpNearMe() {
    var group = doc.querySelector("[data-near-me]");
    var button = doc.querySelector("[data-near-me-locate]");
    var geo = root.navigator && root.navigator.geolocation;
    if (!group || !button || !geo) return;
    var form = group.closest("form");
    var status = group.querySelector("[data-near-me-status]");
    var say = function (text) {
      if (status) status.textContent = text;
    };
    group.hidden = false;
    button.hidden = false;
    button.addEventListener("click", function () {
      button.disabled = true;
      say("Finding where you are…");
      geo.getCurrentPosition(
        function (pos) {
          var input = form.querySelector('input[name="here"]');
          if (!input) {
            input = doc.createElement("input");
            input.type = "hidden";
            input.name = "here";
            form.appendChild(input);
          }
          input.value = roundPosition(pos.coords.latitude, pos.coords.longitude);
          var area = form.querySelector('select[name="near"]');
          if (area) area.value = "";
          // Like an area, Near me lists the closest first unless a sort
          // (or a search, ranked by match) was chosen.
          var params = new URLSearchParams(root.location.search);
          var sort = form.querySelector('select[name="sort"]');
          if (sort && !params.get("sort") && !params.get("q")) sort.value = "nearest";
          say("Showing events near you…");
          form.submit();
        },
        function () {
          button.disabled = false;
          say("Couldn't get your location. Check the browser's location permission, or pick an area.");
        },
        { enableHighAccuracy: false, timeout: 15000, maximumAge: 300000 }
      );
    });
  }

  // ------------------------------------------------------------ transit

  function fetchTransit(from, id) {
    var url = "/v1/transit?from=" + encodeURIComponent(from) + "&event=" + encodeURIComponent(id);
    return root.fetch(url, { headers: { Accept: "application/json" } }).then(function (r) {
      if (!r.ok) throw new Error("transit " + r.status);
      return r.json();
    });
  }

  // Event page: "Getting there" (rendered hidden). If the location
  // permission is already granted, look the times up straight away;
  // otherwise a "Transit time" button asks. Only a ~200 m-rounded position
  // is sent (to us; we ask the city's journey planner).
  function setUpTransit() {
    var box = doc.querySelector("[data-transit-event]");
    var geo = root.navigator && root.navigator.geolocation;
    if (!box || !geo || !root.fetch) return;
    var id = box.getAttribute("data-transit-event");
    var lat = parseFloat(box.getAttribute("data-lat"));
    var lng = parseFloat(box.getAttribute("data-lng"));
    var walkLine = box.querySelector("[data-transit-walk]");
    var transitLine = box.querySelector("[data-transit-line]");
    var detail = box.querySelector("[data-transit-detail]");
    var links = box.querySelector("[data-transit-links]");
    var button = box.querySelector("[data-transit-locate]");
    var status = box.querySelector("[data-transit-status]");
    var say = function (text) {
      status.textContent = text;
    };
    var show = function (view) {
      if (!view) return;
      walkLine.textContent = view.walk;
      transitLine.textContent = view.transit;
      transitLine.hidden = !view.transit;
      detail.textContent = view.detail;
      detail.hidden = !view.detail;
      links.replaceChildren();
      view.links.forEach(function (l, i) {
        if (i) links.appendChild(doc.createTextNode(" "));
        var a = doc.createElement("a");
        a.setAttribute("href", l.url);
        a.setAttribute("rel", "noopener");
        a.textContent = l.label + " ↗";
        links.appendChild(a);
      });
      links.hidden = view.links.length === 0;
    };
    var busy = false;
    var run = function () {
      if (busy) return;
      busy = true;
      button.disabled = true;
      say("Finding where you are…");
      geo.getCurrentPosition(
        function (pos) {
          var walk = walkEstimate(pos.coords.latitude, pos.coords.longitude, lat, lng);
          say("Checking public transport…");
          fetchTransit(transitOrigin(pos.coords.latitude, pos.coords.longitude), id)
            .then(
              function (r) {
                // Nowhere near (another city): nothing useful to say.
                if (r && r.status === "outside_area") {
                  box.hidden = true;
                  return;
                }
                show(transitView(r, walk));
              },
              function () {
                // Planner or server trouble: walking only, quietly (unless
                // it's too far to walk anyway).
                if (walk.km > 15) box.hidden = true;
                else show(transitView(null, walk));
              }
            )
            .then(function () {
              say("");
              button.hidden = true;
            });
        },
        function () {
          busy = false;
          button.disabled = false;
          say("Couldn't get your location. Check the browser's location permission.");
        },
        { enableHighAccuracy: false, timeout: 15000, maximumAge: 300000 }
      );
    };
    box.hidden = false;
    button.hidden = false;
    button.addEventListener("click", run);
    var perms = root.navigator.permissions;
    if (perms && typeof perms.query === "function") {
      perms.query({ name: "geolocation" }).then(
        function (p) {
          if (p.state === "granted") run();
        },
        function () {}
      );
    }
  }

  // Home page, sorted closest first from "Near me": add public-transport
  // time to the cards as they scroll into view (two lookups at a time).
  function setUpTransitCards() {
    var badges = doc.querySelectorAll("[data-transit-card]");
    if (!badges.length || !root.fetch || !root.IntersectionObserver) return;
    var params = new URLSearchParams(root.location.search);
    var here = params.get("here");
    if (!here || params.get("sort") !== "nearest") return;
    var queue = [];
    var active = 0;
    var pump = function () {
      while (active < 2 && queue.length) {
        var badge = queue.shift();
        active++;
        fetchTransit(here, badge.getAttribute("data-transit-card"))
          .then(
            function (b) {
              return function (r) {
                var text = transitBadge(r);
                if (text) {
                  b.textContent = text;
                  b.hidden = false;
                }
              };
            }(badge),
            function () {}
          )
          .then(function () {
            active--;
            pump();
          });
      }
    };
    var byCard = new Map();
    var io = new root.IntersectionObserver(
      function (entries) {
        entries.forEach(function (en) {
          if (!en.isIntersecting) return;
          io.unobserve(en.target);
          queue.push(byCard.get(en.target));
        });
        pump();
      },
      { rootMargin: "100px" }
    );
    Array.prototype.forEach.call(badges, function (b) {
      var card = b.closest("article") || b.parentNode;
      byCard.set(card, b);
      io.observe(card);
    });
  }

  function start() {
    setUpNearMe();
    setUpTransit();
    setUpTransitCards();
    setUpHandoffs();
    refresh();
    renderSaved();
    renderSavedCalendar();
  }
  if (doc.readyState === "loading") doc.addEventListener("DOMContentLoaded", start);
  else start();
})(typeof window !== "undefined" ? window : globalThis);
