-- Config tables: API-managed configuration stored alongside the job queue.
--
-- Bootstrap env vars (MLQ_PORT, MLQ_API_KEY, MLQ_MEDIA_BASE_PATH) are
-- NOT stored here — they are read from the process environment at startup.
-- Everything else (languages, instances, jellyfin) is managed via the
-- /api/v1/* endpoints and persisted in these tables.

-- Key-value bag for general settings (primary_language, queue_concurrency).
CREATE TABLE IF NOT EXISTS config (
    key   TEXT PRIMARY KEY,
    value TEXT NOT NULL
);

-- Language definitions — keyed by short code (e.g. "fr", "en", "es").
CREATE TABLE IF NOT EXISTS languages (
    key        TEXT    PRIMARY KEY,
    iso_639_1  TEXT    NOT NULL,  -- JSON array: ["fr"]
    iso_639_2  TEXT    NOT NULL,  -- JSON array: ["fre", "fra"]
    radarr_id  INTEGER NOT NULL,
    sonarr_id  INTEGER NOT NULL
);

-- Arr instances — one per Radarr/Sonarr service.
CREATE TABLE IF NOT EXISTS instances (
    name             TEXT PRIMARY KEY,
    kind             TEXT NOT NULL CHECK (kind IN ('radarr', 'sonarr')),
    language         TEXT NOT NULL REFERENCES languages(key),
    url              TEXT NOT NULL,
    api_key          TEXT NOT NULL,
    storage_path     TEXT NOT NULL,
    library_path     TEXT NOT NULL,
    link_strategy    TEXT NOT NULL CHECK (link_strategy IN ('symlink', 'hardlink')),
    propagate_delete INTEGER NOT NULL DEFAULT 1
);

-- Jellyfin config — singleton (at most one row).
CREATE TABLE IF NOT EXISTS jellyfin (
    id      INTEGER PRIMARY KEY CHECK (id = 1),
    url     TEXT NOT NULL,
    api_key TEXT NOT NULL
);

-- Setup completion flag — inserted by POST /api/v1/setup/complete.
-- Presence of this row means webhook processing is active.
CREATE TABLE IF NOT EXISTS setup (
    id           INTEGER PRIMARY KEY CHECK (id = 1),
    completed_at TEXT    NOT NULL
);
