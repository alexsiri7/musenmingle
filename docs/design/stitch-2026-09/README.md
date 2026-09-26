# Muse & Mingle visual design (Google Stitch, September 2026)

The owner's design, exported from Google Stitch. The site implements it in
`src/web.css` (tokens as CSS custom properties) and `src/web.rs` (maud).

- `DESIGN.md` — the design system ("Gallery-Minimal White Cube"): colours,
  type scale, spacing, components. Follow it for any UI work.
- `*.png` — the Stitch screens: listing/home, event detail, calendar, map
  and the wordmark. `implemented/` has screenshots of our pages after the
  restyle (390 px and 1280 px), taken against a scratch database seeded from
  real listings, with generated stand-in images instead of venue thumbnails.

Rules when implementing a screen:

- **Plain CSS and our own assets only.** Stitch's export used CDN Tailwind,
  Google Fonts and remote images; we translate to `src/web.css`, fonts are
  self-hosted from `static/fonts/`, icons are inline SVG. The CSP stays strict.
- **The copy in the screens is placeholder, and much of it is untrue for us.**
  No "curated"/"editorial desk"/"hand-selected" claims (listings are gathered
  automatically from venues' own sites), no invented counts or percentages,
  no "archive"/"accession"/"REF" numbers, no accounts or avatars. "No ads, no
  sponsored listings" is true. AI text is always labelled "✨ AI".
- **Deliberate deviations:** tertiary text is `#666` (not `#767676`, which
  fails WCAG AA on `#f4f4f4`); focus rings are 2px cobalt (1px on inputs plus
  the cobalt border); the placeholder for a missing image shows only the
  category, venue and start day, never catalogue-style numbers; there is a
  dark colour scheme using the same tokens.
