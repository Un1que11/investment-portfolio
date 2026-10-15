-- Which published NBP rate applies to a given calendar day.
--
-- NBP publishes table A on business days only. Once a past day has been resolved (to its own
-- table, or to the last one before a weekend / holiday), the answer never changes, so it is
-- cached here and later lookups need no network. Days from "today" onwards are never stored:
-- today's table may not be published yet.
CREATE TABLE fx_rate_days (
    currency   TEXT NOT NULL,
    day        TEXT NOT NULL CHECK (date(day) IS day),
    rate_date  TEXT NOT NULL CHECK (date(rate_date) IS rate_date),
    PRIMARY KEY (currency, day),
    FOREIGN KEY (currency, rate_date) REFERENCES fx_rates (currency, rate_date) ON DELETE CASCADE,
    CHECK (rate_date <= day)
) STRICT;
