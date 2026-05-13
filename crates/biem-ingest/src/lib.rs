//! Ingestion pipeline — orchestrates parsers, registry, and bitmap store.

mod pipeline;
pub mod vault_gen;

pub use pipeline::{BulkIndexResult, CompactResult, IngestAction, IngestError, IngestResult, IngestionPipeline};
