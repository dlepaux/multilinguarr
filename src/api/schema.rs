//! Schema discovery endpoints — self-documenting field definitions.
//!
//! No auth required. Any consumer (seed script, AI agent, human with
//! curl) can discover the expected shape before `POSTing`.

use axum::response::IntoResponse;
use axum::Json;
use serde::Serialize;
use serde_json::json;

#[derive(Debug, Serialize)]
struct Field {
    name: &'static str,
    #[serde(rename = "type")]
    value_type: &'static str,
    required: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    default: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    options: Option<Vec<&'static str>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    example: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    hint: Option<&'static str>,
}

#[utoipa::path(
    get,
    path = "/api/v1/languages/schema",
    tag = "schema",
    responses(
        (status = 200, description = "Language field definitions"),
    ),
)]
pub async fn languages_schema() -> impl IntoResponse {
    Json(json!({
        "fields": [
            Field {
                name: "key",
                value_type: "string",
                required: true,
                default: None,
                options: None,
                example: Some("fr"),
                hint: Some("Short code used as reference in instances and config"),
            },
            Field {
                name: "iso_639_1",
                value_type: "string[]",
                required: true,
                default: None,
                options: None,
                example: Some("[\"fr\"]"),
                hint: Some("ISO 639-1 codes for ffprobe matching"),
            },
            Field {
                name: "iso_639_2",
                value_type: "string[]",
                required: true,
                default: None,
                options: None,
                example: Some("[\"fre\", \"fra\"]"),
                hint: Some("ISO 639-2 codes for ffprobe matching"),
            },
            Field {
                name: "radarr_id",
                value_type: "integer",
                required: true,
                default: None,
                options: None,
                example: Some("2"),
                hint: Some("Radarr language ID (from Radarr API)"),
            },
            Field {
                name: "sonarr_id",
                value_type: "integer",
                required: true,
                default: None,
                options: None,
                example: Some("2"),
                hint: Some("Sonarr language ID (from Sonarr API)"),
            },
        ]
    }))
}

#[utoipa::path(
    get,
    path = "/api/v1/instances/schema",
    tag = "schema",
    responses(
        (status = 200, description = "Instance field definitions"),
    ),
)]
pub async fn instances_schema() -> impl IntoResponse {
    Json(json!({
        "fields": [
            Field {
                name: "name",
                value_type: "string",
                required: true,
                default: None,
                options: None,
                example: Some("radarr-fr"),
                hint: Some("Unique identifier, used in webhook URL path"),
            },
            Field {
                name: "type",
                value_type: "select",
                required: true,
                default: None,
                options: Some(vec!["radarr", "sonarr"]),
                example: None,
                hint: None,
            },
            Field {
                name: "language",
                value_type: "string",
                required: true,
                default: None,
                options: None,
                example: Some("fr"),
                hint: Some("Language key from /api/v1/languages"),
            },
            Field {
                name: "url",
                value_type: "url",
                required: true,
                default: None,
                options: None,
                example: Some("http://radarr-fr:7878"),
                hint: None,
            },
            Field {
                name: "api_key",
                value_type: "password",
                required: true,
                default: None,
                options: None,
                example: None,
                hint: None,
            },
            Field {
                name: "storage_path",
                value_type: "path",
                required: true,
                default: None,
                options: None,
                example: Some("/srv/media/storage/radarr-fr"),
                hint: None,
            },
            Field {
                name: "library_path",
                value_type: "path",
                required: true,
                default: None,
                options: None,
                example: Some("/srv/media/library/movies/fr"),
                hint: None,
            },
            Field {
                name: "link_strategy",
                value_type: "select",
                required: true,
                default: Some(json!("symlink")),
                options: Some(vec!["symlink", "hardlink"]),
                example: None,
                hint: Some("hardlink requires same filesystem; symlink works across filesystems/disks"),
            },
            Field {
                name: "propagate_delete",
                value_type: "bool",
                required: false,
                default: Some(json!(true)),
                options: None,
                example: None,
                hint: Some("When true, deletes fan out to other instances"),
            },
        ]
    }))
}

#[utoipa::path(
    get,
    path = "/api/v1/config/schema",
    tag = "schema",
    responses(
        (status = 200, description = "Config field definitions"),
    ),
)]
pub async fn config_schema() -> impl IntoResponse {
    Json(json!({
        "fields": [
            Field {
                name: "primary_language",
                value_type: "string",
                required: true,
                default: None,
                options: None,
                example: Some("fr"),
                hint: Some("Language key from /api/v1/languages — the primary download language"),
            },
            Field {
                name: "queue_concurrency",
                value_type: "integer",
                required: false,
                default: Some(json!(2)),
                options: None,
                example: None,
                hint: Some("Number of concurrent webhook job workers"),
            },
        ]
    }))
}
