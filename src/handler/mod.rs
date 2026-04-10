//! Event handler orchestration — story 08a (import handlers + infrastructure).
//!
//! See `plan/active/multilinguarr-rust-rewrite/08a-event-handler-import.md`
//! and `08b-delete-cross-instance-sync.md` for the full scope split.

mod cross_instance;
mod delete;
mod error;
mod import;
mod registry;

#[cfg(test)]
mod tests;

pub use error::HandlerError;
pub use registry::HandlerRegistry;
