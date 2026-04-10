//! Language detection engine.
//!
//! Runs `ffprobe` on the media file to determine which configured
//! languages are present. ffprobe is the single source of truth —
//! the arr API is not consulted for language metadata.

mod error;
mod ffprobe;

#[cfg(test)]
mod tests;

use std::collections::HashSet;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use crate::config::LanguagesConfig;

pub use error::DetectionError;
pub use ffprobe::{parse_streams_json, AudioStream, FfprobeProber, SystemFfprobe};

/// Default per-call ffprobe timeout.
pub const DEFAULT_FFPROBE_TIMEOUT: Duration = Duration::from_secs(30);

/// The outcome of [`LanguageDetector::detect`].
#[derive(Debug, Clone)]
pub struct DetectionResult {
    /// Set of language *keys* (as declared in `[languages.definitions]`)
    /// that were detected in the file.
    pub languages: HashSet<String>,
    /// `true` when the file contains two or more configured languages.
    pub is_multi_audio: bool,
}

/// The detection engine. Cloneable — holds `Arc`s internally.
#[derive(Debug, Clone)]
pub struct LanguageDetector<P: FfprobeProber = SystemFfprobe> {
    languages: Arc<LanguagesConfig>,
    ffprobe: Arc<P>,
    ffprobe_timeout: Duration,
}

impl<P: FfprobeProber> LanguageDetector<P> {
    /// Build a detector with the default ffprobe timeout.
    pub fn new(languages: Arc<LanguagesConfig>, ffprobe: P) -> Self {
        Self::with_timeout(languages, ffprobe, DEFAULT_FFPROBE_TIMEOUT)
    }

    /// Build a detector with a custom ffprobe timeout.
    pub fn with_timeout(
        languages: Arc<LanguagesConfig>,
        ffprobe: P,
        ffprobe_timeout: Duration,
    ) -> Self {
        Self {
            languages,
            ffprobe: Arc::new(ffprobe),
            ffprobe_timeout,
        }
    }

    /// Detect the languages present in a media file via ffprobe.
    ///
    /// # Errors
    ///
    /// Returns [`DetectionError`] if ffprobe fails to spawn, times out, exits non-zero, or returns unparseable output.
    pub async fn detect(&self, file_path: &Path) -> Result<DetectionResult, DetectionError> {
        let start = std::time::Instant::now();
        let streams = self.ffprobe.probe(file_path, self.ffprobe_timeout).await?;
        metrics::histogram!(crate::observability::names::FFPROBE_DURATION)
            .record(start.elapsed().as_secs_f64());
        let languages = self.languages_from_streams(&streams);
        Ok(DetectionResult {
            is_multi_audio: languages.len() >= 2,
            languages,
        })
    }

    fn languages_from_streams(&self, streams: &[AudioStream]) -> HashSet<String> {
        let mut out = HashSet::new();
        for stream in streams {
            let Some(code) = stream.language.as_deref() else {
                continue;
            };
            let code_lower = code.to_ascii_lowercase();
            if code_lower == "und" {
                continue;
            }
            for (key, def) in &self.languages.definitions {
                let one = def
                    .iso_639_1
                    .iter()
                    .any(|c| c.eq_ignore_ascii_case(&code_lower));
                let two = def
                    .iso_639_2
                    .iter()
                    .any(|c| c.eq_ignore_ascii_case(&code_lower));
                if one || two {
                    out.insert(key.clone());
                }
            }
        }
        out
    }
}
