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
| `portfolio-db` | `crates/db` | SQLite storage: connection, migrations, decimal codec |
| `portfolio-web` | `crates/web` | Axum web dashboard (binary `portfolio-web`) |
| `portfolio-sync` | `crates/sync` | Background sync binary (`portfolio-sync`), run by launchd |

## Database

SQLite via sqlx. The schema lives in `migrations/` and is embedded into the binaries, which
apply pending migrations on startup (`portfolio_db::open`). Money, quantities and rates are
stored as exact decimal strings (`rust_decimal`), never as floats; the schema rejects anything
else. Database files (`*.db`) are git-ignored.

To add a migration, create `migrations/<YYYYMMDDHHMMSS>_<name>.sql`. Never edit a migration
that has already been applied to a real database.

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
