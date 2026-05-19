//! Query engine — bitmap intersection, filter resolution, metadata assembly.

mod engine;
pub mod graph_engine;

pub use engine::BitmapQueryEngine;
pub use graph_engine::PetgraphQueryEngine;
pub use locus_core::{QueryEngine, QueryError, QueryRequest, QueryResult, Filter, MatchPointer, ChunkPointer};
pub use locus_core::semantic::{SemanticQueryRequest, SemanticQueryResult, ScoredPointer};
