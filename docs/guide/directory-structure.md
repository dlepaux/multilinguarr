---
description: Understand the three-tier directory layout — downloads, storage, and library — required by multilinguarr.
---

# Directory Structure

multilinguarr requires a three-tier directory structure. This is the most important concept to understand before setting up.

## The three tiers

```
/srv/media/
  downloads/          ← arr download clients write here (temporary)
  storage/            ← arr moves completed files here (permanent)
    radarr-fr/
    radarr-en/
    sonarr-fr/
    sonarr-en/
  library/            ← multilinguarr manages this (symlinks/hardlinks)
    movies/fr/
    movies/en/
    tv/fr/
    tv/en/
```

| Tier         | Who manages it                      | What's in it                                                            |
| ------------ | ----------------------------------- | ----------------------------------------------------------------------- |
| `downloads/` | Download client (qBittorrent, etc.) | Temporary files being downloaded                                        |
| `storage/`   | Radarr/Sonarr                       | Completed, organized media — one directory per arr instance             |
| `library/`   | multilinguarr                       | Symlinks/hardlinks organized by language — media server reads from here |

## Why not downloads → library directly?

Most arr setups use two tiers: downloads → library (with hardlinks or moves). This works fine for single-language setups. But multilinguarr needs the middle `storage/` tier because:

1. **Each arr instance owns its storage** — Radarr-FR manages `storage/radarr-fr/`, Radarr-EN manages `storage/radarr-en/`
2. **multilinguarr never touches real files** — it only creates/removes links in `library/`
3. **Language routing happens at the link level** — the same file in `storage/radarr-fr/Movie (2024)/` can appear in both `library/movies/fr/` and `library/movies/en/` via symlinks

## Migration from a flat setup

If you currently have a single Radarr with `downloads/ → movies/`:

1. Create the new directory structure: `storage/radarr-fr/`, `library/movies/fr/`, etc.
2. Move your existing media from `movies/` into `storage/radarr-fr/`
3. In Radarr, update the root folder to `storage/radarr-fr/`
4. Set up multilinguarr and run a regeneration to create the library links

::: warning
Back up your media database before moving files. Radarr tracks files by path — changing the root folder requires a library refresh.
:::

## Docker volume mapping

All services (arr instances, multilinguarr, media server) must see the same paths. The simplest approach:

```yaml
volumes:
  - /srv/media:/srv/media
```

Mount the entire media tree at the same path in every container. This ensures webhook file paths match what multilinguarr and Jellyfin see on disk.

::: tip
If you use hardlinks, `storage/` and `library/` must be on the same filesystem. Symlinks work across filesystems.
:::
