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
| `portfolio-xtb` | `crates/xtb` | XTB xStation export (xlsx) parser and idempotent import |
| `portfolio-sources` | `crates/sources` | External market data (read-only): NBP FX rates and gold, VWCE, BTC/ETH prices |
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
- **Gold:** NBP price per gram of fine gold (PLN, business days), cached in `prices` as asset
  `XAU_G`. A 1/10 oz coin (3.11 g) is valued at price x weight x buyback factor, or at a manual
  dealer quote when one is entered.
  Live check: `cargo run -p portfolio-sources --example gold_coin -- 2026-10-11 0.95`

## XTB import

XTB has no API since 14.03.2025; the account history export from xStation (History → export
to Excel) is the source. Import it into the local database:

```bash
cargo run -p portfolio-xtb --example import -- ~/Downloads/IKZE_....xlsx
```

Re-importing the same or an overlapping export is safe: operations already imported are
skipped (identified by a hash of their contents, since the export has no per-operation id).
Operation types not supported yet (e.g. fees, taxes, sales) make the import fail with their
names rather than be skipped silently. To turn a new export into a test fixture, anonymise it
with `tools/anonymise_xtb_export.py` and check the result before committing it.

## Kraken sync

Read-only sync of the Kraken account via the private API (`Balance` and `Ledgers` only; the
client has no trading or withdrawal calls). Create a key with **query** permissions only and put
it in `.env` as `KRAKEN_API_KEY` and `KRAKEN_PRIVATE_KEY` (see `.env.example`). Then:

```bash
cargo run -p portfolio-sources --example kraken_sync
```

Ledger entries are stored raw in `kraken_ledger` (incremental: each run fetches only entries
newer than the last one) and mapped into transactions: EUR deposits (also card purchases held as
`EUR.HOLD`), instant Buy (`spend` + `receive`) as buys, ETH staking rewards as income in kind
(net of Kraken's commission), fees as separate rows. Bundle dissolves are internal moves and are
skipped. Entry types not supported yet are kept raw and listed by the sync.

## Portfolio report

Holdings, net contributions and value in PLN on a date, from everything imported and synced:

```bash
cargo run -p portfolio-sources --example portfolio -- 2026-10-14 0.95
```

(arguments: date, default today; gold buyback factor, default 1 until `config.toml`). Net
contributions are money put in from outside, at the NBP rate of each flow's date: deposits minus
withdrawals for XTB and Kraken, coin purchases for gold. Value is every position at that day's
price plus the cash left in each account; positions without a price are listed, not guessed.

P&L is value minus net contributions (and as a % of them). Per asset it is value minus cost
(buys plus that asset's fees) plus sale proceeds and cash dividends, each at the NBP rate of its
own date, so currency moves since purchase are part of an asset's result; % is P&L over cost.
What assets do not explain (cash, currency moves on cash, account fees) is shown separately, so
the parts add up to the total.

XIRR is the annual return that accounts for when money went in: the same flows, dated (money
put in negative, the value on the date as the last flow), solved Actual/365 like Excel's XIRR.
For the portfolio the flows are the contributions; per asset, buys and fees, sales and cash
dividends. Assets held only from staking rewards cost nothing and have no XIRR. Over a short
period the annualised figure exaggerates, so the report prints the date it starts from.

## Run

```bash
cargo run -p portfolio-web
```

The dashboard listens on <http://127.0.0.1:3000> (loopback only). To use a different address:

```bash
PORTFOLIO_ADDR=127.0.0.1:8080 cargo run -p portfolio-web
```

Health check: `curl http://127.0.0.1:3000/health` returns `ok`.

The database defaults to `portfolio.db` in the working directory; set `PORTFOLIO_DB_URL`
(e.g. `sqlite:///Users/me/portfolio/portfolio.db`) to keep it elsewhere.

Pages:
- `/gold`: add, list and delete physical gold coins (purchase date, price in PLN, fine gold
  weight, optional manual quote and label). Amounts may be typed with a decimal comma.

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
