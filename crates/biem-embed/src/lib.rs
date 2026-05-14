//! Embedding and vector search implementations for BIEM.
//!
//! Provides `VectorStore` implementations:
//! - `InMemoryVectorStore` — brute-force cosine similarity (for tests)
//! - (Future) `UsearchVectorStore` — persistent HNSW index
//!
//! Provides `Embedder` implementations:
//! - `FastEmbedEmbedder` — local ONNX inference (requires `fastembed-embedder` feature)

pub mod memory;

#[cfg(feature = "fastembed-embedder")]
pub mod fastembed;

pub use memory::InMemoryVectorStore;

#[cfg(feature = "fastembed-embedder")]
pub use fastembed::FastEmbedEmbedder;
