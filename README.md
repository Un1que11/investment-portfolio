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
- **A source that is down:** today's value then uses the latest stored close of the last 10
  days (`prices::close_or_last_known`), and the report lists the asset in `last_known` with
  when that close was fetched. Past days are never filled this way. With no close that recent
  the asset has no value, as before.

## XTB import

XTB has no API since 14.03.2025; the account history export from xStation (History → export
to Excel) is the source. Upload it on `/import` (see below), or import it from the command line:

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

(arguments: date, default today; gold buyback factor, default from `config.toml`). Net
contributions are money put in from outside, at the NBP rate of each flow's date: deposits minus
withdrawals for XTB and Kraken, coin purchases for gold. Value is every position at that day's
price plus the cash left in each account; positions without a price are listed, not guessed.

P&L is value minus net contributions (and as a % of them). Per asset it is value minus cost
(buys plus that asset's fees) plus sale proceeds and cash dividends, each at the NBP rate of its
own date, so currency moves since purchase are part of an asset's result; % is P&L over cost.
What assets do not explain (cash, currency moves on cash, account fees) is shown separately, so
the parts add up to the total.

Each asset's contribution is its P&L in PLN, ranked from the asset that earned the most to the
one that earned the least (fully sold assets included, unpriced ones listed apart). It is also
shown as a share of the total P&L and in percentage points of the portfolio's P&L %; with the
unexplained rest, both add up to the whole. When the portfolio is at a loss, the shares are of
the loss.

XIRR is the annual return that accounts for when money went in: the same flows, dated (money
put in negative, the value on the date as the last flow), solved Actual/365 like Excel's XIRR.
For the portfolio the flows are the contributions; per asset, buys and fees, sales and cash
dividends. Assets held only from staking rewards cost nothing and have no XIRR. Over a short
period the annualised figure exaggerates, so the report prints the date it starts from.

The benchmark answers "what if every contribution had gone into VWCE instead": the same net
contributions, each on its own date, buy (or, for withdrawals, sell) VWCE units at that day's
price in PLN (the Xetra close of the day or the last trading day before it, at the NBP rate of
that close's day), and the units are valued on the report date. It shows the benchmark's value,
P&L and XIRR, and how far the portfolio is ahead of it (negative: behind). It assumes no fees,
fractional units and no cash waiting to be invested. For contribution dates older than the
~100 days of daily closes, the first such report fetches VWCE's weekly history once (one or two
Alpha Vantage requests) and stores it; those dates then use the week-end close before them. A
withdrawal larger than the benchmark was worth on its day is reported instead of a result.

The periods block repeats P&L, net contributions, XIRR and the gap to all-in VWCE for each period
of the Overview's switch, ending on the report date: 1M, 6M and 1Y start at the end of the same
day one, six or twelve months earlier (the month's last day when it is shorter), YTD at the end
of 31 December, and All at the first contribution. The value at the start counts as money put in
on the start day, so a period's P&L is end value minus start value minus the period's own
contributions (% of start value plus contributions), its XIRR uses the start value as the first
flow, and its benchmark puts the start value into VWCE on the start day too. XIRR is shown as
"-" for a period shorter than 3 months, counted from the start day, or from the first
contribution when nothing was held at the start: annualising less than a quarter mostly
magnifies noise, and the Overview says "Needs 3+ months of data". Start values are priced like
the home chart's points, through the same caches; a start day with nothing held needs no price.

The allocation compares the assets' values with the targets in `config.toml`: each class
(VWCE, crypto, gold) as a share of the three together, and BTC and ETH as shares of the crypto.
Cash left in accounts belongs to no class and is left out. Deviations are actual minus target,
in percentage points and in PLN (positive = overweight), so per class they add up to zero; the
PLN deviation within crypto is what would have to move between BTC and ETH. Without a price for
every held asset there is no allocation.

A third argument, an amount in PLN, adds a top-up plan: how to split that much new money between
VWCE, BTC, ETH and gold to get closer to the targets without selling. Classes paused in
`config.toml` are skipped, with what each would need; a fourth argument `include-paused` buys
them too.

```bash
cargo run -p portfolio-sources --example portfolio -- 2026-10-14 0.95 2000
```

The split follows the Allocation screen of the design. With the new money the base grows by the
amount, and each class's need is its target share of the new base minus its value now (zero if it
is already over). If the needs of the classes being bought add up to the amount or more, the
amount is split pro rata to need. Otherwise, which only happens when a paused class is skipped,
every need is bought and the rest is split between those classes by target weight. A paused class
still counts in the base, and the plan reports what it would need. The crypto purchase is then
split between BTC and ETH the same way, against the crypto split; if crypto gets nothing, neither
coin does. Cash in the accounts is not counted, as in the allocation.

The IKZE block covers the calendar year of the report date: what was deposited into IKZE accounts
from 1 January up to the date, how much of that year's limit in `config.toml` is left, and the
estimated tax saving, which is the deductible part (everything paid in, up to the limit) times
the tax rate in `config.toml`. Deposits count in the year of their date. Withdrawals are shown
apart and give no limit back, since the limit caps payments in. The tax due on an early
withdrawal ("zwrot") is not estimated. A year without a limit in the config is reported, not
guessed.

## Configuration

Settings live in `config.toml` at the repository root (another file can be named with
`PORTFOLIO_CONFIG`), parsed and validated by `portfolio_core::config::Config`:

- `[targets]`: target allocation in percent (`vwce`, `crypto`, `gold`), adding up to exactly 100;
  `[targets.crypto_split]` splits the crypto share between `btc` and `eth`, also adding up to 100.
- `[paused]`: optional `vwce`, `crypto`, `gold` flags (`true`/`false`, default `false`). The
  top-up plan skips a paused class unless asked to include it; its target still counts.
- `[ikze.limits]`: the IKZE contribution limit in PLN per year (`2026 = "11304.00"`).
- `[tax] rate_percent`: the income tax rate used to estimate the IKZE deduction (12 or 32).
- `[gold] buyback_factor`: the share of melt value a dealer pays for a coin (above 0, at most 1.5).

Numbers are exact decimals: write whole numbers bare and fractions as strings (`"0.95"`); TOML
floats are rejected so that nothing passes through binary floating point. Unknown keys, missing
keys and shares that do not add up are reported as errors.

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
(e.g. `sqlite:///Users/me/portfolio/portfolio.db`) to keep it elsewhere. The gold buyback factor,
the IKZE limits and the tax rate come from `config.toml` (or the file named by
`PORTFOLIO_CONFIG`), and `ALPHAVANTAGE_API_KEY`
(environment or `.env`) prices VWCE; without it VWCE shows as unpriced.

Pages:
- `/`: the Overview (`design/screens/01–03`): total value with the period's P&L, the XIRR and
  Contributed tiles, and the value chart (value with its area, stepped net contributions, dashed
  "if all had gone into VWCE"). `?period=1M|6M|YTD|1Y|All` (default All) picks the period and
  `bench=off` hides the benchmark; the switch and the checkbox swap the panel through htmx and
  push the URL. Chart days are the period's start (the first contribution for All), each month
  end and today, priced through the same caches as the report. Problems with prices go to stderr.
- `/allocation`: the allocation (`design/screens/04`): the actual class shares over the dashed
  targets, the drift table (target, actual, drift in pp with a neutral bar, and in PLN; BTC and
  ETH as shares of crypto) and the top-up plan for an amount (`?amount=2000`, the default;
  `paused=on` includes the classes paused in `[paused]`): what each class gets (crypto split into
  BTC and ETH), what a skipped paused class would need, the shares after the top-up and whether
  the VWCE part fits what is left of the year's IKZE limit. The amount takes a decimal comma and
  spaces; the input, the chips and the checkbox swap the plan through htmx. Cash is not part of
  the base, and a held asset without a price is said instead of the shares.
- `/ikze`: the IKZE year (`design/screens/05`): paid in against the year's limit with the rest
  hatched, a reminder to top up before 31 December (days left; the average paid in a month so
  far and where it ends the year; the rest of the limit split evenly over the months left, the
  current one included until something is paid in it, rounded down to the grosz), the estimated
  deduction on what was paid in and on the whole limit, and the deposits by month with the
  months still to top up dashed. `?rate=12|32` picks the bracket (default `[tax] rate_percent`);
  the switch swaps the card through htmx. "Plan a top-up" opens `/allocation` with one suggested top-up. It needs no price, only the NBP rate of a deposit in
  another currency; a missing rate or a year without a limit is said instead of a total.
- `/gold`: the coins (`design/screens/06`): valuation, paid, P&L and holding tiles, the coins
  table (each coin's value and P&L, a remove button), and the inline add form (name, date, price;
  fine gold weight and a dealer quote under "Weight and dealer quote") with each field's error
  under it. Coins are valued at the NBP price per gram x weight x `[gold] buyback_factor`, or at
  their dealer quote; without an NBP price the others show "—". The header says how coins are
  valued and shows "Purchases paused" when `[paused] gold` is set. Adding and removing swap the
  tiles, table and form through htmx. Amounts may be typed with a decimal comma.
- `/import`: the XTB import (`design/screens/07`), Upload → Review → Done. Drop or choose the
  xStation export (at most 2 MB); the review lists its operations newest first, each "New" or
  "Already imported" (a checkbox hides the latter), and changes nothing. "Add N operations"
  imports the new ones and shows how much was paid into IKZE this year. The file is never written
  to disk: the review sends it back in a hidden field and it is parsed again on confirm, which
  makes a repeated confirm harmless. The file name is shown with the export's account number
  (and any run of six or more digits) masked.
- `/components`: the shared components (number tile, delta badge, progress and allocation
  bars, asset card, table rows, buttons, inputs, status chips) with the design's sample figures,
  to compare with `design/screens/09-components.png`.

Every page uses the Portfel layout from `design/`: the sidebar (a top bar with nav pills at
760px and narrower), `design/tokens.css` served as `/static/tokens.css`, and the Geist font from
Google Fonts. The theme follows the system until the sidebar toggle is used; the choice is then
kept in a `theme` cookie. Numbers are formatted by `portfolio_web::format` (`11 500,00 zł`, `−` for minus, `+14,0%`,
`pp` for drift).

States (`design/screens/08`):
- Overview with nothing imported or added: "Nothing to show yet" with Import XTB history and
  Add a coin.
- A source that is down (Overview, Allocation): a banner per source ("Alpha Vantage isn't
  responding. VWCE uses its last known price from 17 Oct, 06:21.") with Retry, which reloads the
  same view. The assets valued at a stored close get the "Stale" chip and "Last price …".
- Sidebar sync status: "Prices updated 08:00" when the last fetch is under 6 h old, "Prices from
  yesterday 22:00" in amber after that, "Couldn't update prices" with Retry when a source failed
  or a price is missing on the page, and "Updating prices…" while htmx re-renders the Overview
  panel.
- Overview loading: the panel's skeleton covers it while the period switch or the benchmark
  checkbox re-render it, from 250 ms on, so quick answers from the caches do not flash it.
- Chart with one valued day: "One data point so far" instead of a line.
- Import with nothing uploaded: the dashed drop zone (P24).

## Daily sync

`portfolio-sync` is the daily job: it syncs the Kraken ledger (read-only, skipped when the
Kraken keys are not set), values the portfolio on the day through the same price caches as the
report (so the day's prices get fetched and cached too) and stores one row per held asset in
`snapshots`: quantity, value in PLN and what is still invested in it in PLN (cost minus sale
proceeds and cash dividends), both rounded to the grosz. An asset sold out keeps a row with
quantity and value 0, so what its sales returned stays in the day. Cash left in the accounts has
no row.

```bash
cargo run -p portfolio-sync                        # today
cargo run -p portfolio-sync -- --date 2026-10-17   # catch up a missed day
cargo run -p portfolio-sync -- --skip-kraken       # snapshot only
```

A day is written whole or not at all: if any held asset has no price (Alpha Vantage down, no
key), or is valued at an earlier day's stored close because its source failed (the pages' "Stale"
stand-in), nothing is stored for that date and an earlier snapshot of it stays; catch the day up
later with `--date`, which asks for that day's own close. Running it again on the
same date replaces that date's rows. It reads `config.toml`, `.env` and `portfolio.db` from the
working directory (or `PORTFOLIO_CONFIG` / `PORTFOLIO_DB_URL`), prints counts and asset names but
no amounts, and exits non-zero when a step failed (a failed Kraken sync still lets the snapshot
run on the ledger already stored).

### Run it every day with launchd (macOS)

```bash
deploy/launchd/install.sh
```

This builds the release binary, writes `~/Library/LaunchAgents/com.portfolio.sync.plist` from
`deploy/launchd/portfolio-sync.plist.template` with this checkout's path, checks it with
`plutil -lint` and loads it. The job runs at 22:30 local time (after the Xetra close and the
day's NBP tables); if the Mac is asleep then, launchd runs it once on wake, and a day the Mac is
off is skipped (catch it up with `--date`). Output goes to `~/Library/Logs/portfolio-sync.log`.

```bash
launchctl kickstart gui/$(id -u)/com.portfolio.sync   # run now
launchctl print gui/$(id -u)/com.portfolio.sync       # state and last exit code
tail -n 20 ~/Library/Logs/portfolio-sync.log
deploy/launchd/install.sh uninstall                   # unload and remove
```

Re-run `install.sh` after pulling changes, so the agent uses a freshly built binary. If the
checkout moves, run it from the new place.

### Monthly summary in Telegram

Optional, off by default. With `monthly_summary = true` under `[telegram]` in `config.toml` and
`TELEGRAM_BOT_TOKEN` / `TELEGRAM_CHAT_ID` in `.env` (the F1 digest's bot), the daily run sends
last month's summary once: on its first run in the new month, or the first one after that if
the Mac was off. A failed send is logged, the run exits non-zero, and the next run tries again.
Sent months are kept in `telegram_summaries`; the log says only "summary for 2026-09 sent".

The summary compares the month's last snapshot with the last one before the month: value, the
change split into new money (what went into the assets, net of sales) and profit, profit to
date, and each asset's profit in the month. It is plain text in the pages' number format. Cash
is not included, as in the snapshots.

```bash
cargo run -p portfolio-sync -- summary                     # print last month's, send nothing
cargo run -p portfolio-sync -- summary --month 2026-09     # print a given month's
cargo run -p portfolio-sync -- summary --send              # send it now (once per month)
```

## Docker

An alternative to running the binaries directly. The image (`Dockerfile`) holds both
`portfolio-web` and `portfolio-sync`, runs them as an unprivileged user (uid 10001) with
`TZ=Europe/Warsaw`, and holds no secrets and no data: `.env`, databases, XTB exports and
`config.toml` are kept out of the build context (`.dockerignore`) and passed in at run time.

```bash
docker build -t portfolio .        # native arch: linux/arm64 on Apple Silicon
```

Both architectures at once (amd64 is emulated on a Mac, so it is much slower):

```bash
docker buildx build --platform linux/arm64,linux/amd64 -t portfolio .
```

Run the dashboard, with the database on a named volume and the config mounted read-only:

```bash
docker run -d --name portfolio-web \
  -p 127.0.0.1:3000:3000 \
  -v portfolio-data:/data \
  -v "$PWD/config.toml:/config/config.toml:ro" \
  --env-file .env \
  portfolio
```

Always publish the port on `127.0.0.1` as above: inside the container the server listens on
`0.0.0.0:3000` (`PORTFOLIO_ADDR`), so `-p 3000:3000` would open the dashboard to the network.
Run the sync for a day with the same volume, config and `.env`:

```bash
docker run --rm -v portfolio-data:/data -v "$PWD/config.toml:/config/config.toml:ro" \
  --env-file .env portfolio portfolio-sync --date 2026-10-20
```

Defaults inside the image: database `sqlite:///data/portfolio.db` (`PORTFOLIO_DB_URL`), config
`/config/config.toml` (`PORTFOLIO_CONFIG`). Dependencies are compiled in their own cached layer
(cargo-chef), so after a code change only the workspace crates are rebuilt. The Rust version is
the `RUST_VERSION` build argument. `.github/workflows/docker.yml` builds the amd64 image and
smoke-tests it on pull requests that touch what goes into it.

## Checks

These must all pass before a PR is opened:

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

GitHub Actions runs the same three checks on every pull request and on pushes to `main`
(`.github/workflows/ci.yml`, Ubuntu runner, latest stable Rust).
