//! Enrichment pipeline for BIEM.
//!
//! Provides the `TagPipeline` which orchestrates pluggable taggers
//! to produce inferred bitmap keys from parse results.

mod pipeline;
mod cache;
mod fs_cache;

pub use pipeline::TagPipeline;
pub use cache::InMemoryTaggerCache;
pub use fs_cache::FsTaggerCache;
