//! Event handler orchestration — import + delete + cross-instance sync.

mod cross_instance;
mod delete;
mod error;
mod import;
mod registry;

#[cfg(test)]
mod tests;

pub use error::{register_failure_counters, HandlerError};
pub use registry::HandlerRegistry;
