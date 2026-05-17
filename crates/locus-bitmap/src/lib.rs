//! BitmapStore trait re-export and LMDB implementation.

pub use locus_core::{BitmapStore, BitmapError};

pub mod memory;
pub mod lmdb;
