-- Initial schema.
--
-- Conventions:
-- * Every money / quantity / rate column is TEXT holding a canonical decimal string written by
--   rust_decimal (e.g. "-12.3400"). Floats never reach the database: the CHECK pattern rejects
--   exponent notation such as "1.0e-07", which is what SQLite produces when a REAL slips in.
-- * Dates are TEXT "YYYY-MM-DD"; timestamps are TEXT RFC 3339 in UTC ("2026-10-15T12:00:00Z").
-- * Currencies are ISO 4217 codes; crypto assets use their ticker as the currency of quantity.
-- * Tables are STRICT so column types are enforced.

CREATE TABLE accounts (
    id          INTEGER PRIMARY KEY,
    name        TEXT    NOT NULL UNIQUE,
    -- ikze / ike: Polish tax-advantaged retirement wrappers; exchange: e.g. Kraken;
    -- manual: holdings entered by hand (physical gold).
    kind        TEXT    NOT NULL CHECK (kind IN ('ikze', 'ike', 'exchange', 'manual')),
    provider    TEXT    NOT NULL CHECK (provider IN ('xtb', 'kraken', 'manual')),
    currency    TEXT    NOT NULL CHECK (length(currency) = 3 AND currency = upper(currency)),
    created_at  TEXT    NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now'))
) STRICT;

CREATE TABLE assets (
    id           INTEGER PRIMARY KEY,
    -- Stable internal identifier, e.g. 'VWCE', 'BTC', 'ETH', 'GOLD_COIN_1_10OZ'.
    symbol       TEXT    NOT NULL UNIQUE,
    name         TEXT    NOT NULL,
    asset_class  TEXT    NOT NULL CHECK (asset_class IN ('vwce', 'crypto', 'gold')),
    -- Currency the asset is quoted in at its price source (VWCE: EUR, crypto: PLN/EUR, gold: PLN).
    currency     TEXT    NOT NULL CHECK (length(currency) = 3 AND currency = upper(currency))
) STRICT;

CREATE TABLE transactions (
    id           INTEGER PRIMARY KEY,
    account_id   INTEGER NOT NULL REFERENCES accounts (id) ON DELETE RESTRICT,
    -- NULL for pure cash movements (deposit / withdrawal, account-level fees).
    asset_id     INTEGER REFERENCES assets (id) ON DELETE RESTRICT,
    kind         TEXT    NOT NULL
                 CHECK (kind IN ('buy', 'sell', 'deposit', 'withdrawal', 'fee', 'dividend')),
    executed_at  TEXT    NOT NULL CHECK (strftime('%Y-%m-%dT%H:%M:%SZ', executed_at) IS executed_at),
    -- Units of the asset; required for buy / sell, always positive.
    quantity     TEXT
                 CHECK (quantity IS NULL OR (
                     quantity GLOB '*[0-9]*' AND quantity NOT GLOB '*[^0-9.]*'
                     AND quantity NOT GLOB '*.*.*' AND quantity GLOB '*[1-9]*')),
    -- Price per unit in `currency`; required for buy / sell.
    unit_price   TEXT
                 CHECK (unit_price IS NULL OR (
                     unit_price GLOB '*[0-9]*' AND unit_price NOT GLOB '*[^0-9.]*'
                     AND unit_price NOT GLOB '*.*.*')),
    -- Absolute cash amount of the operation in `currency` (direction comes from `kind`).
    amount       TEXT    NOT NULL
                 CHECK (amount GLOB '*[0-9]*' AND amount NOT GLOB '*[^0-9.]*'
                        AND amount NOT GLOB '*.*.*'),
    currency     TEXT    NOT NULL CHECK (length(currency) = 3 AND currency = upper(currency)),
    -- Where the row came from and its id there, for idempotent imports:
    -- XTB: hash of the export row; Kraken: ledger id; manual: generated.
    source       TEXT    NOT NULL CHECK (source IN ('xtb', 'kraken', 'manual')),
    external_id  TEXT    NOT NULL,
    note         TEXT,
    created_at   TEXT    NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now')),
    UNIQUE (source, external_id),
    CHECK (kind NOT IN ('buy', 'sell')
           OR (asset_id IS NOT NULL AND quantity IS NOT NULL AND unit_price IS NOT NULL)),
    CHECK (kind <> 'dividend' OR asset_id IS NOT NULL)
) STRICT;

CREATE INDEX transactions_account_time ON transactions (account_id, executed_at);
CREATE INDEX transactions_asset_time ON transactions (asset_id, executed_at);

-- Daily closing prices per asset, in the asset's quote currency.
CREATE TABLE prices (
    asset_id    INTEGER NOT NULL REFERENCES assets (id) ON DELETE CASCADE,
    price_date  TEXT    NOT NULL CHECK (date(price_date) IS price_date),
    price       TEXT    NOT NULL
                CHECK (price GLOB '*[0-9]*' AND price NOT GLOB '*[^0-9.]*'
                       AND price NOT GLOB '*.*.*'),
    currency    TEXT    NOT NULL CHECK (length(currency) = 3 AND currency = upper(currency)),
    source      TEXT    NOT NULL CHECK (source IN ('stooq', 'kraken', 'nbp', 'manual')),
    fetched_at  TEXT    NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now')),
    PRIMARY KEY (asset_id, price_date)
) STRICT;

-- NBP table A mid rates: PLN per 1 unit of `currency`, as published for `rate_date`.
CREATE TABLE fx_rates (
    currency       TEXT NOT NULL CHECK (length(currency) = 3 AND currency = upper(currency)
                                        AND currency <> 'PLN'),
    rate_date      TEXT NOT NULL CHECK (date(rate_date) IS rate_date),
    rate           TEXT NOT NULL
                   CHECK (rate GLOB '*[0-9]*' AND rate NOT GLOB '*[^0-9.]*'
                          AND rate NOT GLOB '*.*.*' AND rate GLOB '*[1-9]*'),
    -- NBP table number, e.g. '199/A/NBP/2026'.
    table_no       TEXT NOT NULL,
    fetched_at     TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now')),
    PRIMARY KEY (currency, rate_date)
) STRICT;

-- Daily per-asset valuation written by portfolio-sync. Portfolio totals are sums over a date.
CREATE TABLE snapshots (
    snapshot_date  TEXT    NOT NULL CHECK (date(snapshot_date) IS snapshot_date),
    asset_id       INTEGER NOT NULL REFERENCES assets (id) ON DELETE CASCADE,
    quantity       TEXT    NOT NULL
                   CHECK (quantity GLOB '*[0-9]*' AND quantity NOT GLOB '*[^0-9.]*'
                          AND quantity NOT GLOB '*.*.*'),
    value_pln      TEXT    NOT NULL
                   CHECK (value_pln GLOB '*[0-9]*' AND value_pln NOT GLOB '*[^0-9.]*'
                          AND value_pln NOT GLOB '*.*.*'),
    -- Net contributions attributed to the asset up to and including this date; can be
    -- negative after sales that returned more than was put in.
    invested_pln   TEXT    NOT NULL
                   CHECK (invested_pln GLOB '*[0-9]*' AND invested_pln NOT GLOB '*[^0-9.-]*'
                          AND invested_pln NOT GLOB '?*-*' AND invested_pln NOT GLOB '*.*.*'),
    created_at     TEXT    NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now')),
    PRIMARY KEY (snapshot_date, asset_id)
) STRICT;
