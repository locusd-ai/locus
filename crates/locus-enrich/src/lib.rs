//! Enrichment pipeline for Locus.
//!
//! Provides the `TagPipeline` which orchestrates pluggable taggers
//! to produce inferred bitmap keys from parse results.

mod pipeline;
mod cache;
mod fs_cache;
pub mod builtin;
mod yaml_tagger;

pub use pipeline::TagPipeline;
pub use cache::InMemoryTaggerCache;
pub use fs_cache::FsTaggerCache;
pub use yaml_tagger::{YamlTagger, load_yaml_taggers};
