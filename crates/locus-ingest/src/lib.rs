//! Ingestion pipeline — orchestrates parsers, registry, and bitmap store.

mod pipeline;

pub use pipeline::{BulkIndexResult, CompactResult, IngestAction, IngestError, IngestResult, IngestionPipeline};
