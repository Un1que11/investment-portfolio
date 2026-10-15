-- Physical gold coins entered by hand (BACKLOG P9).
--
-- Each coin also has a 'buy' row in `transactions` (account "Gold (manual)", asset
-- GOLD_COINS, quantity 1, amount = purchase price), so contributions and positions treat gold
-- like every other asset. Deleting that transaction deletes the coin (ON DELETE CASCADE);
-- the application deletes coins that way, in one database transaction.
CREATE TABLE gold_coins (
    id                  INTEGER PRIMARY KEY,
    transaction_id      INTEGER NOT NULL UNIQUE REFERENCES transactions (id) ON DELETE CASCADE,
    -- Optional free text, e.g. "Krugerrand 1/10 oz".
    label               TEXT CHECK (label IS NULL OR length(label) BETWEEN 1 AND 100),
    purchased_on        TEXT NOT NULL CHECK (date(purchased_on) IS purchased_on),
    purchase_price_pln  TEXT NOT NULL
                        CHECK (purchase_price_pln GLOB '*[0-9]*'
                               AND purchase_price_pln NOT GLOB '*[^0-9.]*'
                               AND purchase_price_pln NOT GLOB '*.*.*'
                               AND purchase_price_pln GLOB '*[1-9]*'),
    -- Grams of fine gold; 3.11 for a 1/10 oz coin.
    fine_weight_g       TEXT NOT NULL
                        CHECK (fine_weight_g GLOB '*[0-9]*'
                               AND fine_weight_g NOT GLOB '*[^0-9.]*'
                               AND fine_weight_g NOT GLOB '*.*.*'
                               AND fine_weight_g GLOB '*[1-9]*'),
    -- A dealer quote that replaces the NBP-based valuation when set.
    manual_quote_pln    TEXT
                        CHECK (manual_quote_pln IS NULL OR (
                               manual_quote_pln GLOB '*[0-9]*'
                               AND manual_quote_pln NOT GLOB '*[^0-9.]*'
                               AND manual_quote_pln NOT GLOB '*.*.*'
                               AND manual_quote_pln GLOB '*[1-9]*')),
    created_at          TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now'))
) STRICT;
