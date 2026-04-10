<p align="center">
  <img src="brand/icon.svg" width="120" alt="multilinguarr" />
</p>

<h1 align="center">multilinguarr</h1>

<p align="center">
  Multi-language audio enforcement for the *arr media stack.
</p>

<p align="center">
  <a href="https://github.com/dlepaux/multilinguarr/actions/workflows/ci.yml"><img src="https://github.com/dlepaux/multilinguarr/actions/workflows/ci.yml/badge.svg" alt="CI" /></a>
  <a href="https://github.com/dlepaux/multilinguarr/pkgs/container/multilinguarr"><img src="https://img.shields.io/badge/ghcr.io-multilinguarr-blue?logo=docker" alt="Docker" /></a>
  <a href="https://dlepaux.github.io/multilinguarr/"><img src="https://img.shields.io/badge/docs-vitepress-purple" alt="Docs" /></a>
  <a href="license.md"><img src="https://img.shields.io/badge/license-MIT-green" alt="License" /></a>
</p>

---

## Why?

Media files are messy. Audio language metadata is unreliable — mislabeled tracks, missing tags, inconsistent naming across indexers. Radarr and Sonarr do a great job managing downloads, but they have no concept of per-language media libraries.

If you want separate Jellyfin/Plex libraries for each language (e.g. French movies, English movies), you're on your own. Manual sorting doesn't scale, and metadata-based smart playlists break the moment a file has the wrong tag.

**multilinguarr** solves this by sitting between your *arr instances and your media player. It intercepts download webhooks, detects actual audio languages via ffprobe, and creates symlinks (or hardlinks) into language-specific library directories. Your media player sees clean, per-language libraries. Your *arr instances keep managing the real files. Nothing gets moved or modified.

## Quick start

```yaml
services:
  multilinguarr:
    image: ghcr.io/dlepaux/multilinguarr:latest
    environment:
      - MULTILINGUARR_API_KEY=your-secret
      - MULTILINGUARR_MEDIA_BASE_PATH=/srv/media
    volumes:
      - ./data:/data              # SQLite database
      - /srv/media:/srv/media     # media tree (same mount as arr instances)
    ports:
      - "3100:3100"
```

Configure languages, instances, and webhooks via the REST API — see the **[documentation](https://dlepaux.github.io/multilinguarr/)** for the full setup guide.

## Documentation

Full documentation is available at **[dlepaux.github.io/multilinguarr](https://dlepaux.github.io/multilinguarr/)**:

- [Introduction](https://dlepaux.github.io/multilinguarr/guide/introduction) — how it works, architecture overview
- [Installation](https://dlepaux.github.io/multilinguarr/guide/installation) — Docker setup, configuration
- [Directory structure](https://dlepaux.github.io/multilinguarr/guide/directory-structure) — required media layout
- [API reference](https://dlepaux.github.io/multilinguarr/api/) — interactive OpenAPI docs

## Compatibility

| Service | Version |
|---------|---------|
| Radarr | v3+ |
| Sonarr | v3+ |
| Jellyfin | any |
| Plex | any |

## License

[MIT](license.md)
