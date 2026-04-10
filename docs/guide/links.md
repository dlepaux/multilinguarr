---
description: Choose between symlinks and hardlinks for multilinguarr library files — trade-offs, filesystem requirements, and configuration.
---

# Symlinks vs Hardlinks

multilinguarr supports both symlinks and hardlinks, configurable per instance.

## Comparison

|                        | Symlink                                           | Hardlink                                       |
| ---------------------- | ------------------------------------------------- | ---------------------------------------------- |
| Cross-filesystem       | Yes                                               | No — must be same filesystem                   |
| Disk space             | Zero extra                                        | Zero extra                                     |
| Breaks if source moves | Yes                                               | No — both are equal references                 |
| Media server support   | Universal                                         | Universal                                      |
| Use when               | Storage and library on different disks/partitions | Same filesystem (typical single-disk or btrfs) |

## Configuration

Set `link_strategy` when creating an instance:

```bash
curl -X POST "$API/api/v1/instances" \
  -H "X-Api-Key: $KEY" -H "Content-Type: application/json" \
  -d '{
    "name": "radarr-fr",
    "type": "radarr",
    "language": "fr",
    "url": "http://radarr-fr:7878",
    "api_key": "...",
    "storage_path": "/srv/media/storage/radarr-fr",
    "library_path": "/srv/media/library/movies/fr",
    "link_strategy": "hardlink"
  }'
```

## How they work

### Symlinks (movies)

A directory symlink is created in the library pointing to the storage folder:

```text
library/movies/fr/Movie (2024)/  →  storage/radarr-fr/Movie (2024)/
```

### Hardlinks (movies)

The directory structure is mirrored and each file is hardlinked individually:

```text
library/movies/fr/Movie (2024)/movie.mkv  ←→  storage/radarr-fr/Movie (2024)/movie.mkv
```

Both files share the same inode — modifying or deleting one does not affect the other (until the last reference is removed).

### Episodes

TV episodes are always linked at the file level (both symlink and hardlink), with season directories created as needed:

```text
library/tv/fr/Show/Season 01/S01E01.mkv  →  storage/sonarr-fr/Show/Season 01/S01E01.mkv
```
