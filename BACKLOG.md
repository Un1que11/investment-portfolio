# Backlog — Portfolio

## Phase 1 — skeleton
- [x] P1. Cargo workspace: crates `core` (models and calculations), `web` (Axum), `sync` (sync binary). README with how to run.
- [x] P2. SQLite schema and migrations: accounts, assets, transactions (buy/sell/deposit/fee/dividend), prices, fx_rates, snapshots. Decimal everywhere.
- [x] P3. [light] `config.toml`: target shares, IKZE limits per year, tax rate, gold buyback factor. Validation: shares add up to 100%.
- [ ] P4. [light] GitHub Actions: fmt + clippy + test on every PR.

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
- [ ] P18. IKZE: contributions this year, remaining limit, estimated deduction.
- [ ] P19. Benchmark "everything in VWCE".

## Phase 4 — interface
- [ ] P20. Home page: total, P&L, XIRR, chart of value vs contributions.
- [ ] P21. Asset cards with the "earned more / less" ranking.
- [ ] P22. Allocation block: actual vs target, deviations, top-up calculator.
- [ ] P23. IKZE block with a limit progress bar and a reminder in December.
- [ ] P24. XTB import page.

## Phase 5 — automation
- [ ] P25. `portfolio-sync`: daily snapshot of prices and value; launchd plist + installation instructions.
- [ ] P26. Optional: monthly summary in Telegram (the same bot as the F1 digest).
