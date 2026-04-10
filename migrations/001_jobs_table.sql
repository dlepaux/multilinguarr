-- Jobs table: persistent queue with visibility-timeout lease.
--
-- Every webhook/admin-initiated unit of work becomes a row here. The
-- worker pool claims rows with `claim_next`, processes them, and
-- transitions them to `completed`, `pending` (retry), or `dead_letter`.
-- A sweeper task resets expired claims back to `pending` so work
-- orphaned by a crashed worker is recovered automatically.

CREATE TABLE IF NOT EXISTS jobs (
    id               INTEGER  PRIMARY KEY AUTOINCREMENT,

    -- What kind of work. The handler registry (story 08) dispatches on
    -- this value. Stored as TEXT to keep the schema human-readable in a
    -- sqlite browser.
    kind             TEXT     NOT NULL,

    -- JSON payload — shape is owned by the handler that consumes `kind`.
    payload          TEXT     NOT NULL,

    -- Status machine:
    --   pending     — waiting to be claimed
    --   claimed     — currently held by a worker (see claimed_until)
    --   completed   — finished successfully, kept for audit
    --   failed      — permanent error, will not be retried, kept for audit
    --   dead_letter — exhausted max_attempts, kept for manual replay
    status           TEXT     NOT NULL CHECK (
        status IN ('pending', 'claimed', 'completed', 'failed', 'dead_letter')
    ),

    attempts         INTEGER  NOT NULL DEFAULT 0,
    max_attempts     INTEGER  NOT NULL DEFAULT 5,

    -- When the job becomes eligible to be claimed. For a fresh job,
    -- this is the creation time; after a transient failure, it is
    -- bumped to now + exponential backoff.
    next_attempt_at  TEXT     NOT NULL,

    -- Lease deadline for claimed rows. NULL for any non-claimed row.
    claimed_until    TEXT,

    -- Identity of the worker currently holding the claim. Useful when
    -- debugging stuck jobs in a multi-worker setup.
    claimed_by       TEXT,

    -- Last error message (stringified ArrError / LinkError / etc.).
    last_error       TEXT,

    created_at       TEXT     NOT NULL,
    updated_at       TEXT     NOT NULL,
    completed_at     TEXT
);

-- Hot path: "give me the next claimable job". Query shape is
--   SELECT ... WHERE status = 'pending' AND next_attempt_at <= ?
--   ORDER BY next_attempt_at, id LIMIT 1
-- Index on (status, next_attempt_at) serves it directly.
CREATE INDEX IF NOT EXISTS idx_jobs_status_next_attempt_at
    ON jobs (status, next_attempt_at);

-- Sweeper path: "any rows whose lease has expired?". Covered by the
-- same index because status is a prefix, but claimed rows are scanned
-- by claimed_until.
CREATE INDEX IF NOT EXISTS idx_jobs_status_claimed_until
    ON jobs (status, claimed_until);
