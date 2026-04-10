---
layout: home

hero:
  name: multilinguarr
  text: Multi-language audio for the *arr stack
  tagline: Automatically sorts your media into language-specific libraries for Jellyfin, Plex, or any media server.
  image:
    src: /logo.svg
    alt: multilinguarr
  actions:
    - theme: brand
      text: Get Started
      link: /guide/introduction
    - theme: alt
      text: View on GitHub
      link: https://github.com/dlepaux/multilinguarr

features:
  - icon: "\U0001F50D"
    title: ffprobe detection
    details: Detects audio languages by analyzing actual audio tracks — not filenames or metadata guesses.
  - icon: "\U0001F517"
    title: Symlinks & hardlinks
    details: Creates language-specific library views without duplicating files. Choose symlinks or hardlinks per instance.
  - icon: "\U0001F30D"
    title: N languages
    details: Configure any number of languages — not locked to EN/FR. Add Spanish, German, Japanese, or any ISO 639 language.
  - icon: "\U0001F4E1"
    title: Webhook-driven
    details: Radarr and Sonarr trigger processing automatically via webhooks. No polling, no cron.
  - icon: "\U0001F4E6"
    title: API-first config
    details: Configure via REST API with self-documenting schema endpoints. No config files to mount.
  - icon: "\U0001F39E"
    title: Any media server
    details: Works with Jellyfin, Plex, or any server that scans library directories. Jellyfin gets native refresh integration.
---
