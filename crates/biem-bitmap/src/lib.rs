//! BitmapStore trait re-export and LMDB implementation.

pub use biem_core::{BitmapStore, BitmapError};

pub mod memory;
// TODO: pub mod lmdb;
