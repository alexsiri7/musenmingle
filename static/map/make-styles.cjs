// Generates static/map/style-light.json and style-dark.json from the pinned
// @protomaps/basemaps package (see docs/map.md). Run from the repo root:
//
//   npm pack @protomaps/basemaps@5.7.2 && tar xzf protomaps-basemaps-5.7.2.tgz
//   node static/map/make-styles.cjs package/dist/cjs/index.cjs
//
// The styles are committed; this script is not run by the build. Changes
// made to the upstream layers: no sprite (the icon layers `roads_oneway` and
// `roads_shields` are dropped, and town dots lose their icon), glyphs from
// our own /static/map/fonts/, and the tiles from our own /tiles/london.pmtiles
// (map.mjs turns both into absolute URLs at run time).
"use strict";
const fs = require("node:fs");
const path = require("node:path");

const basemaps = require(path.resolve(process.argv[2]));
const DROP = new Set(["roads_oneway", "roads_shields"]);
const ICON_KEYS = /^icon-/;

function style(flavorName) {
  const layers = basemaps
    .layers("protomaps", basemaps.namedFlavor(flavorName), { lang: "en" })
    .filter((l) => !DROP.has(l.id))
    .map((l) => {
      if (!l.layout) return l;
      const layout = {};
      for (const [k, v] of Object.entries(l.layout)) if (!ICON_KEYS.test(k)) layout[k] = v;
      return { ...l, layout };
    });
  return {
    version: 8,
    name: "Muse & Mingle " + flavorName,
    glyphs: "/static/map/fonts/{fontstack}/{range}.pbf",
    sources: {
      protomaps: {
        type: "vector",
        url: "pmtiles:///tiles/london.pmtiles",
        attribution: "© OpenStreetMap contributors · Protomaps",
      },
    },
    layers,
  };
}

const out = path.join(__dirname);
for (const [file, flavor] of [["style-light.json", "grayscale"], ["style-dark.json", "black"]]) {
  fs.writeFileSync(path.join(out, file), JSON.stringify(style(flavor)) + "\n");
  console.log("wrote", file);
}
