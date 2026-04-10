---
description: Migrate from a flat single-language arr setup to multilinguarr's three-tier multi-language directory structure.
---

# Migration Guide

This guide is for users migrating from a standard single-language arr setup to a multi-language setup with multilinguarr.

## Before you start

::: warning Back up your arr databases
Radarr and Sonarr track files by path. Changing root folders requires a library refresh. Back up your databases before proceeding.
:::

## Current setup (typical)

Most arr users have a two-tier structure:

```
/srv/media/
  downloads/        ← download client
  movies/           ← Radarr root folder + Jellyfin library
  tv/               ← Sonarr root folder + Jellyfin library
```

## Target setup

```
/srv/media/
  downloads/
  storage/
    radarr-fr/      ← Radarr-FR root folder
    radarr-en/      ← Radarr-EN root folder
    sonarr-fr/      ← Sonarr-FR root folder
    sonarr-en/      ← Sonarr-EN root folder
  library/
    movies/fr/      ← Jellyfin "Films" library
    movies/en/      ← Jellyfin "Movies" library
    tv/fr/          ← Jellyfin "Séries" library
    tv/en/          ← Jellyfin "Series" library
```

## Steps

### 1. Create the directory structure

```bash
mkdir -p /srv/media/storage/{radarr-fr,radarr-en,sonarr-fr,sonarr-en}
mkdir -p /srv/media/library/movies/{fr,en}
mkdir -p /srv/media/library/tv/{fr,en}
```

### 2. Move existing media

Your existing library becomes the primary language storage:

```bash
# Movies → primary storage
mv /srv/media/movies/* /srv/media/storage/radarr-fr/

# TV → primary storage
mv /srv/media/tv/* /srv/media/storage/sonarr-fr/
```

### 3. Update arr root folders

In Radarr-FR:

- Remove old root folder (`/srv/media/movies`)
- Add new root folder (`/srv/media/storage/radarr-fr`)
- Run "Update All" to refresh file paths

Repeat for each arr instance with its corresponding storage path.

### 4. Set up multilinguarr

Follow the [Installation](/guide/installation) and [Configuration](/guide/configuration) guides.

### 5. Run regeneration

```bash
# Preview first
curl -X POST "$API/api/v1/admin/regenerate?dry_run=true" \
  -H "X-Api-Key: $KEY"

# If the preview looks correct, run for real
curl -X POST "$API/api/v1/admin/regenerate" \
  -H "X-Api-Key: $KEY"
```

### 6. Update Jellyfin/Plex libraries

Point your media server libraries at the new `library/` paths:

| Library name | Path                           |
| ------------ | ------------------------------ |
| Films        | `/srv/media/library/movies/fr` |
| Movies       | `/srv/media/library/movies/en` |
| Séries       | `/srv/media/library/tv/fr`     |
| Series       | `/srv/media/library/tv/en`     |

Trigger a library scan in your media server.
