//! Query engine — bitmap intersection, filter resolution, metadata assembly.

mod engine;

pub use engine::BitmapQueryEngine;
pub use locus_core::{QueryEngine, QueryError, QueryRequest, QueryResult, Filter, MatchPointer, ChunkPointer};
pub use locus_core::semantic::{SemanticQueryRequest, SemanticQueryResult, ScoredPointer};
