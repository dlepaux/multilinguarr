---
description: Configure multilinguarr entirely via its REST API — define languages, register arr instances, and set up webhook endpoints.
---

# Configuration

multilinguarr is configured entirely via its REST API. No config files to mount. The API is self-documenting via schema endpoints.

## Setup flow

After starting the container:

### 1. Define languages

```bash
API="http://localhost:3100"
KEY="your-secret-api-key"

# Check what fields are expected
curl "$API/api/v1/languages/schema"

# Add French
curl -X POST "$API/api/v1/languages" \
  -H "X-Api-Key: $KEY" -H "Content-Type: application/json" \
  -d '{
    "key": "fr",
    "iso_639_1": ["fr"],
    "iso_639_2": ["fre", "fra"],
    "radarr_id": 2,
    "sonarr_id": 2
  }'

# Add English
curl -X POST "$API/api/v1/languages" \
  -H "X-Api-Key: $KEY" -H "Content-Type: application/json" \
  -d '{
    "key": "en",
    "iso_639_1": ["en"],
    "iso_639_2": ["eng"],
    "radarr_id": 1,
    "sonarr_id": 1
  }'
```

::: tip Finding language IDs
`radarr_id` and `sonarr_id` are the internal language IDs used by your arr instances. You can find them in the arr API at `/api/v3/language`.
:::

### 2. Set primary language

```bash
curl -X PUT "$API/api/v1/config" \
  -H "X-Api-Key: $KEY" -H "Content-Type: application/json" \
  -d '{"primary_language": "fr", "queue_concurrency": 2}'
```

::: warning
`queue_concurrency` must be at least 1. Default is 2.
:::

The primary language is the one your main arr instance downloads. Alternates fetch their own copies when a single-language file is imported.

### 3. Add instances

```bash
# Check the expected shape
curl "$API/api/v1/instances/schema"

# Radarr FR (primary)
curl -X POST "$API/api/v1/instances" \
  -H "X-Api-Key: $KEY" -H "Content-Type: application/json" \
  -d '{
    "name": "radarr-fr",
    "type": "radarr",
    "language": "fr",
    "url": "http://radarr-fr:7878",
    "api_key": "your-radarr-fr-api-key",
    "storage_path": "/srv/media/storage/radarr-fr",
    "library_path": "/srv/media/library/movies/fr",
    "link_strategy": "symlink",
    "propagate_delete": true
  }'

# Radarr EN (alternate)
curl -X POST "$API/api/v1/instances" \
  -H "X-Api-Key: $KEY" -H "Content-Type: application/json" \
  -d '{
    "name": "radarr-en",
    "type": "radarr",
    "language": "en",
    "url": "http://radarr-en:7878",
    "api_key": "your-radarr-en-api-key",
    "storage_path": "/srv/media/storage/radarr-en",
    "library_path": "/srv/media/library/movies/en",
    "link_strategy": "symlink",
    "propagate_delete": true
  }'

```

When `propagate_delete` is `true` (default), deletes on this instance are propagated to other instances of the same type.

### 4. Complete setup

```bash
# Check what's missing
curl "$API/api/v1/setup/status" -H "X-Api-Key: $KEY"

# Mark setup as complete
curl -X POST "$API/api/v1/setup/complete" -H "X-Api-Key: $KEY"
```

### 5. Configure webhooks in arr

In each Radarr/Sonarr instance, add a webhook notification:

- **URL**: `http://multilinguarr:3100/webhook/{instance-name}`
  - e.g., `http://multilinguarr:3100/webhook/radarr-fr`
- **Events**: On Download, On Upgrade, On Delete

## Schema endpoints

Every resource has a schema endpoint that returns field definitions. No auth required:

```bash
curl http://localhost:3100/api/v1/languages/schema
curl http://localhost:3100/api/v1/instances/schema
curl http://localhost:3100/api/v1/config/schema
```

These are designed to be consumed by seed scripts, AI agents, or humans exploring the API.
