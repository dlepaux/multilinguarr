//! Fixture management — pre-generated `.mkv` files in `tests/media/`.
//!
//! CI generates fixtures via ffmpeg (see `.github/workflows/ci.yml`).
//! For local runs, generate them with `scripts/generate-fixtures.sh`.
//!
//! If the fixtures are missing, the test harness fails early with a
//! clear error pointing at the generate script.

use std::path::PathBuf;

use super::Result;

/// Tree layout inside `tests/media/`.
#[derive(Debug, Clone)]
pub struct Fixtures {
    pub multi_audio: PathBuf,
    pub english_only: PathBuf,
    pub french_only: PathBuf,
    pub episode_multi: PathBuf,
    pub episode_en: PathBuf,
    pub episode_fr: PathBuf,
}

impl Fixtures {
    /// Locate the fixture directory at `tests/media/` relative to the
    /// crate root. Errors if any file is missing.
    pub async fn locate() -> Result<Self> {
        let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let media_dir = manifest_dir.join("tests/media");
        let multi_audio = media_dir.join("test-movie-multi.mkv");
        let english_only = media_dir.join("test-movie-en.mkv");
        let french_only = media_dir.join("test-movie-fr.mkv");
        let episode_multi = media_dir.join("test-episode-multi.mkv");
        let episode_en = media_dir.join("test-episode-en.mkv");
        let episode_fr = media_dir.join("test-episode-fr.mkv");

        for path in [
            &multi_audio,
            &english_only,
            &french_only,
            &episode_multi,
            &episode_en,
            &episode_fr,
        ] {
            if !tokio::fs::try_exists(path).await.unwrap_or(false) {
                return Err(format!(
                    "fixture {} missing — run \
                     `scripts/generate-fixtures.sh` to regenerate",
                    path.display()
                )
                .into());
            }
        }

        Ok(Self {
            multi_audio,
            english_only,
            french_only,
            episode_multi,
            episode_en,
            episode_fr,
        })
    }
}
