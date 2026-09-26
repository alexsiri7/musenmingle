// LetsArt: "Saved" events, stored only in this browser (localStorage).
// Served from /static/app.js; no third-party code, no inline scripts.
// Progressive enhancement: pages render and work without this file; the
// save buttons are rendered `hidden` and only shown from here.
//
// Pure helpers are exported for `node --test tests/js/` (see the bottom).
(function (root) {
  "use strict";

  var STORAGE_KEY = "letsart.saved.v1";
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
  function when(startsIso, endsIso, nowIso) {
    if (!startsIso) return "";
    var start = new Date(startsIso);
    var end = endsIso ? new Date(endsIso) : null;
    if (end && end > start && fmtDate(endsIso) !== fmtDate(startsIso)) {
      return start <= new Date(nowIso)
        ? "Until " + fmtDate(endsIso)
        : fmtDate(startsIso) + " – " + fmtDate(endsIso);
    }
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
   * A VCALENDAR for `events` ({id, title, venue, starts_at, ends_at, url}).
   * `origin` builds each event's LetsArt link.
   */
  function toICS(events, nowIso, origin) {
    var lines = [
      "BEGIN:VCALENDAR",
      "VERSION:2.0",
      "PRODID:-//LetsArt//Saved events//EN",
      "CALSCALE:GREGORIAN",
    ];
    events.forEach(function (e) {
      if (!e.starts_at) return;
      lines.push("BEGIN:VEVENT");
      lines.push("UID:" + e.id + "@letsart");
      lines.push("DTSTAMP:" + icsTime(nowIso));
      lines.push("DTSTART:" + icsTime(e.starts_at));
      if (e.ends_at) lines.push("DTEND:" + icsTime(e.ends_at));
      lines.push("SUMMARY:" + icsEscape(e.title));
      if (e.venue) lines.push("LOCATION:" + icsEscape(e.venue));
      lines.push("URL:" + origin + "/events/" + e.id);
      lines.push("END:VEVENT");
    });
    lines.push("END:VCALENDAR");
    return lines.map(fold).join("\r\n") + "\r\n";
  }

  var api = {
    STORAGE_KEY: STORAGE_KEY,
    loadSaved: loadSaved,
    storeSaved: storeSaved,
    isSaved: isSaved,
    toggle: toggle,
    when: when,
    price: price,
    toICS: toICS,
  };
  if (typeof module === "object" && module.exports) {
    module.exports = api;
  }

  // ------------------------------------------------------------ browser

  var doc = root.document;
  if (!doc) return;

  var storage = getStorage();

  function eventFromButton(b) {
    return {
      id: b.getAttribute("data-save-id"),
      title: b.getAttribute("data-title"),
      venue: b.getAttribute("data-venue") || null,
      starts_at: b.getAttribute("data-starts") || null,
      ends_at: b.getAttribute("data-ends") || null,
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

  function renderCard(tpl, item, event, nowIso, gone) {
    var node = tpl.content.firstElementChild.cloneNode(true);
    var e = event || {
      id: item.id,
      title: item.snapshot.title,
      venue_name: item.snapshot.venue,
      starts_at: item.snapshot.starts_at,
      ends_at: item.snapshot.ends_at,
    };
    var href = "/events/" + encodeURIComponent(e.id);
    var title = slot(node, "title");
    title.textContent = e.title;
    title.setAttribute("href", href);
    slot(node, "when").textContent = when(e.starts_at, e.ends_at, nowIso);
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
    var img = slot(node, "image");
    if (event && typeof event.image_url === "string" && /^https:\/\//i.test(event.image_url)) {
      img.setAttribute("src", event.image_url);
      img.hidden = false;
    } else if (img) {
      img.parentNode.removeChild(img);
    }
    var details = slot(node, "details");
    slot(node, "details-title").textContent = ": " + e.title;
    var sources = slot(node, "sources");
    if (event) {
      details.setAttribute("href", href);
      (event.sources || []).forEach(function (s) {
        var u = safeLink(s.url);
        if (!u) return;
        sources.appendChild(doc.createTextNode(" · "));
        var a = doc.createElement("a");
        a.setAttribute("href", u);
        a.setAttribute("rel", "noopener noreferrer");
        a.textContent = "on " + s.source;
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
            ? { id: e.id, title: e.title, venue: e.venue_name, starts_at: e.starts_at, ends_at: e.ends_at }
            : {
                id: item.id,
                title: item.snapshot.title,
                venue: item.snapshot.venue,
                starts_at: item.snapshot.starts_at,
                ends_at: item.snapshot.ends_at,
              };
        });
        var blob = new Blob([toICS(events, new Date().toISOString(), root.location.origin)], {
          type: "text/calendar",
        });
        var a = doc.createElement("a");
        a.href = URL.createObjectURL(blob);
        a.download = "letsart-saved.ics";
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

  function start() {
    refresh();
    renderSaved();
  }
  if (doc.readyState === "loading") doc.addEventListener("DOMContentLoaded", start);
  else start();
})(typeof window !== "undefined" ? window : globalThis);
