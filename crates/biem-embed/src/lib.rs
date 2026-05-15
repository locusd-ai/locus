//! Embedding and vector search implementations for BIEM.
//!
//! Provides `VectorStore` implementations:
//! - `InMemoryVectorStore` — brute-force cosine similarity (for tests)
//! - `UsearchVectorStore` — persistent HNSW index (requires `usearch-store` feature)
//!
//! Provides `Embedder` implementations:
//! - `FastEmbedEmbedder` — local ONNX inference (requires `fastembed-embedder` feature)

pub mod memory;

#[cfg(feature = "fastembed-embedder")]
pub mod fastembed;

#[cfg(feature = "usearch-store")]
pub mod usearch;

pub use memory::InMemoryVectorStore;

#[cfg(feature = "fastembed-embedder")]
pub use fastembed::FastEmbedEmbedder;

#[cfg(feature = "fastembed-embedder")]
pub use fastembed::FastEmbedReranker;

#[cfg(feature = "usearch-store")]
pub use self::usearch::UsearchVectorStore;
