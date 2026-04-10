---
description: Install multilinguarr with Docker (recommended) or build from source. Includes Docker Compose example and environment variables.
---

# Installation

## Docker (recommended)

```yaml
services:
  multilinguarr:
    image: ghcr.io/dlepaux/multilinguarr:latest
    environment:
      - MULTILINGUARR_API_KEY=your-secret-api-key
      - MULTILINGUARR_MEDIA_BASE_PATH=/srv/media
    volumes:
      - multilinguarr-data:/data
      - /srv/media:/srv/media
    ports:
      - "3100:3100"
    restart: unless-stopped

volumes:
  multilinguarr-data:
```

### Environment variables

| Variable                        | Required | Default                  | Description                                 |
| ------------------------------- | -------- | ------------------------ | ------------------------------------------- |
| `MULTILINGUARR_API_KEY`         | Yes      | —                        | API key for all authenticated endpoints     |
| `MULTILINGUARR_MEDIA_BASE_PATH` | Yes      | —                        | Root of your media directory tree           |
| `MULTILINGUARR_PORT`            | No       | `3100`                   | HTTP port                                   |
| `MULTILINGUARR_DATABASE_PATH`   | No       | `/data/multilinguarr.db` | SQLite database location                    |
| `MULTILINGUARR_LOG_LEVEL`       | No       | `info`                   | Log level (trace, debug, info, warn, error) |

### Volumes

| Mount        | Purpose                                                        |
| ------------ | -------------------------------------------------------------- |
| `/data`      | SQLite database (config + job queue) — persist across restarts |
| `/srv/media` | Media tree — must match arr instance and media server mounts   |

## From source

```bash
# Requires Rust 1.88+ and ffprobe
git clone https://github.com/dlepaux/multilinguarr.git
cd multilinguarr
cargo build --release
# Binary at target/release/multilinguarr
```

Then set the environment variables and run:

```bash
export MULTILINGUARR_API_KEY=your-secret
export MULTILINGUARR_MEDIA_BASE_PATH=/srv/media
./target/release/multilinguarr
```
