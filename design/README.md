# Portfel — UI design

The interface is built **to match this design**. When code and design disagree, the design wins unless the PR explains why.

## Files
| Path | What it is |
|---|---|
| `screens/*.png` | Visual reference for every screen (light, dark, phone, states, components). Start here. |
| `portfel-design.pdf` | The same screens in full resolution. |
| `portfel-interactive.html` | The clickable prototype, opens offline in a browser. |
| `reference/NN-*.html` | Source markup of each screen: exact inline styles, SVG icons, and a `<script type="text/x-dc">` block with the demo logic (formatting, chart geometry, top-up maths). `sc-for` / `sc-if` map to askama `{% for %}` / `{% if %}`; `{{...}}` are the values the server must provide. Not meant to render standalone. |
| `tokens.css` | Colour tokens (light + dark), base styles and the ≤760px responsive rules, copied verbatim. Use as the base stylesheet. |

## Foundations
- **Font:** Geist 400/500/600 (Google Fonts). Tabular figures everywhere (`font-variant-numeric: tabular-nums`).
- **Type scale:** hero 64/500, letter-spacing −3.5% (44px on phone) · figure 24/500 · title 15/600 · body 14/400 · caption 12/400.
- **Theme:** tokens are CSS variables on `.app`, swapped by `data-theme="light|dark"`. Default follows `prefers-color-scheme`; the sidebar toggle overrides it and is remembered in a cookie so the server renders the right theme with no flash.
- **One accent** (`--accent`), stepped shades `--accent-2/3` for the three asset classes (VWCE, Crypto, Gold).
- **Numbers:** Polish format `11 500,00 zł` with a non-breaking space as thousands separator and before `zł`; minus is `−` (U+2212); percentages one decimal (`+14,0%`), drift in `pp`.
- **Gain / loss** use `--gain` / `--loss` plus a sign and ▲/▼ arrow, never colour alone. **Drift from target is neutral grey**: being over or under is not good or bad.

## Layout
- Desktop: left sidebar 224px (logo, nav: Overview, Allocation, IKZE, Gold, Import; footer: price sync status and theme toggle), main column with ~56px side padding.
- ≤760px: sidebar becomes a top bar with horizontally scrolling nav pills; sync status shrinks to a dot + time next to the theme icon. See `screens/03-overview-phone.png`.
- Cards: `--surface`, 1px `--line` border, ~16px radius, `--shadow`.

## Screens → backlog
| Screen | Task | Notes |
|---|---|---|
| Shell, tokens, components | P28 | Base layout used by every page. |
| Overview | P20, P21 | Period switch 1M/6M/YTD/1Y/All re-renders hero, tiles and chart via htmx. Chart: value line + area, stepped contributions, optional dashed "all-in VWCE" benchmark with the "You're +X zł ahead" note. |
| Allocation | P22 | Actual vs dashed target bars, drift table incl. BTC/ETH inside crypto, top-up plan with amount input, quick chips (942 / 2 000 / 5 000 zł), "Include paused assets", after-top-up bar, IKZE room note. |
| IKZE | P23 | Limit progress bar (remaining hatched), December reminder banner with a suggested split of the remaining amount, 12%/32% deduction toggle, contributions-by-month bars with dashed "needed to max out" months. |
| Gold | P29 | Summary tiles, coins table, inline add form with validation; "Purchases paused" badge. Restyles the existing page. |
| Import | P24 | 3-step flow Upload → Review → Done; preview table with New vs Already imported, toggle to show duplicates. |
| States | P30 | Empty, loading skeleton, stale price badge, price source down banner with Retry, chart with one data point, empty import drop zone. Each is a partial the server swaps in. |

## Charts
Charts are **server-rendered inline SVG** (the reference computes paths on a 1000×300 viewBox with "nice" y-ticks — port that geometry to Rust). This matches the design exactly and needs no JS. Chart.js is not used for these screens.
