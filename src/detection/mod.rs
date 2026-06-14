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
    /// All audio streams ffprobe reported, retained so callers can apply
    /// the language/role/channel base-track rule for a target language.
    pub audio_streams: Vec<AudioStream>,
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
        let result = self.ffprobe.probe(file_path, self.ffprobe_timeout).await;
        let outcome = match &result {
            Ok(_) => "success",
            Err(DetectionError::FfprobeTimeout { .. }) => "timeout",
            Err(_) => "error",
        };
        metrics::histogram!(
            crate::observability::names::FFPROBE_DURATION,
            "outcome" => outcome,
        )
        .record(start.elapsed().as_secs_f64());
        let streams = result?;
        let languages = self.languages_from_streams(&streams);
        Ok(DetectionResult {
            is_multi_audio: languages.len() >= 2,
            languages,
            audio_streams: streams,
        })
    }

    fn languages_from_streams(&self, streams: &[AudioStream]) -> HashSet<String> {
        let mut out = HashSet::new();
        for stream in streams {
            let Some(code) = stream.language.as_deref() else {
                continue;
            };
            if code.eq_ignore_ascii_case("und") {
                continue;
            }
            for key in self.languages.definitions.keys() {
                if self.code_matches_key(code, key) {
                    out.insert(key.clone());
                }
            }
        }
        out
    }

    /// True if an ISO 639-1/2 `code` maps to the configured language `key`.
    fn code_matches_key(&self, code: &str, key: &str) -> bool {
        self.languages.definitions.get(key).is_some_and(|def| {
            def.iso_639_1.iter().any(|c| c.eq_ignore_ascii_case(code))
                || def.iso_639_2.iter().any(|c| c.eq_ignore_ascii_case(code))
        })
    }

    /// True if any audio stream is a usable base track for `target`: a
    /// non-commentary stream in the target language — or untagged / `und`,
    /// treated as the instance language to mirror the import fallback —
    /// with a 5.1-or-lower (`<= 6`) channel layout. Unknown channel count
    /// fails open (counts as a base track) so a release is never rejected
    /// on missing ffprobe data.
    #[must_use]
    pub fn has_base_audio_track(&self, streams: &[AudioStream], target: &str) -> bool {
        streams.iter().any(|s| {
            let lang_ok = match s.language.as_deref() {
                None => true,
                Some(code) => {
                    code.eq_ignore_ascii_case("und") || self.code_matches_key(code, target)
                }
            };
            lang_ok && !s.is_commentary && s.channels.is_none_or(|c| c <= 6)
        })
    }
}
