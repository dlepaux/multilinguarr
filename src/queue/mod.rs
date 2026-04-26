//! SQLite-backed job queue with visibility-timeout lease semantics.
//!
//! Producers (webhook handlers, admin endpoints) call
//! [`JobStore::enqueue`]. A [`spawn_worker_pool`] dispatcher claims
//! work atomically via `UPDATE ... RETURNING`, hands it to a
//! [`JobProcessor`], and acks the result. Crashed workers are recovered
//! by either [`spawn_sweeper`] or [`JobStore::reset_orphaned_claims`]
//! at startup.

mod error;
mod job;
mod store;
mod worker;

#[cfg(test)]
mod tests;

pub use error::QueueError;
pub use job::{Job, JobId, JobPayload, JobStatus, QueueStats, RetryPolicy};
pub use store::{FailureDisposition, JobStore};
pub use worker::{
    spawn_sweeper, spawn_worker_pool, JobProcessor, ProcessOutcome, WorkerPoolConfig,
};
