//! SQLite storage: opening the database, running migrations, and storing decimals as text.
//!
//! Schema lives in `migrations/` at the workspace root and is embedded at compile time.

use std::str::FromStr;

use rust_decimal::Decimal;
use sqlx::encode::IsNull;
use sqlx::error::BoxDynError;
use sqlx::migrate::{MigrateError, Migrator};
use sqlx::sqlite::{
    SqliteArgumentValue, SqliteConnectOptions, SqliteJournalMode, SqlitePool, SqlitePoolOptions,
    SqliteTypeInfo, SqliteValueRef,
};
use sqlx::{Decode, Encode, Sqlite, Type};

pub static MIGRATOR: Migrator = sqlx::migrate!("../../migrations");

/// Opens (creating if needed) the database at `url`, e.g. `sqlite://portfolio.db`, and applies
/// pending migrations.
pub async fn open(url: &str) -> Result<SqlitePool, OpenError> {
    let options = SqliteConnectOptions::from_str(url)?
        .create_if_missing(true)
        .foreign_keys(true)
        .journal_mode(SqliteJournalMode::Wal);
    let pool = SqlitePoolOptions::new().connect_with(options).await?;
    MIGRATOR.run(&pool).await?;
    Ok(pool)
}

/// An in-memory database with migrations applied. One connection, so all queries share it.
pub async fn open_in_memory() -> Result<SqlitePool, OpenError> {
    let options = SqliteConnectOptions::from_str("sqlite::memory:")?.foreign_keys(true);
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(options)
        .await?;
    MIGRATOR.run(&pool).await?;
    Ok(pool)
}

#[derive(Debug)]
pub enum OpenError {
    Connect(sqlx::Error),
    Migrate(MigrateError),
}

impl std::fmt::Display for OpenError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            OpenError::Connect(e) => write!(f, "cannot open database: {e}"),
            OpenError::Migrate(e) => write!(f, "cannot apply migrations: {e}"),
        }
    }
}

impl std::error::Error for OpenError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            OpenError::Connect(e) => Some(e),
            OpenError::Migrate(e) => Some(e),
        }
    }
}

impl From<sqlx::Error> for OpenError {
    fn from(e: sqlx::Error) -> Self {
        OpenError::Connect(e)
    }
}

impl From<MigrateError> for OpenError {
    fn from(e: MigrateError) -> Self {
        OpenError::Migrate(e)
    }
}

/// A `Decimal` stored as a canonical decimal string in a TEXT column.
///
/// SQLite has no decimal type and its REAL is a binary float, so amounts are kept as text to
/// stay exact. Scale is preserved: `12.3400` is written and read back as `12.3400`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct DbDecimal(pub Decimal);

impl From<Decimal> for DbDecimal {
    fn from(d: Decimal) -> Self {
        DbDecimal(d)
    }
}

impl From<DbDecimal> for Decimal {
    fn from(d: DbDecimal) -> Self {
        d.0
    }
}

impl Type<Sqlite> for DbDecimal {
    fn type_info() -> SqliteTypeInfo {
        <String as Type<Sqlite>>::type_info()
    }

    fn compatible(ty: &SqliteTypeInfo) -> bool {
        <String as Type<Sqlite>>::compatible(ty)
    }
}

impl<'q> Encode<'q, Sqlite> for DbDecimal {
    fn encode_by_ref(&self, buf: &mut Vec<SqliteArgumentValue<'q>>) -> Result<IsNull, BoxDynError> {
        <String as Encode<'q, Sqlite>>::encode(self.0.to_string(), buf)
    }
}

impl<'r> Decode<'r, Sqlite> for DbDecimal {
    fn decode(value: SqliteValueRef<'r>) -> Result<Self, BoxDynError> {
        let text = <&str as Decode<'r, Sqlite>>::decode(value)?;
        // Plain decimal notation only: exponent forms mean a float got in somewhere.
        Ok(DbDecimal(Decimal::from_str_exact(text)?))
    }
}
