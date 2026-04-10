---
description: multilinguarr enforces multi-language audio in the *arr stack, routing media into per-language libraries for Jellyfin, Plex, or any media server.
---

# Introduction

multilinguarr enforces multi-language audio in the \*arr media stack. When Radarr or Sonarr downloads a file, multilinguarr detects its audio languages via ffprobe and creates symlinks (or hardlinks) into language-specific media libraries.

## The problem

You have Radarr downloading movies in French and English. Your media server shows them all in one library. You want separate libraries per language — "Films" for French, "Movies" for English — so each family member sees content in their language.

## The solution

multilinguarr sits between your arr instances and your media server:

```
Radarr/Sonarr → webhook → multilinguarr → ffprobe → symlinks → Jellyfin/Plex
```

1. Your arr instance downloads a file and sends a webhook
2. multilinguarr runs ffprobe to detect audio languages
3. Based on the result, it creates symlinks in the right library directories
4. Your media server sees the files in language-specific libraries

**Multi-audio files** (e.g., French + English tracks) appear in both libraries. **Single-language files** appear only in their matching library, and multilinguarr tells the other arr instance to fetch its own copy.

## Key design principles

- **ffprobe is the source of truth** — not filenames, not arr metadata. The actual audio tracks decide.
- **No file duplication** — symlinks or hardlinks point to the original file. Zero extra disk space.
- **API-first** — no config files. Configure via REST API with self-documenting schema endpoints.
- **Webhook-driven** — no polling, no cron. Instant processing when arr completes a download.

## Compatibility

| Software | Support                            |
| -------- | ---------------------------------- |
| Radarr   | v3+                                |
| Sonarr   | v3+                                |
| Jellyfin | Native library refresh integration |
| Plex     | Directory scanning — transparent   |
| Others   | Any server that scans directories  |

## Next step

Ready to install? Head to the [Installation guide](/guide/installation).
