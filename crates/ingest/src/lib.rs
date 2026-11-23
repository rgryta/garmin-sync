//! The Garmin ingester: poll and backfill, turning payloads (and FIT files) into archive rows.

pub mod config;
pub mod fit;
pub mod normalize;
pub mod runner;
pub mod source;
