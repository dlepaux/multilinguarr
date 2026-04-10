---
description: Learn how multilinguarr processes Radarr/Sonarr webhooks, probes audio tracks with ffprobe, and creates library links.
---

# How It Works

## Event flow

```
┌──────────┐    webhook     ┌──────────────┐    ffprobe    ┌─────────┐
│ Radarr/  │ ──────────────►│              │ ─────────────►│ media   │
│ Sonarr   │    POST        │ multilinguarr│               │ file    │
└──────────┘    /webhook/   │              │◄──────────────│         │
                radarr-fr   │              │  audio tracks │         │
                            │              │               └─────────┘
                            │              │
                            │   ┌──────────┤
                            │   │ SQLite   │  job queue +
                            │   │ database │  config store
                            │   └──────────┤
                            │              │
                            │              │── symlink ──► library/movies/fr/
                            │              │── symlink ──► library/movies/en/
                            └──────────────┘
```

### Step by step

1. **Webhook received** — Radarr or Sonarr sends a POST to `/webhook/{instance}` when a file is downloaded, upgraded, or deleted
2. **Job enqueued** — the webhook payload is stored in SQLite and acknowledged immediately (200 OK with job ID)
3. **Worker claims job** — a background worker picks up the job from the queue
4. **ffprobe detection** — the handler runs ffprobe on the file path from the webhook payload to detect audio languages
5. **Link decision** — based on detected languages and the instance config:
   - **Multi-audio** (e.g., FR + EN): symlinks created in both FR and EN libraries
   - **Single language**: symlink created in the matching library only
   - **Primary single-language**: the alternate instance is told to fetch its own copy (cross-instance propagation)
6. **Upgrade handling** — if the webhook indicates an upgrade, old links are removed before creating new ones
7. **Delete handling** — when content is deleted, corresponding links are removed from all libraries

## Job queue

Every webhook becomes a row in the SQLite `jobs` table. This gives:

- **Crash recovery** — if the container restarts mid-processing, pending jobs resume
- **Retry with backoff** — transient failures (network, filesystem) are retried automatically
- **Audit trail** — every event is recorded with its payload, status, and any errors
- **Reprocessing** — `POST /api/v1/jobs/reprocess` replays all historical events

## Cross-instance propagation

When a primary instance (e.g., Radarr-FR) imports a single-language file:

1. multilinguarr creates a symlink in the primary library (FR)
2. It calls the alternate instance's API (Radarr-EN) to add the same movie
3. Radarr-EN searches for and downloads its own copy
4. When Radarr-EN's download completes, its webhook fires and multilinguarr creates the EN library link

This ensures each instance has its own file — no shared storage between instances.

## Regeneration

`POST /api/v1/admin/regenerate` walks all storage directories, runs ffprobe on every file, and rebuilds the entire link tree. Use this:

- After initial setup (to process existing media)
- After a config change (new instance or language added)
- For recovery (if links were accidentally deleted)

Add `?dry_run=true` to preview what would be done without touching the filesystem.
