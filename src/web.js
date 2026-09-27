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
    events.forEach(function (e) {
      if (!e.starts_at) return;
      lines.push("BEGIN:VEVENT");
      lines.push("UID:" + e.id + "@musenmingle.interstellarai.net");
      lines.push("DTSTAMP:" + icsTime(nowIso));
      if (isAllDay(e)) {
        lines.push("DTSTART;VALUE=DATE:" + icsDate(e.starts_at, 0));
        lines.push("DTEND;VALUE=DATE:" + icsDate(e.ends_at || e.starts_at, 1));
      } else {
        lines.push("DTSTART:" + icsTime(e.starts_at));
        if (e.ends_at) lines.push("DTEND:" + icsTime(e.ends_at));
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

  var api = {
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

  function eventFromButton(b) {
    return {
      id: b.getAttribute("data-save-id"),
      title: b.getAttribute("data-title"),
      venue: b.getAttribute("data-venue") || null,
      starts_at: b.getAttribute("data-starts") || null,
      ends_at: b.getAttribute("data-ends") || null,
      all_day: b.getAttribute("data-all-day") === "true",
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
    slot(node, "when").textContent = when(e.starts_at, e.ends_at, nowIso, e.all_day);
    var venue = slot(node, "venue");
    if (e.venue_name) venue.textContent = e.venue_name;
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
      figure.hidden = false;
      removeSlot(node, "blank");
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
      // The primary call to action is the source's own page; venue links
      // keep the referrer (rel="noopener", no "noreferrer").
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
          slot(node, "cta-wrap").hidden = false;
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
      details.hidden = true;
      if (gone) slot(node, "gone").hidden = false;
    }
    var btn = node.querySelector("button.save");
    btn.setAttribute("data-save-id", e.id);
    btn.setAttribute("data-title", e.title);
    btn.setAttribute("data-venue", e.venue_name || "");
    btn.setAttribute("data-starts", e.starts_at || "");
    btn.setAttribute("data-ends", e.ends_at || "");
    btn.setAttribute("data-all-day", e.all_day ? "true" : "false");
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
              }
            : {
                id: item.id,
                title: item.snapshot.title,
                venue: item.snapshot.venue,
                starts_at: item.snapshot.starts_at,
                ends_at: item.snapshot.ends_at,
                all_day: isAllDay(item.snapshot),
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

  function start() {
    setUpHandoffs();
    refresh();
    renderSaved();
    renderSavedCalendar();
  }
  if (doc.readyState === "loading") doc.addEventListener("DOMContentLoaded", start);
  else start();
})(typeof window !== "undefined" ? window : globalThis);
