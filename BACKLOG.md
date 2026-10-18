# Backlog — Portfolio

## Phase 1 — skeleton
- [x] P1. Cargo workspace: crates `core` (models and calculations), `web` (Axum), `sync` (sync binary). README with how to run.
- [x] P2. SQLite schema and migrations: accounts, assets, transactions (buy/sell/deposit/fee/dividend), prices, fx_rates, snapshots. Decimal everywhere.
- [x] P3. [light] `config.toml`: target shares, IKZE limits per year, tax rate, gold buyback factor. Validation: shares add up to 100%.
- [x] P4. [light] GitHub Actions: fmt + clippy + test on every PR.

## Phase 2 — data
- [x] P5. NBP exchange rates (table A) with a DB cache and weekend handling (the last published rate is used).
- [x] P6. VWCE price (Stooq, EUR) with a cache; conversion to PLN at the NBP rate.
- [x] P7. BTC/ETH prices from the Kraken public Ticker.
- [x] P27. VWCE history older than ~100 trading days: Alpha Vantage weekly closes (`TIME_SERIES_WEEKLY`) for dates outside the daily cache, without overwriting daily closes. Needed for XIRR (P14) and the benchmark (P19).
- [x] P8. NBP gold price per gram; valuation of a 1/10 oz coin.
- [x] P9. Manual gold entry: a form to add a coin, a list, deletion.
- [x] P10. XTB xlsx import: parser for the operations and contributions history, idempotent by operation hash. Needs a sample export from Misha — until then, build the parser on a synthetic fixture and mark it `BLOCKED: need an anonymised sample`.
- [x] P11. Kraken private API (read-only): ledger and trades, incremental sync.

## Phase 3 — calculations (all in `core`, all with tests)
- [x] P12. Positions and value on a date; net contributions.
- [x] P13. P&L absolute and in % per asset and for the portfolio.
- [x] P14. XIRR for the portfolio and per asset.
- [x] P15. Each asset's contribution to the total profit, and a ranking.
- [x] P16. Deviation from target (pp and PLN), including the BTC/ETH split within crypto.
- [x] P17. Top-up calculator: amount N → a purchase plan without selling.
- [x] P18. IKZE: contributions this year, remaining limit, estimated deduction.
- [x] P19. Benchmark "everything in VWCE".

## Phase 4 — interface (build to `design/`, see design/README.md)
- [x] P28. Design system base: serve `design/tokens.css` (+ Geist), base layout with the 224px sidebar (nav, sync status footer, theme toggle), phone top bar ≤760px, theme default from `prefers-color-scheme` overridden by a cookie, askama partials for the shared components (number tile, delta badge, progress bar, allocation bar, asset card, table row, buttons, inputs) matching `screens/09-components.png`. Number formatting helpers (Polish format, U+2212 minus, signed values) with tests.
- [x] P31. Config: a `paused` flag per asset class (crypto and gold paused now); the top-up calculator (P17) skips paused assets unless `include_paused`, and reports "would need N zł" for them. Check P17's split matches the design's rule (pro rata to need; any surplus by target weight) and fix or explain differences.
- [x] P32. Period views: P&L, contributions and XIRR for 1M / 6M / YTD / 1Y / All (XIRR shown as "—" when the period is too short to be meaningful), and the benchmark gap "You're +X zł ahead of all-in VWCE" for each period.
- [x] P20. Overview (`screens/01–03`): hero total with P&L badge, XIRR and Contributed tiles, period switch via htmx, SVG chart (value + area, stepped contributions, toggleable dashed benchmark). Depends on P28, P32.
- [ ] P21. Asset cards on Overview: Gold, VWCE, Bitcoin, Ether ranked by profit with #rank, value, sparkline, invested, P&L, share of profit bar; phone variant as in `screens/03`. Depends on P20.
- [ ] P22. Allocation (`screens/04`): actual vs dashed target bars, drift table with BTC/ETH sub-rows and neutral drift bars, top-up plan panel (amount input with htmx, chips 942 / 2 000 / 5 000 zł, include paused, after-top-up bar, remaining IKZE room note). Depends on P28, P31.
- [ ] P23. IKZE (`screens/05`): limit progress bar with hatched remainder, December reminder banner with days left and a suggested split of the remaining limit over the remaining months, 12%/32% deduction toggle, contributions-by-month SVG bars with dashed "needed to max out" months. Depends on P28.
- [ ] P29. Gold page restyle (`screens/06`): summary tiles, coins table, inline add form with field validation as in the components sheet, "Purchases paused" badge. Depends on P28, P31.
- [ ] P24. XTB import (`screens/07`): Upload → Review → Done steps, preview table (New vs Already imported, toggle), confirm "Add N operations", Cancel. Depends on P28.
- [ ] P30. States (`screens/08`): empty overview with Import / Add a coin actions, loading skeleton, stale price badge and "source unavailable" banner with Retry, single-point chart, empty import drop zone, sidebar sync status variants. Depends on P20.

## Phase 5 — automation
- [ ] P25. `portfolio-sync`: daily snapshot of prices and value; launchd plist + installation instructions.
- [ ] P26. Optional: monthly summary in Telegram (the same bot as the F1 digest).

## Ideas (proposed by night job)
- Top-up plan in buyable units: gold in whole 1/10 oz coins at the current coin price, with what is left over re-split between the other assets. Reason: P17's plan can say "gold 417 PLN" while a coin costs several times that, so its gold line cannot be bought as is.
- (S) Kraken balance reconciliation: after each sync, compare the `Balance` endpoint per asset with the positions derived from the stored ledger and report any mismatch. Reason: `Balance` is already fetched but unused; a ledger type the mapper misses (staking, dust conversion, airdrop) would otherwise skew value and XIRR silently.
- (S) Database backup in `portfolio-sync`: a daily `VACUUM INTO` copy of the SQLite file with the last N kept, path in the config. Reason: hand-entered coins and imported XTB history exist only in that file and cannot be re-fetched from any API.
- (S) Gold purchase premium per coin: price paid vs NBP melt value on the purchase date, and today's buyback spread, shown separately from the gold price move. Reason: gold's P&L starts negative by the dealer premium, and without the split the "earned the least" ranking mixes price moves with buying costs.
