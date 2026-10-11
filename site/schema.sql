-- One row per page view or tracked click. No IP addresses or cookies: the
-- visitor column is a hash that changes every day (see src/worker.js).
CREATE TABLE IF NOT EXISTS hits (
  id INTEGER PRIMARY KEY,
  at TEXT NOT NULL,        -- ISO time
  day TEXT NOT NULL,       -- YYYY-MM-DD (UTC)
  kind TEXT NOT NULL,      -- view, download or github
  path TEXT,
  referrer TEXT,           -- the referring site's host name only
  country TEXT,            -- two-letter code from Cloudflare
  device TEXT,             -- desktop, phone or tablet
  browser TEXT,
  visitor TEXT             -- daily hash
);
CREATE INDEX IF NOT EXISTS hits_by_day ON hits (day, kind);
