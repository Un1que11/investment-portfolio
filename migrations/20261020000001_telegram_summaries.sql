-- Months whose summary was sent to Telegram (BACKLOG P26), so the daily sync sends each month
-- once, on its first run in the next month (or later, if the Mac was off).
CREATE TABLE telegram_summaries (
    -- "2026-09"
    month    TEXT PRIMARY KEY CHECK (month GLOB '[0-9][0-9][0-9][0-9]-[01][0-9]'),
    -- RFC 3339, UTC.
    sent_at  TEXT NOT NULL
);
