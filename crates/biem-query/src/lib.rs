//! Query engine — bitmap intersection, filter resolution, metadata assembly.

mod engine;

pub use engine::BitmapQueryEngine;
pub use biem_core::{QueryEngine, QueryError, QueryRequest, QueryResult, Filter, MatchPointer, ChunkPointer};
