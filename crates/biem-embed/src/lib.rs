//! Embedding and vector search implementations for BIEM.
//!
//! Provides `VectorStore` implementations:
//! - `InMemoryVectorStore` — brute-force cosine similarity (for tests)
//! - (Future) `UsearchVectorStore` — persistent HNSW index

pub mod memory;

pub use memory::InMemoryVectorStore;
