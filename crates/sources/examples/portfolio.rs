//! Print holdings, net contributions, value, P&L, each asset's contribution to it, XIRR and the
//! deviation from the target allocation in PLN for a date (default: today), the year's IKZE
//! contributions against the limit, and optionally how to split a top-up of N PLN:
//!
//!     cargo run -p portfolio-sources --example portfolio -- [YYYY-MM-DD] [gold buyback factor] [N]
//!
//! Uses PORTFOLIO_DB_URL (default sqlite://portfolio.db) and ALPHAVANTAGE_API_KEY for VWCE
//! (environment or `.env`). Targets and the gold buyback factor come from `config.toml` (or the
//! file named by PORTFOLIO_CONFIG); a factor given on the command line overrides the config's.

use chrono::{Local, NaiveDate, Utc};
use portfolio_core::allocation::Deviation;
use portfolio_core::config::Config;
use portfolio_sources::alphavantage::AlphaVantageClient;
use portfolio_sources::kraken::KrakenClient;
use portfolio_sources::nbp::NbpClient;
use portfolio_sources::prices::Clock;
use portfolio_sources::valuation::{Sources, portfolio_on};
use rust_decimal::Decimal;

#[tokio::main]
async fn main() -> std::process::ExitCode {
    match run().await {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            std::process::ExitCode::FAILURE
        }
    }
}

async fn run() -> Result<(), Box<dyn std::error::Error>> {
    let _ = dotenvy::dotenv();
    let clock = Clock {
        now: Utc::now(),
        today: Local::now().date_naive(),
    };
    let mut args = std::env::args().skip(1);
    let date = match args.next() {
        Some(s) => s.parse::<NaiveDate>()?,
        None => clock.today,
    };
    let path = std::env::var("PORTFOLIO_CONFIG").unwrap_or_else(|_| "config.toml".to_owned());
    let text =
        std::fs::read_to_string(&path).map_err(|e| format!("cannot read config {path}: {e}"))?;
    let config = Config::from_toml(&text)?;
    let factor = match args.next() {
        Some(s) => s.parse::<Decimal>()?,
        None => config.gold_buyback_factor(),
    };
    let top_up = args.next().map(|s| s.parse::<Decimal>()).transpose()?;
    let db_url =
        std::env::var("PORTFOLIO_DB_URL").unwrap_or_else(|_| "sqlite://portfolio.db".to_owned());
    let pool = portfolio_db::open(&db_url).await?;
    let nbp = NbpClient::new()?;
    let kraken = KrakenClient::new()?;
    let av = AlphaVantageClient::from_env().ok();
    let sources = Sources {
        pool: &pool,
        nbp: &nbp,
        kraken: &kraken,
        alphavantage: av.as_ref(),
        clock,
        gold_buyback_factor: factor,
    };
    let r = portfolio_on(date, &sources).await?;

    let pln = |v: Decimal| format!("{:.2}", v);
    println!("Portfolio on {date}");
    for (asset, qty) in &r.holdings.assets {
        let value = r
            .valuation
            .by_asset
            .get(asset)
            .map_or("no price".into(), |v| format!("{} PLN", pln(*v)));
        println!("  {asset:<11} {qty:>14}  {value:>16}");
    }
    for ((account, currency), amount) in &r.holdings.cash {
        let value = r
            .valuation
            .cash
            .get(&(account.clone(), currency.clone()))
            .map_or("no rate".into(), |v| format!("{} PLN", pln(*v)));
        println!("  cash {currency} ({account}) {amount}  {value}");
    }
    println!(
        "Value: {} PLN{}",
        pln(r.valuation.total),
        if r.valuation.complete() {
            ""
        } else {
            " (incomplete)"
        }
    );
    println!("Net contributions: {} PLN", pln(r.contributions.total));
    for (account, c) in &r.contributions.by_account {
        println!("  {account}: {} PLN", pln(*c));
    }
    let pct = |ratio: Option<Decimal>| {
        ratio.map_or("-".into(), |x| format!("{:.2}%", x * Decimal::ONE_HUNDRED))
    };
    match r.pnl.pnl_if_complete() {
        Some(total) => println!("P&L: {} PLN ({})", pln(total), pct(r.pnl.pnl_ratio())),
        None => println!(
            "P&L: not available, prices missing for {}",
            r.valuation.missing.join(", ")
        ),
    }
    for (asset, a) in &r.pnl.by_asset {
        let p = a
            .pnl()
            .map_or("no price".into(), |v| format!("{} PLN", pln(v)));
        println!(
            "  {asset:<11} cost {:>10}  P&L {p:>14} ({})",
            pln(a.cost),
            pct(a.pnl_ratio())
        );
    }
    if let Some(other) = r.pnl.other() {
        println!("  cash, FX on cash, account fees: {} PLN", pln(other));
    }
    println!("Contribution to P&L, most to least:");
    for (asset, c) in &r.attribution.ranked {
        let points = c.return_points.map_or("-".into(), |x| {
            format!("{:+.2} pp", x * Decimal::ONE_HUNDRED)
        });
        println!(
            "  {asset:<11} {:>14} PLN  {:>8} of the total  {points:>10}",
            pln(c.pnl),
            pct(c.share_of_total)
        );
    }
    if !r.attribution.unpriced.is_empty() {
        println!(
            "  not ranked (no price): {}",
            r.attribution.unpriced.join(", ")
        );
    }
    let annual = |x: &Result<Decimal, _>| match x {
        Ok(rate) => format!("{} a year", pct(Some(*rate))),
        Err(e) => format!("not available ({e})"),
    };
    let since = r.xirr.first_flow.map_or("-".into(), |d| d.to_string());
    println!("XIRR since {since}: {}", annual(&r.xirr.portfolio));
    for (asset, x) in &r.xirr.by_asset {
        println!("  {asset:<11} {}", annual(x));
    }
    let points = |x: Decimal| format!("{:+.2} pp", x * Decimal::ONE_HUNDRED);
    let line = |name: &str, d: &Deviation| {
        println!(
            "  {name:<11} {:>14} PLN  {:>7} (target {:>7})  {:>10}  {:>14} PLN",
            pln(d.value),
            pct(Some(d.share)),
            pct(Some(d.target_share)),
            points(d.points()),
            format!("{:+.2}", d.pln()),
        )
    };
    match r.allocation(&config) {
        Ok(a) => {
            println!(
                "Allocation of {} PLN (cash left out), deviation from target:",
                pln(a.total)
            );
            for (class, d) in &a.by_class {
                line(class.as_str(), d);
            }
            if let Some(split) = &a.within_crypto {
                println!("  within crypto:");
                line("btc", &split.btc);
                line("eth", &split.eth);
            }
        }
        Err(e) => println!("Allocation: not available ({e})"),
    }
    if let Some(amount) = top_up {
        match r.top_up(&config, amount) {
            Ok(t) => {
                println!("Top-up of {} PLN without selling, buy:", pln(amount));
                for (s, buy) in &t.buys {
                    println!("  {:<11} {:>14} PLN", s.as_str(), pln(*buy));
                }
                println!("Deviation from target after it:");
                for (class, d) in &t.after.by_class {
                    line(class.as_str(), d);
                }
                if let Some(split) = &t.after.within_crypto {
                    println!("  within crypto:");
                    line("btc", &split.btc);
                    line("eth", &split.eth);
                }
            }
            Err(e) => println!("Top-up: not available ({e})"),
        }
    }
    match r.ikze(&config) {
        Ok(y) => {
            println!(
                "IKZE {}: contributed {} of {} PLN, {} PLN left",
                y.year,
                pln(y.contributed),
                pln(y.limit),
                pln(y.remaining)
            );
            if !y.over_limit.is_zero() {
                println!(
                    "  {} PLN over the limit (not deductible)",
                    pln(y.over_limit)
                );
            }
            println!(
                "  estimated tax saving: {} PLN ({} of {} PLN deductible)",
                pln(y.tax_saving),
                pct(Some(y.tax_rate)),
                pln(y.deductible)
            );
            if !y.withdrawn.is_zero() {
                println!(
                    "  withdrawn this year: {} PLN (frees no limit; tax on it not estimated)",
                    pln(y.withdrawn)
                );
            }
        }
        Err(e) => println!("IKZE: not available ({e})"),
    }
    for p in &r.problems {
        println!("  note: {p}");
    }
    Ok(())
}
