# investment-portfolio

A personal investment dashboard. It runs locally on macOS and shows the whole portfolio in PLN:
invested vs current value, P&L, XIRR, each asset's contribution, and drift from the target
allocation. See `CLAUDE.md` for the full spec and `BACKLOG.md` for the roadmap.

## Requirements

- Rust stable (install via [rustup](https://rustup.rs)); the minimum supported version is set
  in `Cargo.toml` (`rust-version`)

## Workspace

| Crate | Path | Purpose |
|---|---|---|
| `portfolio-core` | `crates/core` | Domain models and calculations (pure, no I/O) |
| `portfolio-db` | `crates/db` | SQLite storage: connection, migrations, decimal codec, caches |
| `portfolio-sources` | `crates/sources` | External market data (read-only): NBP FX rates, VWCE, BTC/ETH prices |
| `portfolio-web` | `crates/web` | Axum web dashboard (binary `portfolio-web`) |
| `portfolio-sync` | `crates/sync` | Background sync binary (`portfolio-sync`), run by launchd |

## Database

SQLite via sqlx. The schema lives in `migrations/` and is embedded into the binaries, which
apply pending migrations on startup (`portfolio_db::open`). Money, quantities and rates are
stored as exact decimal strings (`rust_decimal`), never as floats; the schema rejects anything
else. Database files (`*.db`) are git-ignored.

To add a migration, create `migrations/<YYYYMMDDHHMMSS>_<name>.sql`. Never edit a migration
that has already been applied to a real database.

## Market data

- **FX rates:** NBP table A mid rates (PLN per unit), cached in `fx_rates`. For a weekend or
  holiday the last table published before that day applies. Resolved past days are cached in
  `fx_rate_days`, so they need no network; today is re-checked until NBP publishes (around noon).
  Live check: `cargo run -p portfolio-sources --example fx_rate -- EUR 2026-10-11`
- **VWCE:** daily closes in EUR from Alpha Vantage (`VWCE.DEX`, Xetra), cached in `prices`,
  converted to PLN with the NBP rate of the close's trading day. Stooq, the original choice,
  now puts its CSV downloads behind a JavaScript bot check. Needs a free key: copy
  `.env.example` to `.env` and set `ALPHAVANTAGE_API_KEY`. The free tier allows ~25 requests a
  day, so an instrument is refetched at most every 6 hours, and rejects quick bursts, so the
  client waits 5 s between requests.
  Live check: `cargo run -p portfolio-sources --example vwce_price -- 2026-10-09`
  Older history: free daily output covers only ~100 trading days, so `backfill_weekly` stores
  week-end closes from `TIME_SERIES_WEEKLY` (source `alphavantage_weekly`, back to 2019-08)
  for days without a daily close. A day inside a backfilled week uses the previous week-end's
  close. Live check: `cargo run -p portfolio-sources --example vwce_backfill -- 2024-03-13`
- **BTC / ETH:** Kraken public API, no key. Kraken has no PLN markets, so prices are taken in
  EUR (`XXBTZEUR`, `XETHZEUR`) and converted with NBP. Live value: `Ticker` last trade with
  today's NBP rate. Daily closes: `OHLC` daily candles (UTC days; the unfinished day is
  skipped), cached in `prices`.
  Live check: `cargo run -p portfolio-sources --example crypto_prices -- 2026-10-11`

## Run

```bash
cargo run -p portfolio-web
```

The dashboard listens on <http://127.0.0.1:3000> (loopback only). To use a different address:

```bash
PORTFOLIO_ADDR=127.0.0.1:8080 cargo run -p portfolio-web
```

Health check: `curl http://127.0.0.1:3000/health` returns `ok`.

```bash
cargo run -p portfolio-sync
```

## Checks

These must all pass before a PR is opened:

```bash
cargo fmt --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```
