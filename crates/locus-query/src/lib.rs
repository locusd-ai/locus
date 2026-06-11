//! Query engine — bitmap intersection, filter resolution, metadata assembly.

mod engine;
pub mod graph_engine;
pub mod filter_parser;

pub use engine::BitmapQueryEngine;
pub use graph_engine::PetgraphQueryEngine;
pub use filter_parser::{parse_filter, match_all_filter, FilterParseError};
pub use locus_core::{nest_chunks, QueryEngine, QueryError, QueryRequest, QueryResult, Filter, MatchPointer, ChunkPointer};
pub use locus_core::semantic::{SemanticQueryRequest, SemanticQueryResult, ScoredPointer};
