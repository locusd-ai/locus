//! Shared types and error definitions for BIEM.
//!
//! This crate defines the core types used across all BIEM modules:
//! IDs, enums, chunk model, and the trait definitions for Registry
//! and BitmapStore.

pub mod types;
pub mod parser;
pub mod registry;
pub mod bitmap;
pub mod query;
pub mod config;
pub mod enrich;
pub mod semantic;
pub mod graph;

pub use types::*;
pub use parser::*;
pub use registry::*;
pub use bitmap::*;
pub use query::*;
pub use enrich::*;
pub use semantic::*;
