//! multilinguarr — multi-language media enforcement for the *arr stack.
//!
//! Modules correspond to the domains laid out in the Rust rewrite plan.
//! Implementations land story-by-story.

pub mod api;
pub mod app;
pub mod client;
pub mod config;
pub mod db;
pub mod detection;
pub mod handler;
pub mod jellyfin;
pub mod link;
pub mod observability;
pub mod queue;
pub mod reconcile;
pub mod webhook;
