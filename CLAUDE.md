# Portfolio — personal investment dashboard

## Goal
One local dashboard that shows Misha's whole portfolio in PLN: how much has been invested, how much has been earned, which asset has earned the most and which the least, and how far the current allocation is from the target. Runs locally on a MacBook (macOS, Apple Silicon).

## Assets and target allocation
| Class | Target | Split | Where it is held |
|---|---|---|---|
| VWCE | 55% | — | IKZE at XTB (an IKE will be added later) |
| Crypto | 25% | BTC 75% / ETH 25% | Kraken |
| Gold | 20% | physical 1/10 oz coins | entered by hand |

Targets live in `config.toml`, not in code.

## Stack
- Rust (stable), Axum, SQLite via sqlx (migrations in `migrations/`)
- Templates: askama + htmx; charts: uPlot or Chart.js (loaded from a CDN)
- xlsx parsing: calamine
- Background jobs: a separate `portfolio-sync` binary, run by launchd
- Base currency: PLN. All amounts are stored as `rust_decimal::Decimal`, never as f64

## Data sources
- **XTB:** the API has been switched off since 14.03.2025. The only source is the account history export from xStation (xlsx), which Misha uploads by hand on the import page. The import must be idempotent: uploading the same file again creates no duplicates.
- **Kraken:** private REST API (Ledgers, TradesHistory, Balance) with a read-only key. The key is kept in the macOS Keychain or in `.env`, which is never committed.
- **Gold:** coins entered by hand (date, purchase price in PLN, weight 3.11 g). Value = NBP price per gram × weight × buyback factor from the config, with the option of a manual quote.
- **Prices:** exchange rates — NBP API (table A); crypto — Kraken public Ticker; VWCE — Stooq CSV (`vwce.de`, EUR). Check that a source responds before using it. If it doesn't, propose an alternative in the PR; don't make things up.

## Key calculations
- Invested (net contributions) vs current value, P&L absolute and in percent
- Return that accounts for the timing of contributions (XIRR), for the portfolio and for each asset
- Each asset's contribution to the total profit (ranking: "earned the most / the least")
- Deviation from target in percentage points and in PLN; a calculator: "I have N zł — how should I split my purchases to get closer to the target without selling"
- IKZE: contributed this year, remaining limit (2026: 11 304 zł; limits per year in the config), estimated tax deduction at a 12% or 32% rate (from the config)
- Benchmark: "if every contribution had gone into VWCE on the same dates"

## Rules for autonomous work
1. Each session takes **one** task: the first open one in `BACKLOG.md`.
2. Work on a separate branch `feat/<id>-<slug>`, open a PR, and describe in it what was done and how to verify it.
3. `cargo fmt`, `cargo clippy -- -D warnings` and `cargo test` must pass before the PR is opened.
4. Every calculation is covered by a unit test with an example worked out by hand.
5. Never commit keys, real exports or real amounts. Fixtures must be anonymised.
6. No API requests other than reads. No trading operations.
7. If a task depends on missing information (for example, the format of a real XTB export), mark it `BLOCKED: <question>` in BACKLOG and move on to the next one.
8. When done, tick the task `[x]` in BACKLOG in the same PR.

## Work modes: day and night
- **Day (manual, Misha starts it):** heavy backlog tasks, one at a time, following the rules above.
- **Night (scheduled task):** light work only:
  1. Review open PRs: leave a comment with bugs found, risks and missing tests.
  2. Bug fixes: if an open PR has failing checks or the review found an obvious bug, fix it with a commit on the same branch.
  3. If there are no open PRs, or they are fine, take the first open task marked `[light]` in BACKLOG.md.
  4. Do not start heavy tasks (without `[light]`) at night. If there are no light tasks, do nothing and report that briefly.
- Misha sets the `[light]` tag himself; at night, never add or remove it.

## Language
Commits, branches, PR descriptions, code comments and reviews: English only.
