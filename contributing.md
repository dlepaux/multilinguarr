# Contributing

Thanks for considering contributing to multilinguarr.

## Development setup

### Prerequisites

- Rust 1.88+ (see `rust-toolchain.toml`)
- ffmpeg/ffprobe (for language detection)
- Docker (for E2E tests)

### Getting started

```bash
# Clone
git clone https://github.com/dlepaux/multilinguarr.git
cd multilinguarr

# Enable git hooks (fmt + clippy + test on pre-commit)
git config core.hooksPath .githooks

# Run unit tests
cargo test

# Run clippy
cargo clippy --all-targets --features e2e -- -D warnings

# Run E2E tests (requires Docker + test fixtures)
# Generate fixtures first:
mkdir -p tests/media
cd tests/media
bash ../../scripts/generate-fixtures.sh
cd -

cargo test --features e2e --test e2e
```

### Code quality

- `cargo fmt` before committing
- `cargo clippy -- -D warnings` must pass with zero warnings
- No `#[allow]` unless structurally unavoidable (document why)
- Tests for new features and bug fixes

### Commit conventions

[Conventional commits](https://www.conventionalcommits.org/):

```
feat(config): add language validation on instance creation
fix(handler): prevent symlink creation for undetermined languages
docs: update readme with docker-compose example
refactor(detection): simplify to ffprobe-only
test(e2e): add upgrade scenario for series
```

### Architecture

Multilinguarr enforces multi-language audio in the *arr media stack. When Radarr/Sonarr downloads a file:

1. Webhook arrives with file path
2. ffprobe detects audio languages
3. Symlinks/hardlinks created in language-specific library directories
4. Jellyfin/Plex sees them as separate libraries per language

Key design decisions:
- **ffprobe is the single source of truth** for language detection (not arr API metadata)
- **Webhook payload provides file paths** (no arr API calls on the hot path)
- **SQLite** for both job queue and configuration persistence
- **API-first configuration** (no config files to mount)

### Pull requests

- One concern per PR
- Include tests
- Update docs if behavior changes
- CI must pass (fmt + clippy + unit tests + E2E)
