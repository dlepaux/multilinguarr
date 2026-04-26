//! `HandlerRegistry` — central state + `JobProcessor` dispatch.
//!
//! Holds the per-instance `ArrClient` and `LinkManager` maps so each
//! handler call is O(1) instead of iterating `Config.instances`. Also
//! holds the shared `LanguageDetector` and the Jellyfin refresh trigger
//! (no-op until story 09).

use std::collections::HashMap;
use std::sync::Arc;

use tracing::{info_span, Instrument};

use super::delete::{
    handle_radarr_movie_delete, handle_radarr_movie_file_delete, handle_sonarr_episode_file_delete,
    handle_sonarr_series_delete,
};
use super::error::HandlerError;
use super::import::{handle_radarr_download, handle_sonarr_download};
use crate::client::ArrClient;
use crate::config::{Config, InstanceConfig, InstanceKind};
use crate::detection::{FfprobeProber, LanguageDetector};
use crate::jellyfin::MediaServer;
use crate::link::LinkManager;
use crate::queue::{Job, JobPayload, JobProcessor, ProcessOutcome};
use crate::webhook::{RadarrEvent, RadarrWebhookJob, SonarrEvent, SonarrWebhookJob};

/// All the moving parts a handler needs in one place. Cheap to clone
/// — every field is either `Arc` or already implements cheap `Clone`.
#[derive(Debug, Clone)]
pub struct HandlerRegistry<P: FfprobeProber = crate::detection::SystemFfprobe> {
    pub(super) config: Arc<Config>,
    pub(super) instances: Arc<HashMap<String, InstanceConfig>>,
    pub(super) clients: Arc<HashMap<String, ArrClient>>,
    pub(super) link_managers: Arc<HashMap<String, LinkManager>>,
    pub(super) detector: Arc<LanguageDetector<P>>,
    pub(super) jellyfin: Arc<dyn MediaServer>,
}

impl<P: FfprobeProber> HandlerRegistry<P> {
    /// Build a registry from validated config + a language detector +
    /// a Jellyfin trigger. The per-instance maps are constructed once,
    /// at startup, by walking `config.instances`.
    ///
    /// # Errors
    ///
    /// Returns [`HandlerError::Arr`] if an `ArrClient` cannot be constructed from an instance config.
    pub fn build(
        config: Arc<Config>,
        detector: LanguageDetector<P>,
        jellyfin: Arc<dyn MediaServer>,
    ) -> Result<Self, HandlerError> {
        let mut instances = HashMap::new();
        let mut clients = HashMap::new();
        let mut link_managers = HashMap::new();

        for instance in &config.instances {
            let client = ArrClient::from_instance(instance)?;
            let link_mgr = LinkManager::from_instance(instance);
            instances.insert(instance.name.clone(), instance.clone());
            clients.insert(instance.name.clone(), client);
            link_managers.insert(instance.name.clone(), link_mgr);
        }

        Ok(Self {
            config,
            instances: Arc::new(instances),
            clients: Arc::new(clients),
            link_managers: Arc::new(link_managers),
            detector: Arc::new(detector),
            jellyfin,
        })
    }

    /// Look up an instance by name. Returns `UnknownInstance` if the
    /// queue contains a job for an instance no longer in config — that
    /// is a permanent failure (won't fix on retry).
    pub(super) fn instance(&self, name: &str) -> Result<&InstanceConfig, HandlerError> {
        self.instances
            .get(name)
            .ok_or_else(|| HandlerError::UnknownInstance(name.to_owned()))
    }

    pub(super) fn client(&self, name: &str) -> Result<&ArrClient, HandlerError> {
        self.clients
            .get(name)
            .ok_or_else(|| HandlerError::UnknownInstance(name.to_owned()))
    }

    pub(super) fn link_manager(&self, name: &str) -> Result<&LinkManager, HandlerError> {
        self.link_managers
            .get(name)
            .ok_or_else(|| HandlerError::UnknownInstance(name.to_owned()))
    }

    /// All configured instances in declaration order. Used by
    /// cross-instance helpers to fan out to siblings.
    pub(super) fn config_instances(&self) -> &[InstanceConfig] {
        &self.config.instances
    }

    /// Iterate every `LinkManager` paired with its `InstanceConfig`.
    /// Used by primary delete handlers to scan every library for
    /// links pointing into a given storage tree.
    pub(super) fn instances_with_link_managers(
        &self,
    ) -> impl Iterator<Item = (&InstanceConfig, &LinkManager)> {
        self.config
            .instances
            .iter()
            .filter_map(move |i| self.link_managers.get(&i.name).map(|m| (i, m)))
    }

    /// `true` when this instance's configured language matches the
    /// global primary language.
    pub(super) fn is_primary(&self, instance: &InstanceConfig) -> bool {
        instance.language == self.config.languages.primary
    }

    /// Find every instance whose configured language is in `languages`
    /// AND whose engine matches `kind`. Used by the multi-audio import
    /// path to decide which libraries get cross-instance links.
    pub(super) fn instances_for_languages(
        &self,
        kind: InstanceKind,
        languages: &std::collections::HashSet<String>,
    ) -> Vec<&InstanceConfig> {
        self.config
            .instances
            .iter()
            .filter(|i| i.kind == kind && languages.contains(&i.language))
            .collect()
    }

    pub(super) async fn process_radarr(&self, job: Job) -> Result<(), HandlerError> {
        let payload: RadarrWebhookJob = job.decode_payload()?;
        match payload.event {
            RadarrEvent::Download(event) => {
                let instance = self.instance(&payload.instance)?.clone();
                handle_radarr_download(&instance, &event, self).await
            }
            // Defensive arm: the HTTP layer filters these before enqueue,
            // so they should never reach the worker. Listed exhaustively
            // to keep the match total.
            RadarrEvent::Test(_)
            | RadarrEvent::Unknown
            | RadarrEvent::Grab
            | RadarrEvent::Rename
            | RadarrEvent::MovieAdded
            | RadarrEvent::MovieFileRenamed
            | RadarrEvent::Health
            | RadarrEvent::HealthRestored
            | RadarrEvent::ApplicationUpdate
            | RadarrEvent::ManualInteractionRequired => {
                tracing::debug!(
                    instance = %payload.instance,
                    "ignoring radarr event in worker (handler-side)"
                );
                Ok(())
            }
            RadarrEvent::MovieDelete(event) => {
                let instance = self.instance(&payload.instance)?.clone();
                handle_radarr_movie_delete(&instance, &event, self).await
            }
            RadarrEvent::MovieFileDelete(event) => {
                let instance = self.instance(&payload.instance)?.clone();
                handle_radarr_movie_file_delete(&instance, &event, self).await
            }
        }
    }

    pub(super) async fn process_sonarr(&self, job: Job) -> Result<(), HandlerError> {
        let payload: SonarrWebhookJob = job.decode_payload()?;
        match payload.event {
            SonarrEvent::Download(event) => {
                let instance = self.instance(&payload.instance)?.clone();
                handle_sonarr_download(&instance, &event, self).await
            }
            SonarrEvent::Test(_)
            | SonarrEvent::Unknown
            | SonarrEvent::Grab
            | SonarrEvent::Rename
            | SonarrEvent::SeriesAdd
            | SonarrEvent::Health
            | SonarrEvent::HealthRestored
            | SonarrEvent::ApplicationUpdate
            | SonarrEvent::ManualInteractionRequired => {
                tracing::debug!(
                    instance = %payload.instance,
                    "ignoring sonarr event in worker (handler-side)"
                );
                Ok(())
            }
            SonarrEvent::SeriesDelete(event) => {
                let instance = self.instance(&payload.instance)?.clone();
                handle_sonarr_series_delete(&instance, &event, self).await
            }
            SonarrEvent::EpisodeFileDelete(event) => {
                let instance = self.instance(&payload.instance)?.clone();
                handle_sonarr_episode_file_delete(&instance, &event, self).await
            }
        }
    }
}

impl<P: FfprobeProber> JobProcessor for HandlerRegistry<P> {
    async fn process(&self, job: Job) -> ProcessOutcome {
        let job_id = job.id;
        let kind = job.kind.clone();
        let span = info_span!("handle_job", job_id, kind = %kind);

        let result = async {
            match kind.as_str() {
                k if k == RadarrWebhookJob::KIND => self.process_radarr(job).await,
                k if k == SonarrWebhookJob::KIND => self.process_sonarr(job).await,
                other => Err(HandlerError::Decode(serde_json::Error::io(
                    std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        format!("unknown job kind: {other}"),
                    ),
                ))),
            }
        }
        .instrument(span)
        .await;

        let outcome = match result {
            Ok(()) => ProcessOutcome::Success,
            Err(err) => {
                let msg = err.to_string();
                if err.is_transient() {
                    tracing::warn!(error = %msg, "handler returned transient error — will retry");
                    ProcessOutcome::Transient(msg)
                } else {
                    tracing::error!(error = %msg, "handler returned permanent error — will not retry");
                    ProcessOutcome::Permanent(msg)
                }
            }
        };

        let status_label = match &outcome {
            ProcessOutcome::Success => "success",
            ProcessOutcome::Transient(_) => "transient",
            ProcessOutcome::Permanent(_) => "permanent",
        };
        metrics::counter!(crate::observability::names::JOBS_PROCESSED,
            "kind" => kind,
            "status" => status_label,
        )
        .increment(1);

        outcome
    }
}
