//! Unit tests for the language detection engine.
//!
//! Uses a stub prober so tests never shell out. Live ffprobe coverage
//! belongs to the E2E suite (story 10).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use crate::config::{LanguageDefinition, LanguagesConfig};

use super::{
    error::DetectionError,
    ffprobe::{AudioStream, FfprobeProber},
    parse_streams_json, LanguageDetector,
};

// ---------- fixtures ----------

fn en_fr_config() -> Arc<LanguagesConfig> {
    let mut defs = HashMap::new();
    defs.insert(
        "fr".to_owned(),
        LanguageDefinition {
            iso_639_1: vec!["fr".to_owned()],
            iso_639_2: vec!["fre".to_owned(), "fra".to_owned()],
            radarr_id: 2,
            sonarr_id: 2,
        },
    );
    defs.insert(
        "en".to_owned(),
        LanguageDefinition {
            iso_639_1: vec!["en".to_owned()],
            iso_639_2: vec!["eng".to_owned()],
            radarr_id: 1,
            sonarr_id: 1,
        },
    );
    Arc::new(LanguagesConfig {
        primary: "fr".to_owned(),
        alternates: vec!["en".to_owned()],
        definitions: defs,
    })
}

// ---------- stub ffprobe ----------

#[derive(Debug)]
struct StubFfprobe {
    response: Result<Vec<AudioStream>, DetectionError>,
}

impl StubFfprobe {
    fn new(response: Result<Vec<AudioStream>, DetectionError>) -> Self {
        Self { response }
    }
}

impl FfprobeProber for StubFfprobe {
    async fn probe(
        &self,
        _path: &Path,
        _timeout: Duration,
    ) -> Result<Vec<AudioStream>, DetectionError> {
        match &self.response {
            Ok(v) => Ok(v.clone()),
            Err(_) => Err(DetectionError::FfprobeUnavailable),
        }
    }
}

fn detector(stub: StubFfprobe) -> LanguageDetector<StubFfprobe> {
    LanguageDetector::new(en_fr_config(), stub)
}

// ---------- detection ----------

#[tokio::test]
async fn single_language_detected() {
    let det = detector(StubFfprobe::new(Ok(vec![AudioStream {
        language: Some("fra".to_owned()),
    }])));
    let result = det.detect(Path::new("/fake/video.mkv")).await.unwrap();
    assert!(result.languages.contains("fr"));
    assert_eq!(result.languages.len(), 1);
    assert!(!result.is_multi_audio);
}

#[tokio::test]
async fn multi_audio_detected() {
    let det = detector(StubFfprobe::new(Ok(vec![
        AudioStream {
            language: Some("eng".to_owned()),
        },
        AudioStream {
            language: Some("fra".to_owned()),
        },
    ])));
    let result = det.detect(Path::new("/fake/video.mkv")).await.unwrap();
    assert!(result.languages.contains("fr"));
    assert!(result.languages.contains("en"));
    assert!(result.is_multi_audio);
}

#[tokio::test]
async fn und_streams_dropped() {
    let det = detector(StubFfprobe::new(Ok(vec![
        AudioStream {
            language: Some("und".to_owned()),
        },
        AudioStream {
            language: Some("eng".to_owned()),
        },
    ])));
    let result = det.detect(&PathBuf::from("/x.mkv")).await.unwrap();
    assert!(result.languages.contains("en"));
    assert!(!result.languages.contains("und"));
    assert_eq!(result.languages.len(), 1);
}

#[tokio::test]
async fn no_language_tags_yields_empty() {
    let det = detector(StubFfprobe::new(Ok(vec![AudioStream { language: None }])));
    let result = det.detect(Path::new("/fake/video.mkv")).await.unwrap();
    assert!(result.languages.is_empty());
    assert!(!result.is_multi_audio);
}

#[tokio::test]
async fn ffprobe_error_propagates() {
    let det = detector(StubFfprobe::new(Err(DetectionError::FfprobeUnavailable)));
    let result = det.detect(Path::new("/fake/video.mkv")).await;
    assert!(result.is_err());
}

#[tokio::test]
async fn iso_639_1_codes_matched() {
    let det = detector(StubFfprobe::new(Ok(vec![AudioStream {
        language: Some("fr".to_owned()),
    }])));
    let result = det.detect(Path::new("/fake/video.mkv")).await.unwrap();
    assert!(result.languages.contains("fr"));
}

#[tokio::test]
async fn three_languages_detected() {
    let det = detector(StubFfprobe::new(Ok(vec![
        AudioStream {
            language: Some("eng".to_owned()),
        },
        AudioStream {
            language: Some("fra".to_owned()),
        },
        AudioStream {
            language: Some("spa".to_owned()),
        },
    ])));
    let result = det.detect(Path::new("/fake/video.mkv")).await.unwrap();
    // spa not in config, only en+fr matched
    assert_eq!(result.languages.len(), 2);
    assert!(result.is_multi_audio);
}

// ---------- parse_streams_json ----------

#[test]
fn parse_streams_empty_envelope() {
    let out = parse_streams_json(r#"{"streams":[]}"#).unwrap();
    assert!(out.is_empty());
}

#[test]
fn parse_streams_missing_streams_field_defaults_to_empty() {
    let out = parse_streams_json(r"{}").unwrap();
    assert!(out.is_empty());
}

#[test]
fn parse_streams_with_language_tags() {
    let json = r#"{
        "streams": [
            { "codec_type": "audio", "tags": { "language": "eng", "title": "English" } },
            { "codec_type": "audio", "tags": { "language": "fra" } }
        ]
    }"#;
    let out = parse_streams_json(json).unwrap();
    assert_eq!(out.len(), 2);
    assert_eq!(out[0].language.as_deref(), Some("eng"));
    assert_eq!(out[1].language.as_deref(), Some("fra"));
}

#[test]
fn parse_streams_missing_tags_yields_none_language() {
    let json = r#"{"streams": [{ "codec_type": "audio" }]}"#;
    let out = parse_streams_json(json).unwrap();
    assert_eq!(out.len(), 1);
    assert!(out[0].language.is_none());
}

#[test]
fn parse_streams_invalid_json_errors() {
    assert!(parse_streams_json("not json").is_err());
}
