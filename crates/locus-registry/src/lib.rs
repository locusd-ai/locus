//! Registry trait re-export and DuckDB implementation.

pub use locus_core::{Registry, RegistryError};

pub mod memory;
pub mod duckdb;
pub mod graph;
