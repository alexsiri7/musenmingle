---
name: Gallery-Minimal White Cube
colors:
  surface: '#f9f9f9'
  surface-dim: '#dadada'
  surface-bright: '#f9f9f9'
  surface-container-lowest: '#ffffff'
  surface-container-low: '#f3f3f3'
  surface-container: '#eeeeee'
  surface-container-high: '#e8e8e8'
  surface-container-highest: '#e2e2e2'
  on-surface: '#1a1c1c'
  on-surface-variant: '#444657'
  inverse-surface: '#2f3131'
  inverse-on-surface: '#f0f1f1'
  outline: '#747689'
  outline-variant: '#c4c5da'
  surface-tint: '#1941ff'
  primary: '#0028c2'
  on-primary: '#ffffff'
  primary-container: '#0038ff'
  on-primary-container: '#c8ceff'
  inverse-primary: '#bbc3ff'
  secondary: '#5f5e5e'
  on-secondary: '#ffffff'
  secondary-container: '#e5e2e1'
  on-secondary-container: '#656464'
  tertiary: '#414242'
  on-tertiary: '#ffffff'
  tertiary-container: '#595959'
  on-tertiary-container: '#d1d0d0'
  error: '#ba1a1a'
  on-error: '#ffffff'
  error-container: '#ffdad6'
  on-error-container: '#93000a'
  primary-fixed: '#dee0ff'
  primary-fixed-dim: '#bbc3ff'
  on-primary-fixed: '#000e5e'
  on-primary-fixed-variant: '#002cce'
  secondary-fixed: '#e5e2e1'
  secondary-fixed-dim: '#c8c6c5'
  on-secondary-fixed: '#1c1b1b'
  on-secondary-fixed-variant: '#474646'
  tertiary-fixed: '#e4e2e2'
  tertiary-fixed-dim: '#c7c6c6'
  on-tertiary-fixed: '#1b1c1c'
  on-tertiary-fixed-variant: '#464747'
  background: '#f9f9f9'
  on-background: '#1a1c1c'
  surface-variant: '#e2e2e2'
typography:
  display:
    fontFamily: Hanken Grotesk
    fontSize: 48px
    fontWeight: '400'
    lineHeight: 52px
    letterSpacing: -0.03em
  display-mobile:
    fontFamily: Hanken Grotesk
    fontSize: 34px
    fontWeight: '400'
    lineHeight: 38px
    letterSpacing: -0.025em
  headline-lg:
    fontFamily: Hanken Grotesk
    fontSize: 32px
    fontWeight: '500'
    lineHeight: 38px
    letterSpacing: -0.02em
  headline-lg-mobile:
    fontFamily: Hanken Grotesk
    fontSize: 26px
    fontWeight: '500'
    lineHeight: 32px
    letterSpacing: -0.015em
  headline-md:
    fontFamily: Hanken Grotesk
    fontSize: 22px
    fontWeight: '500'
    lineHeight: 28px
    letterSpacing: -0.015em
  headline-sm:
    fontFamily: Hanken Grotesk
    fontSize: 17px
    fontWeight: '600'
    lineHeight: 24px
    letterSpacing: -0.01em
  body-lg:
    fontFamily: Hanken Grotesk
    fontSize: 16px
    fontWeight: '400'
    lineHeight: 26px
    letterSpacing: -0.005em
  body-md:
    fontFamily: Hanken Grotesk
    fontSize: 14px
    fontWeight: '400'
    lineHeight: 22px
    letterSpacing: 0em
  body-sm:
    fontFamily: Hanken Grotesk
    fontSize: 12px
    fontWeight: '400'
    lineHeight: 18px
    letterSpacing: 0em
  label-caps:
    fontFamily: Hanken Grotesk
    fontSize: 11px
    fontWeight: '600'
    lineHeight: 14px
    letterSpacing: 0.12em
  mono-meta:
    fontFamily: JetBrains Mono
    fontSize: 12px
    fontWeight: '400'
    lineHeight: 16px
    letterSpacing: -0.01em
  mono-meta-sm:
    fontFamily: JetBrains Mono
    fontSize: 11px
    fontWeight: '400'
    lineHeight: 14px
    letterSpacing: 0em
spacing:
  gutter: 1.5rem
  gutter-mobile: 1rem
  margin: 2.5rem
  margin-mobile: 1.25rem
  space-xs: 0.25rem
  space-sm: 0.5rem
  space-md: 1rem
  space-lg: 1.5rem
  space-xl: 2.5rem
---

## Brand & Style

This design system embodies the serene discipline of an art gallery monograph and exhibition catalogue. Engineered for curating high-caliber cultural happenings across London, it strips away gratuitous ornamentation in favor of architectural clarity, razor-sharp 1px hairlines, and purposeful typographic cadence.

### Brand Personality
- **Curated & Authoritative:** Discriminating taste delivered with institutional restraint.
- **Architectural & Serene:** Ample whitespace evoking white cube exhibition walls and heavy cotton archival stock.
- **Subtle Precision:** High-utility informational density balanced with expansive editorial breathing room.

### Emotional Response
Users should feel unhurried, focused, and culturally informed—experiencing the interface not as an intrusive promotional feed, but as an indispensable index of metropolitan artistic life.

### Design Movement
**Gallery-Minimalism with Modernist Swiss Utility:** Strict 1px hairline segmentations, rigorous typographic hierarchy, zero drop shadows, and an uncompromising reliance on proportional spacing, structural borders, and purposeful contrast.

## Colors

The palette is strictly calibrated around white gallery walls, archival ink, and an electric cobalt spark reserved solely for interactive feedback, live statuses, and focused states.

### Primary Roles
- **Canvas Base (`#ffffff`):** Pure exhibition white. Primary surface for panels, active cards, and modal sheets.
- **Canvas Off-White (`#fafafa`):** Wall wash. Page-level canvas background providing subtle contrast against pure white cards.
- **Surface Muted (`#f4f4f4`):** Secondary structural fill for image fallbacks, pill fills, and tabular row striping.
- **Hairline Border (`#e5e5e5`):** Structural divider for all horizontal bands, metadata grids, and card perimeters.
- **Hairline Border Subdued (`#f0f0f0`):** Secondary inner separators and micro-dividers.

### Text & Ink Roles
- **Text Ink Primary (`#111111`):** Deep charcoal-black for headlines, venue titles, and primary action text. Exceeds WCAG AAA.
- **Text Ink Secondary (`#555555`):** Mid-tone neutral for editorial descriptions, curators' notes, and body copy.
- **Text Ink Tertiary / Mono Meta (`#767676`):** De-emphasized neutral for timestamps, postal codes, and uppercase category flags.

### Accent Roles
- **Electric Cobalt (`#0038ff` / active state `#1d4ed8`):** Reserved for live statuses, focused inputs, active date filters, bookmark active fills, and precise hyperlink highlights. Must never dominate large background fills.

## Typography

The typographic strategy pairs `Hanken Grotesk` (clean, neo-grotesque, neutral proportions echoing contemporary gallery signage) with `JetBrains Mono` for metadata, coordinates, and temporal anchors.

### Hierarchical Rules
- **Display & Headlines:** Set in `Hanken Grotesk` with tight negative tracking (`-0.03em` to `-0.015em`) and strict proportional line-heights. They mimic architectural cut-sheet headings.
- **Section Eyebrows & Badges:** Use `label-caps` (`Hanken Grotesk`, uppercase, `0.12em` letter-spacing) to categorize event genres (e.g., `TALK`, `EXHIBITION`, `PRIVATE VIEW`, `SYMPOSIUM`).
- **Dates, Times & London Postcodes:** Must consistently use `mono-meta` or `mono-meta-sm` (`JetBrains Mono`). This establishes an unyielding technical rhythm for indexing locations (e.g., `E2 7DD`, `18:30—21:00 GMT`, `RUNS UNTIL 24.11`).

## Layout & Spacing

Layouts conform to a strict structural grid governed by explicit hairline delimiters instead of card floating islands.

### Grid & Composition
- **Desktop (1200px+):** 12-column fluid grid, 2.5rem outer canvas margin, 1.5rem gutter. Max-width constrained to 1440px for editorial legibility.
- **Tablet (768px - 1199px):** 8-column grid with 1.75rem margins and 1.25rem gutters.
- **Mobile (< 768px):** 4-column single-column stack with 1.25rem margins and 1rem gutter.

### Structural Flow
- Sections are bounded by 1px top and bottom borders (`#e5e5e5`).
- Adjacent event listings form an uninterrupted tabular index: shared hairline borders prevent doubling line weight.
- Spacing inside data cells relies strictly on `space-md` (vertical padding) and `space-lg` (horizontal cell breathing space).

## Elevation & Depth

This design system is intentionally **flat, planar, and shadow-free**. Visual hierarchy is achieved exclusively through hairline linear boundaries, typographic scale, and subtle planar contrast.

### Planar Tiers
- **Tier 0 (Background Canvas):** `#fafafa` creates an expansive backdrop.
- **Tier 1 (Surface Sheets & Cards):** `#ffffff` sharply demarcated by `1px solid #e5e5e5`. No blur, no soft drop shadow.
- **Tier 2 (Modals & Drawers):** `#ffffff` surrounded by `1px solid #111111` or `#e5e5e5`. Overlays use an understated, translucent white veil (`rgba(255, 255, 255, 0.85)` with `backdrop-filter: blur(8px)`).
- **Tier 3 (Popovers & Tooltips):** Solid `#111111` with crisp white text, anchored cleanly without diffuse shadows.

## Shapes

The shape language is strictly **Sharp (0px radius)**, adhering to architectural, gallery monograph, and print catalogue conventions.

- **Buttons, inputs, cards, image containers, badges, and modals:** Must have `border-radius: 0px`.
- Crisp edge junctions reflect geometric confidence and reinforce structural hairline grid alignments.
- Subtle inner tags or status dots may use pure circular geometry (e.g., a 6px `border-radius: 50%` indicator for "Open Today"), but all structural containers remain rigidly orthogonal.

## Components

### Buttons
- **Primary:** `#111111` solid fill, `#ffffff` text, 0px border radius, 44px min height. Text is medium-weight with subtle letter-spacing. On hover, background shifts to `#0038ff`.
- **Secondary / Outline:** Pure `#ffffff` background with 1px border in `#111111`, `#111111` text. On hover, background becomes `#f4f4f4`.
- **Tertiary / Utility:** Transparent background, underlined or paired with an arrow glyph (`→`), shifting to `#0038ff` on hover.

### Chips & Category Filters
- Rectilinear frame (`1px solid #e5e5e5`), `#ffffff` background, `label-caps` typography.
- **Active State:** Background flips to `#111111` with `#ffffff` text, or displays a left 2px border accent in `#0038ff`.

### Event Card & List Item
- Shared hairline perimeter (`1px solid #e5e5e5`).
- Structured in modular zones: left column date & location in `mono-meta`, center title and description in `Hanken Grotesk`, right column action/status flag.
- **Hover State:** Background subtly transitions to `#fafafa` with zero elevation jump; title color shifts to `#0038ff`.

### Image Fallback Box (Monograph Blank)
- When no image is present, the layout displays a clean `#f4f4f4` canvas bounded by `1px solid #e5e5e5`.
- Contains centered, faint geometric coordinates or catalog accession numbering (e.g., `MM-LON-2025 // [IMAGE ARCHIVED]`) in `mono-meta-sm` (`#767676`). Never show broken image icons.

### Inputs & Search Bars
- Background: `#ffffff`, border: `1px solid #e5e5e5`, text: `#111111`.
- Height: 44px, sharp corners, mono placeholder styling.
- **Focus State:** 1px perimeter instantly renders in `#0038ff` without diffuse outline halos.

### Checkboxes & Radios
- Sharp 16px square boxes with a 1px border (`#111111`).
- Checked state: solid `#111111` fill with an inset `#ffffff` check or square node. Never rounded.

### Coordinate & Status Indicator
- A live event badge containing an active 6px circle pulse in `#0038ff` alongside `mono-meta` text (e.g., `● LIVE TODAY // SHOREDITCH`).