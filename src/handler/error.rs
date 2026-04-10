//! Handler-layer errors.
//!
//! Wraps the lower-layer errors (`ArrError`, `LinkError`) plus a few
//! handler-specific failure modes. The `is_transient` predicate
//! collapses everything down to "should the worker pool retry this?".

use std::path::PathBuf;

use thiserror::Error;

use crate::client::ArrError;
use crate::detection::DetectionError;
use crate::link::LinkError;
use crate::queue::QueueError;

#[derive(Debug, Error)]
pub enum HandlerError {
    #[error("arr api error: {0}")]
    Arr(#[from] ArrError),

    #[error("link manager error: {0}")]
    Link(#[from] LinkError),

    #[error("language detection failed: {0}")]
    Detection(#[from] DetectionError),

    #[error("instance `{0}` is not registered with the handler")]
    UnknownInstance(String),

    #[error("payload missing required field: {0}")]
    MissingField(&'static str),

    #[error("malformed path: {0:?}")]
    MalformedPath(PathBuf),

    #[error("failed to decode job payload: {0}")]
    Decode(#[from] serde_json::Error),

    #[error("queue error: {0}")]
    Queue(#[from] QueueError),
}

impl HandlerError {
    /// `true` when retrying the job has any chance of succeeding.
    ///
    /// - `Arr` errors defer to `ArrError::is_transient` (5xx, timeout,
    ///   connect failures).
    /// - `Link::Io` is treated as transient because filesystem
    ///   contention (EBUSY, ENOSPC after cleanup, etc.) can clear up.
    /// - Everything else is permanent — retrying a missing field or a
    ///   bad path will give the same answer.
    #[must_use]
    pub fn is_transient(&self) -> bool {
        match self {
            Self::Arr(e) => e.is_transient(),
            Self::Link(LinkError::Io { .. }) => true,
            Self::Link(_)
            | Self::Detection(_)
            | Self::UnknownInstance(_)
            | Self::MissingField(_)
            | Self::MalformedPath(_)
            | Self::Decode(_)
            | Self::Queue(_) => false,
        }
    }
}
