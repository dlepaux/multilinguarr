-- Backfill recipe: surface jobs historically dropped by the
-- cross-instance 409 race that was fixed at the client layer.
--
-- Before the fix, a 409 UNIQUE-constraint response on cross-instance
-- add_series / add_movie was classified as a permanent error. The
-- queue worker marked the job as `failed` and abandoned its
-- downstream work (cross-library symlink, Jellyfin refresh).
--
-- This query lists those jobs by tvdb_id / tmdb_id and the affected
-- file id so the operator can manually re-trigger from Sonarr /
-- Radarr (re-run "Manual Import" or "Process" on the relevant file).
--
-- Usage:
--     sqlite3 /path/to/multilinguarr.db < backfill-409-race-incidents.sql
--
-- Out of scope: auto-replay. Operator decision per row — some files
-- may since have been deleted, replaced, or re-imported through other
-- paths.

SELECT
    id                                                AS job_id,
    kind,
    json_extract(payload, '$.instance')               AS source_instance,
    json_extract(payload, '$.event.series.tvdbId')    AS tvdb_id,
    json_extract(payload, '$.event.movie.tmdbId')     AS tmdb_id,
    json_extract(payload, '$.event.episodeFile.id')   AS episode_file_id,
    json_extract(payload, '$.event.movieFile.id')     AS movie_file_id,
    completed_at,
    last_error
FROM jobs
WHERE status = 'failed'
  AND last_error LIKE '%UNIQUE constraint failed%'
ORDER BY created_at;
