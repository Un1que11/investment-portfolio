//! Domain models and portfolio calculations.
//!
//! This crate has no I/O: everything here is pure and unit-tested.

pub mod config;
pub mod fx;
pub mod gold;
pub mod pnl;
pub mod portfolio;
pub mod price;
pub mod xirr;

use std::fmt;
use std::str::FromStr;

/// Top-level asset classes that target allocations are defined for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum AssetClass {
    Vwce,
    Crypto,
    Gold,
}

impl AssetClass {
    pub const ALL: [AssetClass; 3] = [AssetClass::Vwce, AssetClass::Crypto, AssetClass::Gold];

    /// Stable identifier used in config files and the database.
    pub fn as_str(self) -> &'static str {
        match self {
            AssetClass::Vwce => "vwce",
            AssetClass::Crypto => "crypto",
            AssetClass::Gold => "gold",
        }
    }
}

impl fmt::Display for AssetClass {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnknownAssetClass(pub String);

impl fmt::Display for UnknownAssetClass {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "unknown asset class: {:?}", self.0)
    }
}

impl std::error::Error for UnknownAssetClass {}

impl FromStr for AssetClass {
    type Err = UnknownAssetClass;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        AssetClass::ALL
            .into_iter()
            .find(|class| class.as_str().eq_ignore_ascii_case(s.trim()))
            .ok_or_else(|| UnknownAssetClass(s.to_owned()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identifiers_round_trip() {
        for class in AssetClass::ALL {
            assert_eq!(class.to_string().parse::<AssetClass>(), Ok(class));
        }
    }

    #[test]
    fn parsing_ignores_case_and_surrounding_whitespace() {
        assert_eq!(" Gold ".parse::<AssetClass>(), Ok(AssetClass::Gold));
        assert_eq!("VWCE".parse::<AssetClass>(), Ok(AssetClass::Vwce));
    }

    #[test]
    fn unknown_identifier_is_rejected() {
        assert_eq!(
            "bonds".parse::<AssetClass>(),
            Err(UnknownAssetClass("bonds".to_owned()))
        );
    }
}
