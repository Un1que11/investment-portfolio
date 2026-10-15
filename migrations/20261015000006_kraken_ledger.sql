-- Raw Kraken ledger entries (BACKLOG P11), exactly as returned by the private Ledgers endpoint.
--
-- This is the source of truth for the Kraken account: transactions are mapped from it, and
-- entry types that cannot be mapped yet stay here (reported by the sync) so they can be
-- mapped later without fetching again. Amounts are signed; the fee is charged on top
-- (balance = previous balance + amount - fee).
CREATE TABLE kraken_ledger (
    ledger_id   TEXT PRIMARY KEY,
    refid       TEXT NOT NULL,
    -- Unix time with fractional seconds, as sent (exact text).
    time_unix   TEXT NOT NULL CHECK (time_unix GLOB '[0-9]*' AND time_unix NOT GLOB '*[^0-9.]*'),
    kind        TEXT NOT NULL,
    subtype     TEXT NOT NULL DEFAULT '',
    aclass      TEXT NOT NULL DEFAULT '',
    asset       TEXT NOT NULL,
    amount      TEXT NOT NULL
                CHECK (amount GLOB '*[0-9]*' AND amount NOT GLOB '*[^0-9.-]*'
                       AND amount NOT GLOB '?*-*' AND amount NOT GLOB '*.*.*'),
    fee         TEXT NOT NULL
                CHECK (fee GLOB '*[0-9]*' AND fee NOT GLOB '*[^0-9.]*' AND fee NOT GLOB '*.*.*'),
    balance     TEXT NOT NULL
                CHECK (balance GLOB '*[0-9]*' AND balance NOT GLOB '*[^0-9.-]*'
                       AND balance NOT GLOB '?*-*' AND balance NOT GLOB '*.*.*'),
    fetched_at  TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now'))
) STRICT;

CREATE INDEX kraken_ledger_refid ON kraken_ledger (refid);
