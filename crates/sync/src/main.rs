//! Background sync: daily price and value snapshots. Scheduled by launchd (see BACKLOG P25).

use portfolio_core::AssetClass;

fn main() {
    let classes: Vec<&str> = AssetClass::ALL.iter().map(|c| c.as_str()).collect();
    println!(
        "portfolio-sync: nothing to sync yet (asset classes: {})",
        classes.join(", ")
    );
}
