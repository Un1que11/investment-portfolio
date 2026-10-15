-- Allow 'alphavantage' as a price source. Stooq's CSV downloads now sit behind a JavaScript
-- bot check, so VWCE quotes come from Alpha Vantage instead (see BACKLOG P6).
--
-- SQLite cannot alter a CHECK constraint, so the table is rebuilt. No other table references
-- `prices`, so dropping it is safe.
CREATE TABLE prices_new (
    asset_id    INTEGER NOT NULL REFERENCES assets (id) ON DELETE CASCADE,
    price_date  TEXT    NOT NULL CHECK (date(price_date) IS price_date),
    price       TEXT    NOT NULL
                CHECK (price GLOB '*[0-9]*' AND price NOT GLOB '*[^0-9.]*'
                       AND price NOT GLOB '*.*.*'),
    currency    TEXT    NOT NULL CHECK (length(currency) = 3 AND currency = upper(currency)),
    source      TEXT    NOT NULL
                CHECK (source IN ('stooq', 'alphavantage', 'kraken', 'nbp', 'manual')),
    fetched_at  TEXT    NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now')),
    PRIMARY KEY (asset_id, price_date)
) STRICT;

INSERT INTO prices_new (asset_id, price_date, price, currency, source, fetched_at)
SELECT asset_id, price_date, price, currency, source, fetched_at FROM prices;

DROP TABLE prices;
ALTER TABLE prices_new RENAME TO prices;
