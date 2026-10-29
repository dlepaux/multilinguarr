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
    parse_streams_json, LanguageDetector, ProbeReport,
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
    fn probe(
        &self,
        _path: &Path,
        _timeout: Duration,
    ) -> impl std::future::Future<Output = Result<ProbeReport, DetectionError>> + Send {
        std::future::ready(match &self.response {
            Ok(v) => Ok(v.clone().into()),
            Err(_) => Err(DetectionError::FfprobeUnavailable),
        })
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
        channels: None,
        is_commentary: false,
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
            channels: None,
            is_commentary: false,
        },
        AudioStream {
            language: Some("fra".to_owned()),
            channels: None,
            is_commentary: false,
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
            channels: None,
            is_commentary: false,
        },
        AudioStream {
            language: Some("eng".to_owned()),
            channels: None,
            is_commentary: false,
        },
    ])));
    let result = det.detect(&PathBuf::from("/x.mkv")).await.unwrap();
    assert!(result.languages.contains("en"));
    assert!(!result.languages.contains("und"));
    assert_eq!(result.languages.len(), 1);
}

#[tokio::test]
async fn no_language_tags_yields_empty() {
    let det = detector(StubFfprobe::new(Ok(vec![AudioStream {
        language: None,
        channels: None,
        is_commentary: false,
    }])));
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
        channels: None,
        is_commentary: false,
    }])));
    let result = det.detect(Path::new("/fake/video.mkv")).await.unwrap();
    assert!(result.languages.contains("fr"));
}

#[tokio::test]
async fn three_languages_detected() {
    let det = detector(StubFfprobe::new(Ok(vec![
        AudioStream {
            language: Some("eng".to_owned()),
            channels: None,
            is_commentary: false,
        },
        AudioStream {
            language: Some("fra".to_owned()),
            channels: None,
            is_commentary: false,
        },
        AudioStream {
            language: Some("spa".to_owned()),
            channels: None,
            is_commentary: false,
        },
    ])));
    let result = det.detect(Path::new("/fake/video.mkv")).await.unwrap();
    // spa not in config, only en+fr matched
    assert_eq!(result.languages.len(), 2);
    assert!(result.is_multi_audio);
}

// ---------- has_base_audio_track (language + role + channel rule) ----------

#[tokio::test]
async fn base_track_rule_is_language_role_and_channel_aware() {
    // en target: 7.1 eng main + 2ch eng commentary + 6ch fre -> FAIL
    // (only eng non-commentary stream is 8ch; the 6ch one is the wrong language)
    let det = detector(StubFfprobe::new(Ok(vec![
        AudioStream {
            language: Some("eng".to_owned()),
            channels: Some(8),
            is_commentary: false,
        },
        AudioStream {
            language: Some("eng".to_owned()),
            channels: Some(2),
            is_commentary: true,
        },
        AudioStream {
            language: Some("fre".to_owned()),
            channels: Some(6),
            is_commentary: false,
        },
    ])));
    let r = det.detect(Path::new("/x.mkv")).await.unwrap();
    assert!(!det.has_base_audio_track(&r.audio_streams, "en"));

    // en target: a 6ch eng non-commentary stream present -> PASS
    let det = detector(StubFfprobe::new(Ok(vec![AudioStream {
        language: Some("eng".to_owned()),
        channels: Some(6),
        is_commentary: false,
    }])));
    let r = det.detect(Path::new("/x.mkv")).await.unwrap();
    assert!(det.has_base_audio_track(&r.audio_streams, "en"));

    // fre-only 6ch, en target -> FAIL (wrong-language base track)
    let det = detector(StubFfprobe::new(Ok(vec![AudioStream {
        language: Some("fre".to_owned()),
        channels: Some(6),
        is_commentary: false,
    }])));
    let r = det.detect(Path::new("/x.mkv")).await.unwrap();
    assert!(!det.has_base_audio_track(&r.audio_streams, "en"));

    // untagged + unknown channels -> fail-open PASS (never false-reject on missing data)
    let det = detector(StubFfprobe::new(Ok(vec![AudioStream {
        language: None,
        channels: None,
        is_commentary: false,
    }])));
    let r = det.detect(Path::new("/x.mkv")).await.unwrap();
    assert!(det.has_base_audio_track(&r.audio_streams, "en"));

    // und main track, 6ch -> PASS (undetermined treated as instance language)
    let det = detector(StubFfprobe::new(Ok(vec![AudioStream {
        language: Some("und".to_owned()),
        channels: Some(6),
        is_commentary: false,
    }])));
    let r = det.detect(Path::new("/x.mkv")).await.unwrap();
    assert!(det.has_base_audio_track(&r.audio_streams, "en"));
}

// ---------- parse_streams_json ----------

#[test]
fn parse_streams_empty_envelope() {
    let out = parse_streams_json(r#"{"streams":[]}"#).unwrap();
    assert!(out.audio_streams.is_empty());
}

#[test]
fn parse_streams_missing_streams_field_defaults_to_empty() {
    let out = parse_streams_json(r"{}").unwrap();
    assert!(out.audio_streams.is_empty());
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
    assert_eq!(out.audio_streams.len(), 2);
    assert_eq!(out.audio_streams[0].language.as_deref(), Some("eng"));
    assert_eq!(out.audio_streams[1].language.as_deref(), Some("fra"));
}

#[test]
fn parse_streams_missing_tags_yields_none_language() {
    let json = r#"{"streams": [{ "codec_type": "audio" }]}"#;
    let out = parse_streams_json(json).unwrap();
    assert_eq!(out.audio_streams.len(), 1);
    assert!(out.audio_streams[0].language.is_none());
}

#[test]
fn parse_streams_invalid_json_errors() {
    assert!(parse_streams_json("not json").is_err());
}

#[test]
fn parse_streams_captures_channels_and_disposition_commentary() {
    let json = r#"{
        "streams": [
            { "codec_type": "audio", "channels": 8, "disposition": { "comment": 0, "visual_impaired": 0 }, "tags": { "language": "eng" } },
            { "codec_type": "audio", "channels": 2, "disposition": { "comment": 1, "visual_impaired": 0 }, "tags": { "language": "eng", "title": "Director's Track" } },
            { "codec_type": "audio", "channels": 6, "tags": { "language": "fre" } }
        ]
    }"#;
    let out = parse_streams_json(json).unwrap();
    assert_eq!(out.audio_streams[0].channels, Some(8));
    assert!(!out.audio_streams[0].is_commentary);
    assert_eq!(out.audio_streams[1].channels, Some(2));
    assert!(out.audio_streams[1].is_commentary); // disposition.comment == 1
    assert_eq!(out.audio_streams[2].channels, Some(6));
    assert!(!out.audio_streams[2].is_commentary); // missing disposition tolerated
}

#[test]
fn parse_streams_commentary_detected_by_title() {
    let json = r#"{"streams":[
        { "codec_type": "audio", "channels": 2, "tags": { "language": "eng", "title": "Audio Commentary" } },
        { "codec_type": "audio", "channels": 2, "tags": { "language": "eng", "title": "Visually Impaired" } },
        { "codec_type": "audio", "channels": 2, "tags": { "language": "eng", "title": "Stereo" } }
    ]}"#;
    let out = parse_streams_json(json).unwrap();
    assert!(out.audio_streams[0].is_commentary); // "commentary" in title
    assert!(out.audio_streams[1].is_commentary); // "visually impaired" in title
    assert!(!out.audio_streams[2].is_commentary);
}

#[test]
fn parse_streams_keeps_only_audio_streams() {
    let json = r#"{"streams":[
        { "codec_type": "video", "codec_name": "hevc", "extradata_size": 110, "tags": { "language": "eng" } },
        { "codec_type": "audio", "channels": 6, "tags": { "language": "fre" } },
        { "codec_type": "subtitle", "tags": { "language": "eng" } }
    ]}"#;
    let out = parse_streams_json(json).unwrap();
    assert_eq!(out.audio_streams.len(), 1);
    assert_eq!(out.audio_streams[0].language.as_deref(), Some("fre"));
}

#[test]
fn parse_streams_reports_the_hevc_header_size() {
    let json = r#"{"streams":[
        { "codec_type": "video", "codec_name": "hevc", "extradata_size": 23 },
        { "codec_type": "audio", "channels": 2 }
    ]}"#;
    let out = parse_streams_json(json).unwrap();
    assert_eq!(out.hevc_header_bytes, Some(23));
}

#[test]
fn parse_streams_reports_no_hevc_header_for_other_codecs() {
    let json = r#"{"streams":[
        { "codec_type": "video", "codec_name": "h264", "extradata_size": 23 },
        { "codec_type": "video", "codec_name": "mjpeg", "disposition": { "attached_pic": 1 } }
    ]}"#;
    let out = parse_streams_json(json).unwrap();
    assert_eq!(out.hevc_header_bytes, None);
}

#[test]
fn hevc_header_is_empty_below_a_parameter_set() {
    let report = |bytes| ProbeReport {
        audio_streams: vec![],
        hevc_header_bytes: bytes,
    };
    assert!(report(Some(23)).has_empty_hevc_header());
    assert!(!report(Some(110)).has_empty_hevc_header());
    assert!(!report(None).has_empty_hevc_header());
}
