// Muse & Mingle: the "Near me, right now" map (/map). Served from
// /static/map/map.mjs; no third-party code, no inline scripts, DOM built
// with textContent only.
//
// Progressive enhancement: /map is a server-rendered list that works without
// this file. With it, a map of our own London tiles appears (MapLibre GL JS
// and the pmtiles reader, vendored under /static/map/), and "Use my exact
// coordinates" sorts events by walking distance from the visitor. The
// position stays in the browser: the script fetches the same London-wide
// `at=now` listing anyone gets (no location in the request) and computes
// distances here.
//
// Pure helpers are exported for `node --test tests/js/` (see the bottom).

const TZ = "Europe/London";
const EARTH_RADIUS_KM = 6371.0088;
/// Walking pace and a detour factor for straight-line distances (the same
/// as `WALK_KMH` / `WALK_DETOUR` in src/listing.rs, the Near me filter).
const WALK_KMH = 5;
const DETOUR = 1.3;
const LIST_MAX = 50;
const PAGE_LIMIT = 100;
const MAX_PAGES = 5;

// ------------------------------------------------------------ pure helpers

export function haversineKm(a, b) {
  const rad = (d) => (d * Math.PI) / 180;
  const dLat = rad(b.lat - a.lat);
  const dLng = rad(b.lng - a.lng);
  const h =
    Math.sin(dLat / 2) ** 2 + Math.cos(rad(a.lat)) * Math.cos(rad(b.lat)) * Math.sin(dLng / 2) ** 2;
  return 2 * EARTH_RADIUS_KM * Math.asin(Math.min(1, Math.sqrt(h)));
}

export function walkMinutes(km) {
  return Math.max(1, Math.round(((km * DETOUR) / WALK_KMH) * 60));
}

/// "≈ 9 min walk · 0.6 km" (an estimate from straight-line distance).
export function walkLabel(km) {
  return "≈ " + walkMinutes(km) + " min walk · " + km.toFixed(1) + " km";
}

/// Public-transport time is only worth asking for from this far (km; the
/// server's transit::MIN_TRANSIT_KM).
export const MIN_TRANSIT_KM = 1.5;

/// "lat,lng" on the ~200 m grid the server rounds transit origins to, so
/// the exact position never leaves the device.
export function transitOrigin(here) {
  const snap = (x, g) => {
    const s = (Math.round(x / g) * g).toFixed(3);
    return s === "-0.000" ? "0.000" : s;
  };
  return snap(here.lat, 0.002) + "," + snap(here.lng, 0.003);
}

/// " · 🚇 18 min" for a `/v1/transit` answer with a journey, else "".
export function transitSuffix(r) {
  const t = r && r.transit;
  return t && typeof t.minutes === "number" ? " · 🚇 " + t.minutes + " min" : "";
}

/// Same as `area_distance` in src/web/map.rs.
export function areaLabel(km) {
  return km < 0.1 ? "At the area centre" : km.toFixed(1) + " km from area centre";
}

const partsFmt = new Intl.DateTimeFormat("en-GB", {
  timeZone: TZ,
  year: "numeric",
  month: "2-digit",
  day: "2-digit",
  hour: "2-digit",
  minute: "2-digit",
  hourCycle: "h23",
  weekday: "short",
});

/// London wall-clock parts of an instant: { ymd, hm, weekday }.
export function londonParts(iso) {
  const p = {};
  for (const x of partsFmt.formatToParts(new Date(iso))) p[x.type] = x.value;
  return { ymd: p.year + "-" + p.month + "-" + p.day, hm: p.hour + ":" + p.minute, weekday: p.weekday };
}

const DAY_CODES = { Mon: "mon", Tue: "tue", Wed: "wed", Thu: "thu", Fri: "fri", Sat: "sat", Sun: "sun" };

/// Today's status from weekly opening hours (the API's `opening_hours`:
/// `[{days: ["sat"], opens: "11:00", closes: "15:00"}]`, London time);
/// mirrors `OpeningHours::today_status` in src/hours.rs.
export function hoursStatus(hours, nowIso) {
  const now = londonParts(nowIso);
  const code = DAY_CODES[now.weekday];
  const r = hours.find((x) => Array.isArray(x.days) && x.days.indexOf(code) !== -1);
  if (!r) return "Closed today";
  if (now.hm < r.opens) return "Opens today at " + r.opens;
  if (now.hm < r.closes) return "Open now until " + r.closes;
  return "Closed now";
}

/// The card's status line; mirrors `live_label` in src/web/map.rs.
/// `hours` is the event's `opening_hours` (or null).
export function liveLabel(startIso, endIso, allDay, nowIso, hours) {
  const start = londonParts(startIso);
  const today = londonParts(nowIso).ymd;
  const startMs = Date.parse(startIso);
  const nowMs = Date.parse(nowIso);
  const day = (p) => (p.ymd === today ? "" : p.weekday + " ");
  const onDay = (p) => (p.ymd === today ? "today" : p.weekday);
  const end = endIso && Date.parse(endIso) > startMs ? londonParts(endIso) : null;
  const ranged = end !== null && end.ymd !== start.ymd;
  const untimed = allDay || start.hm === "00:00";
  const future = startMs > nowMs;
  if (ranged) {
    if (future && !untimed) return "Opens " + day(start) + start.hm;
    if (future) return "Opens " + onDay(start) + " · check opening hours";
    if (Array.isArray(hours) && hours.length) return hoursStatus(hours, nowIso);
    return "Open today · check opening hours";
  }
  if (untimed) {
    const d = onDay(start);
    return d.charAt(0).toUpperCase() + d.slice(1) + " · check opening hours";
  }
  if (future) return "Starts " + day(start) + start.hm + (end ? "–" + end.hm : "");
  return end ? "On now until " + end.hm : "Started " + start.hm;
}

function titleCase(s) {
  return s ? s.charAt(0).toUpperCase() + s.slice(1) : "";
}

export function safeLink(u) {
  if (typeof u !== "string") return null;
  try {
    const url = new URL(u);
    return url.protocol === "http:" || url.protocol === "https:" ? url.href : null;
  } catch (e) {
    return null;
  }
}

function money(amount, currency) {
  const n = Number(amount);
  const s = Number.isInteger(n) ? String(n) : n.toFixed(2);
  if (!currency || currency === "GBP") return "£" + s;
  if (currency === "EUR") return "€" + s;
  if (currency === "USD") return "$" + s;
  return s + " " + currency;
}

/// Same as `price` in src/web.rs.
export function priceLabel(e) {
  if (e.is_free) return "Free";
  const a = e.price_min != null ? e.price_min : null;
  const b = e.price_max != null ? e.price_max : null;
  if (a !== null && b !== null && Number(a) !== Number(b)) {
    return money(a, e.currency) + "–" + money(b, e.currency);
  }
  if (a !== null) return money(a, e.currency);
  if (b !== null) return "Up to " + money(b, e.currency);
  return null;
}

/// A listing event (GET /v1/events JSON) as the map's model. The first
/// source with a link is the call to action, as on the Saved page.
export function eventFromJson(e, nowIso) {
  let cta = null;
  for (const s of e.sources || []) {
    const u = safeLink(s.url);
    if (u) {
      cta = { url: u, name: s.display_name || s.source };
      break;
    }
  }
  const credit = e.image_credit && safeLink(e.image_credit.url);
  const thumb =
    typeof e.thumbnail_url === "string" && e.thumbnail_url.indexOf("/thumbs/") === 0 && credit
      ? { src: e.thumbnail_url, credit: { name: e.image_credit.name, url: credit } }
      : null;
  return {
    id: e.id,
    title: e.title,
    venue: e.venue_name || "",
    lat: typeof e.lat === "number" ? e.lat : null,
    lng: typeof e.lng === "number" ? e.lng : null,
    starts_at: e.starts_at,
    ends_at: e.ends_at || null,
    all_day: !!e.all_day,
    category: titleCase(e.category),
    status: liveLabel(e.starts_at, e.ends_at, !!e.all_day, nowIso, e.opening_hours || null),
    price: priceLabel(e),
    free: !!e.is_free,
    thumb,
    cta,
  };
}

/// The model of a server-rendered card (`near_card` in src/web/map.rs).
export function eventFromDataset(d) {
  const num = (v) => (v === undefined || v === "" ? null : Number(v));
  const credit = safeLink(d.creditUrl);
  const cta = safeLink(d.ctaUrl);
  return {
    id: d.eventId,
    n: num(d.n),
    title: d.title || "",
    venue: d.venue || "",
    lat: num(d.lat),
    lng: num(d.lng),
    starts_at: d.starts || null,
    category: d.category || "",
    status: d.status || "",
    thumb:
      d.thumb && d.thumb.indexOf("/thumbs/") === 0 && credit
        ? { src: d.thumb, w: num(d.thumbW), h: num(d.thumbH), credit: { name: d.creditName || "", url: credit } }
        : null,
    cta: cta ? { url: cta, name: d.ctaName || "" } : null,
  };
}

/// Events with coordinates, nearest to `here` first (ties: earlier start).
export function byDistanceFrom(here, events) {
  return events
    .filter((e) => e.lat !== null && e.lng !== null)
    .map((e) => Object.assign({}, e, { km: haversineKm(here, e) }))
    .sort((a, b) => a.km - b.km || Date.parse(a.starts_at) - Date.parse(b.starts_at));
}

/// "soon": ongoing events first (as "now"), then by start; nearer first.
export function sortEvents(events, mode, nowIso) {
  const now = Date.parse(nowIso);
  const list = events.slice();
  if (mode === "soon") {
    list.sort(
      (a, b) =>
        Math.max(Date.parse(a.starts_at), now) - Math.max(Date.parse(b.starts_at), now) ||
        (a.km || 0) - (b.km || 0)
    );
  } else {
    list.sort((a, b) => (a.km || 0) - (b.km || 0));
  }
  return list;
}

/// The browser's position, asked for once. Rejects with
/// { reason: "unsupported" | "denied" | "unavailable" | "timeout" }.
export function locate(geolocation) {
  return new Promise((resolve, reject) => {
    if (!geolocation || typeof geolocation.getCurrentPosition !== "function") {
      reject({ reason: "unsupported" });
      return;
    }
    geolocation.getCurrentPosition(
      (pos) => resolve({ lat: pos.coords.latitude, lng: pos.coords.longitude }),
      (err) => {
        const code = err && err.code;
        reject({ reason: code === 1 ? "denied" : code === 3 ? "timeout" : "unavailable" });
      },
      { enableHighAccuracy: true, timeout: 15000, maximumAge: 60000 }
    );
  });
}

export function locateMessage(reason, areaLabelText) {
  const why = {
    unsupported: "This browser can't share its location",
    denied: "Location permission was not given",
    timeout: "Finding your location took too long",
    unavailable: "Your location isn't available right now",
  }[reason] || "Your location isn't available";
  return why + ". Showing " + areaLabelText + " instead; pick another area below.";
}

/// Everything on in London for `span` ("now" | "today"): GET /v1/events
/// pages (no location sent), at most MAX_PAGES of PAGE_LIMIT.
export async function fetchLondon(fetchFn, span) {
  const events = [];
  let cursor = null;
  for (let i = 0; i < MAX_PAGES; i++) {
    let url = "/v1/events?at=" + (span === "today" ? "today" : "now") + "&limit=" + PAGE_LIMIT;
    if (cursor) url += "&cursor=" + encodeURIComponent(cursor);
    const r = await fetchFn(url, { headers: { Accept: "application/json" } });
    if (!r.ok) throw new Error("HTTP " + r.status);
    const page = await r.json();
    for (const e of page.events || []) events.push(e);
    cursor = page.next_cursor;
    if (!cursor) break;
  }
  return events;
}

// ------------------------------------------------------------ browser

const doc = typeof document !== "undefined" ? document : null;
if (doc && doc.getElementById("near-list")) {
  if (doc.readyState === "loading") doc.addEventListener("DOMContentLoaded", start);
  else start();
}

function slot(el, name) {
  return el.querySelector('[data-slot="' + name + '"]');
}

function el(tag, cls, text) {
  const e = doc.createElement(tag);
  if (cls) e.className = cls;
  if (text !== undefined && text !== null) e.textContent = text;
  return e;
}

function start() {
  const list = doc.getElementById("near-list");
  const tpl = doc.getElementById("near-card-template");
  const locateBtn = doc.getElementById("locate");
  const status = doc.getElementById("locate-status");
  const count = doc.getElementById("near-count");
  const sortSel = doc.getElementById("sort");
  const pane = doc.getElementById("map-pane");
  const activeArea = doc.querySelector('[data-slot="area-active"]');
  const areaText = activeArea ? activeArea.textContent : "Central London";
  const state = {
    mode: "area", // or "here"
    here: null,
    span: (doc.querySelector('.seg[aria-current="true"]') || { dataset: { span: "now" } }).dataset.span,
    sort: sortSel ? sortSel.value : "near",
    events: cardsToModels(list),
    map: null,
  };

  // The number badge of each card becomes a "show on map" button (JS only).
  enhanceCards(list);
  if (sortSel) {
    sortSel.addEventListener("change", () => {
      if (state.mode === "here") {
        state.sort = sortSel.value;
        renderHere();
      } else if (sortSel.form) {
        sortSel.form.submit();
      }
    });
  }
  for (const seg of doc.querySelectorAll(".seg")) {
    seg.addEventListener("click", (ev) => {
      if (state.mode !== "here") return; // a normal link otherwise
      ev.preventDefault();
      for (const s of doc.querySelectorAll(".seg")) s.removeAttribute("aria-current");
      seg.setAttribute("aria-current", "true");
      state.span = seg.dataset.span;
      loadHere();
    });
  }
  if (locateBtn && typeof navigator !== "undefined" && navigator.geolocation) {
    locateBtn.hidden = false;
    locateBtn.addEventListener("click", async () => {
      status.textContent = "Finding your location…";
      locateBtn.disabled = true;
      try {
        state.here = await locate(navigator.geolocation);
      } catch (err) {
        status.textContent = locateMessage(err.reason, areaText);
        locateBtn.disabled = false;
        return;
      }
      locateBtn.disabled = false;
      state.mode = "here";
      for (const a of doc.querySelectorAll(".area-btn")) a.removeAttribute("aria-current");
      if (activeArea) activeArea.textContent = "Your location";
      await loadHere();
    });
  }

  // Public-transport time for the cards in view (from the ~200 m-rounded
  // position, two lookups at a time); walking only when it isn't quicker
  // or the planner doesn't answer.
  let transitObserver = null;
  function transitForCards(events) {
    if (transitObserver) transitObserver.disconnect();
    if (!state.here || typeof IntersectionObserver === "undefined" || typeof fetch !== "function") return;
    const from = transitOrigin(state.here);
    const queue = [];
    let active = 0;
    const pump = () => {
      while (active < 2 && queue.length) {
        const { art, id } = queue.shift();
        active++;
        fetch("/v1/transit?from=" + encodeURIComponent(from) + "&event=" + encodeURIComponent(id), {
          headers: { Accept: "application/json" },
        })
          .then((r) => (r.ok ? r.json() : null))
          .then((r) => {
            const suffix = transitSuffix(r);
            const walk = art.querySelector('[data-slot="walk"]');
            if (suffix && walk && walk.textContent) walk.textContent += suffix;
          })
          .catch(() => {})
          .finally(() => {
            active--;
            pump();
          });
      }
    };
    const byArt = new Map();
    for (const e of events) {
      if (typeof e.km !== "number" || e.km < MIN_TRANSIT_KM) continue;
      const art = list.querySelector("#ev-" + CSS.escape(e.id));
      if (art) byArt.set(art, e.id);
    }
    transitObserver = new IntersectionObserver(
      (entries) => {
        for (const en of entries) {
          if (!en.isIntersecting) continue;
          transitObserver.unobserve(en.target);
          queue.push({ art: en.target, id: byArt.get(en.target) });
        }
        pump();
      },
      { rootMargin: "100px" }
    );
    for (const art of byArt.keys()) transitObserver.observe(art);
  }

  async function loadHere() {
    status.textContent = "Loading what's on in London…";
    let raw;
    try {
      raw = await fetchLondon(fetch, state.span);
    } catch (e) {
      status.textContent = "Couldn't load events. Please try again.";
      return;
    }
    const nowIso = new Date().toISOString();
    state.all = byDistanceFrom(
      state.here,
      raw.map((e) => eventFromJson(e, nowIso))
    ).slice(0, LIST_MAX);
    status.textContent =
      "Sorted by walking distance from where you are. Public transport times use your position rounded to about 200 m.";
    renderHere();
  }

  function renderHere() {
    const nowIso = new Date().toISOString();
    const events = sortEvents(state.all, state.sort, nowIso).map((e, i) =>
      Object.assign({}, e, { n: i + 1 })
    );
    list.replaceChildren();
    for (const e of events) list.appendChild(renderCard(tpl, e));
    enhanceCards(list);
    transitForCards(events);
    const empty = doc.getElementById("near-empty");
    if (empty) empty.hidden = true;
    if (count) {
      const s = count.firstElementChild;
      s.replaceChildren(
        el("strong", null, events.length + (events.length === 1 ? " event" : " events")),
        doc.createTextNode(
          state.span === "today" ? " on now or later today, nearest to you" : " on now or starting in the next 3 hours, nearest to you"
        )
      );
      const order = slot(count, "order");
      if (order) order.textContent = state.sort === "soon" ? "Starting soonest" : "Nearest first";
    }
    state.events = events;
    doc.dispatchEvent(new Event("musenmingle:cards"));
    if (state.map) state.map.setEvents(events, state.here);
  }

  if (pane && supportsWebGL()) {
    pane.hidden = false;
    import(pane.dataset.maplibre)
      .then((maplibregl) => createMap(maplibregl, pane, state, list))
      .then((m) => {
        state.map = m;
        m.setEvents(state.events, state.here);
      })
      .catch(() => {
        pane.hidden = true;
      });
  }
}

function supportsWebGL() {
  try {
    const c = doc.createElement("canvas");
    return !!(c.getContext("webgl2") || c.getContext("webgl"));
  } catch (e) {
    return false;
  }
}

function cardsToModels(list) {
  const out = [];
  for (const a of list.querySelectorAll("article.near-card")) out.push(eventFromDataset(a.dataset));
  return out;
}

function renderCard(tpl, e) {
  const node = tpl.content.firstElementChild.cloneNode(true);
  const art = node.querySelector("article");
  art.id = "ev-" + e.id;
  const d = art.dataset;
  d.eventId = e.id;
  d.n = String(e.n);
  if (e.lat !== null) d.lat = String(e.lat);
  if (e.lng !== null) d.lng = String(e.lng);
  d.title = e.title;
  d.venue = e.venue;
  d.status = e.status;
  d.category = e.category;
  d.starts = e.starts_at || "";
  if (e.thumb) {
    d.thumb = e.thumb.src;
    d.creditName = e.thumb.credit.name;
    d.creditUrl = e.thumb.credit.url;
  }
  if (e.cta) {
    d.ctaUrl = e.cta.url;
    d.ctaName = e.cta.name;
  }
  const href = "/events/" + encodeURIComponent(e.id);
  slot(node, "n").textContent = String(e.n);
  slot(node, "walk").textContent = typeof e.km === "number" ? walkLabel(e.km) : "";
  const title = slot(node, "title");
  title.textContent = e.title;
  title.setAttribute("href", href);
  const venue = slot(node, "venue");
  if (e.venue) venue.textContent = e.venue;
  else venue.remove();
  slot(node, "status").textContent = e.status;
  if (e.price) {
    const p = slot(node, "price");
    p.textContent = e.price;
    if (e.free) p.classList.add("free");
    p.hidden = false;
    slot(node, "sep").hidden = false;
  }
  slot(node, "category").textContent = e.category;
  if (e.cta) {
    const cta = slot(node, "cta");
    cta.setAttribute("href", e.cta.url);
    cta.textContent = "See it on " + e.cta.name + " →";
    const vh = el("span", "vh", ": " + e.title);
    cta.appendChild(vh);
    cta.hidden = false;
  }
  const details = slot(node, "details");
  details.setAttribute("href", href);
  slot(node, "details-title").textContent = ": " + e.title;
  const btn = node.querySelector("button.save");
  btn.setAttribute("data-save-id", e.id);
  btn.setAttribute("data-title", e.title);
  btn.setAttribute("data-venue", e.venue);
  btn.setAttribute("data-starts", e.starts_at || "");
  btn.setAttribute("data-ends", e.ends_at || "");
  btn.setAttribute("data-all-day", e.all_day ? "true" : "false");
  slot(node, "save-title").textContent = ": " + e.title;
  return node;
}

function enhanceCards(list) {
  for (const art of list.querySelectorAll("article.near-card")) {
    const num = art.querySelector(".near-num");
    if (!num || num.tagName === "BUTTON" || !art.dataset.lat) continue;
    const b = el("button", "near-num");
    b.type = "button";
    b.textContent = art.dataset.n;
    b.setAttribute("aria-label", "Show on map: " + art.dataset.title);
    b.dataset.showOnMap = art.dataset.eventId;
    num.replaceWith(b);
  }
}

// ------------------------------------------------------------ map

async function createMap(maplibregl, pane, state, list) {
  const origin = location.origin;
  const dark = matchMedia("(prefers-color-scheme: dark)").matches;
  const protocol = new globalThis.pmtiles.Protocol();
  maplibregl.addProtocol("pmtiles", protocol.tile);
  // Our styles use paths; MapLibre needs absolute URLs for the tiles (on
  // our origin, versioned) and the glyphs.
  const r = await fetch(dark ? pane.dataset.styleDark : pane.dataset.styleLight);
  if (!r.ok) throw new Error("style HTTP " + r.status);
  const style = await r.json();
  style.glyphs = origin + pane.dataset.glyphs;
  style.sources.protomaps.url = "pmtiles://" + origin + pane.dataset.tiles;
  const center = [Number(pane.dataset.centerLng), Number(pane.dataset.centerLat)];
  const map = new maplibregl.Map({
    container: pane.querySelector("#map"),
    style,
    center,
    zoom: 13,
    minZoom: 9,
    maxZoom: 18,
    maxBounds: [
      [-0.75, 51.2],
      [0.55, 51.8],
    ],
    attributionControl: false,
    dragRotate: false,
    pitchWithRotate: false,
  });
  map.touchZoomRotate.disableRotation();

  const markers = new Map();
  let popup = null;
  let selected = null;
  let youMarker = null;
  let events = [];
  const fit = doc.getElementById("map-fit");

  map.on("load", () => {
    map.addSource("events", { type: "geojson", data: geojson([]), cluster: true, clusterRadius: 36, clusterMaxZoom: 16 });
    // Invisible layer so the clustered source is laid out; markers are DOM.
    map.addLayer({ id: "events-layout", type: "circle", source: "events", paint: { "circle-opacity": 0, "circle-radius": 1 } });
    map.getSource("events").setData(geojson(events));
  });
  map.on("sourcedata", (e) => {
    if (e.sourceId === "events" && e.isSourceLoaded) updateMarkers();
  });
  doc.addEventListener("keydown", (ev) => {
    if (ev.key === "Escape" && popup) popup.remove();
  });
  map.on("moveend", () => {
    updateMarkers();
    syncList();
  });

  for (const b of pane.querySelectorAll("[data-zoom]")) {
    b.addEventListener("click", () => (b.dataset.zoom === "in" ? map.zoomIn() : map.zoomOut()));
  }
  if (fit) fit.addEventListener("click", () => fitAll(true, true));
  list.addEventListener("click", (ev) => {
    const b = ev.target.closest && ev.target.closest("[data-show-on-map]");
    if (!b) return;
    select(b.dataset.showOnMap, true);
  });

  function geojson(evts) {
    return {
      type: "FeatureCollection",
      features: evts
        .filter((e) => e.lat !== null && e.lng !== null)
        .map((e) => ({
          type: "Feature",
          geometry: { type: "Point", coordinates: [e.lng, e.lat] },
          properties: { id: e.id, n: e.n },
        })),
    };
  }

  function updateMarkers() {
    if (!map.getSource("events")) return;
    const seen = new Set();
    for (const f of map.querySourceFeatures("events")) {
      const p = f.properties;
      const key = p.cluster ? "c" + p.cluster_id : "e" + p.id;
      if (seen.has(key)) continue;
      seen.add(key);
      if (markers.has(key)) continue;
      const b = el("button", p.cluster ? "map-marker cluster" : "map-marker");
      b.type = "button";
      if (p.cluster) {
        b.textContent = "\u00d7" + p.point_count;
        b.setAttribute("aria-label", p.point_count + " events here: zoom in");
        b.addEventListener("click", async () => {
          const zoom = await map.getSource("events").getClusterExpansionZoom(p.cluster_id);
          map.easeTo({ center: f.geometry.coordinates, zoom });
        });
      } else {
        const e = events.find((x) => x.id === p.id);
        b.textContent = String(p.n);
        b.setAttribute("aria-label", "Event " + p.n + (e ? ": " + e.title : ""));
        if (selected === p.id) b.classList.add("selected");
        b.addEventListener("click", () => select(p.id, false));
      }
      const m = new maplibregl.Marker({ element: b, anchor: "center" }).setLngLat(f.geometry.coordinates).addTo(map);
      markers.set(key, m);
    }
    for (const [key, m] of markers) {
      if (!seen.has(key)) {
        m.remove();
        markers.delete(key);
      }
    }
  }

  function clearMarkers() {
    for (const m of markers.values()) m.remove();
    markers.clear();
  }

  // Near the visitor, frame them and their nearest few; otherwise (or on
  // "Show all") every event.
  function fitAll(animate, all) {
    const located = events.filter((e) => e.lat !== null);
    const pts = (state.here && !all ? located.slice(0, 8) : located).map((e) => [e.lng, e.lat]);
    if (state.here) pts.push([state.here.lng, state.here.lat]);
    if (pts.length === 0) return;
    const b = new maplibregl.LngLatBounds(pts[0], pts[0]);
    for (const p of pts) b.extend(p);
    map.fitBounds(b, { padding: { top: 48, bottom: 48, left: 48, right: 80 }, maxZoom: 15, animate: !!animate });
  }

  function syncList() {
    // Cards whose event is outside the map view are hidden (phone and desktop).
    if (!events.length) return;
    const bounds = map.getBounds();
    let shown = 0;
    for (const art of list.querySelectorAll("article.near-card")) {
      const lat = Number(art.dataset.lat);
      const lng = Number(art.dataset.lng);
      const inView = art.dataset.lat === undefined || bounds.contains([lng, lat]);
      art.parentElement.hidden = !inView;
      if (inView) shown++;
    }
    if (fit) {
      fit.hidden = shown === events.length;
      fit.textContent = "Show all " + events.length + " (" + shown + " in view)";
    }
  }

  function select(id, fly) {
    const e = events.find((x) => x.id === id);
    if (!e || e.lat === null) return;
    selected = id;
    for (const [key, m] of markers) m.getElement().classList.toggle("selected", key === "e" + id);
    for (const art of list.querySelectorAll("article.near-card")) {
      art.classList.toggle("selected", art.dataset.eventId === id);
    }
    if (popup) popup.remove();
    // The popover opens above its marker, so the marker is eased into the
    // lower part of the map.
    const lift = Math.min(pane.clientHeight * 0.35, 220);
    popup = new maplibregl.Popup({ closeButton: false, closeOnClick: false, anchor: "bottom", offset: 18, maxWidth: "20rem", className: "near-pop" })
      .setLngLat([e.lng, e.lat])
      .setDOMContent(popover(e))
      .addTo(map);
    doc.dispatchEvent(new Event("musenmingle:cards"));
    if (fly) {
      map.easeTo({ center: [e.lng, e.lat], zoom: Math.max(map.getZoom(), 15), offset: [0, lift] });
      pane.scrollIntoView({ block: "nearest", behavior: "smooth" });
    } else {
      map.easeTo({ center: [e.lng, e.lat], offset: [0, lift] });
      const card = doc.getElementById("ev-" + id);
      if (card) {
        card.parentElement.hidden = false;
        card.scrollIntoView({ block: "nearest", behavior: "smooth" });
      }
    }
  }

  function popover(e) {
    const box = el("div", "pop");
    const head = el("div", "pop-head");
    head.appendChild(el("p", "pop-kicker", e.category));
    const close = el("button", "pop-close");
    close.type = "button";
    close.setAttribute("aria-label", "Close");
    close.textContent = "×";
    close.addEventListener("click", () => popup && popup.remove());
    head.appendChild(close);
    box.appendChild(head);
    const h = el("h3", null);
    const a = el("a", null, e.title);
    a.setAttribute("href", "/events/" + encodeURIComponent(e.id));
    h.appendChild(a);
    box.appendChild(h);
    if (e.venue) box.appendChild(el("p", "pop-venue", e.venue));
    const st = el("p", "near-status");
    st.appendChild(el("span", "dot"));
    st.appendChild(el("span", null, e.status));
    box.appendChild(st);
    if (e.thumb) {
      const fig = el("figure", "pop-thumb");
      const img = el("img");
      img.setAttribute("src", e.thumb.src);
      img.setAttribute("alt", "");
      img.setAttribute("width", String(e.thumb.w || 480));
      img.setAttribute("height", String(e.thumb.h || 270));
      fig.appendChild(img);
      const cap = el("figcaption", "credit", "Image: ");
      const ca = el("a", null, e.thumb.credit.name);
      ca.setAttribute("href", e.thumb.credit.url);
      ca.setAttribute("rel", "noopener");
      cap.appendChild(ca);
      fig.appendChild(cap);
      box.appendChild(fig);
    }
    // Our detail page is the main button (as on the event cards); the
    // venue's own page is the secondary link.
    const actions = el("div", "pop-actions");
    const det = el("a", "button", "Details");
    det.setAttribute("href", "/events/" + encodeURIComponent(e.id));
    actions.appendChild(det);
    // The card's Save toggle (web.js handles clicks and shows it).
    const card = doc.getElementById("ev-" + e.id);
    const save = card && card.querySelector("button.save[data-save-id]");
    if (save) {
      const b = save.cloneNode(true);
      b.hidden = true;
      actions.appendChild(b);
    }
    if (e.cta) {
      const cta = el("a", "arrow-link", "See it on " + e.cta.name + " →");
      cta.setAttribute("href", e.cta.url);
      cta.setAttribute("rel", "noopener");
      actions.appendChild(cta);
    }
    box.appendChild(actions);
    return box;
  }

  return {
    setEvents(evts, here) {
      events = evts;
      selected = null;
      if (popup) popup.remove();
      clearMarkers();
      if (youMarker) youMarker.remove();
      if (here) {
        const you = el("span", "map-you");
        you.setAttribute("role", "img");
        you.setAttribute("aria-label", "You are here");
        youMarker = new maplibregl.Marker({ element: you }).setLngLat([here.lng, here.lat]).addTo(map);
      }
      const src = map.getSource("events");
      if (src) src.setData(geojson(events));
      fitAll(false);
      if (!events.length) map.jumpTo({ center: here ? [here.lng, here.lat] : center, zoom: 13 });
    },
  };
}
