use std::path::Path;

use crate::{ParseResult};

/// Trait that all parsers implement.
///
/// Parsers are pure functions — no I/O, no state.
/// Content is provided as bytes; the parser returns structured metadata.
pub trait Parser: Send + Sync {
    /// Returns true if this parser can handle the given file path.
    /// Typically checks file extension.
    fn can_parse(&self, path: &Path) -> bool;

    /// Parse the file content and return structured metadata.
    /// Must not perform I/O — content is provided as bytes.
    fn parse(&self, path: &Path, content: &[u8]) -> Result<ParseResult, ParseError>;
}

/// Errors that can occur during parsing.
#[derive(Debug, thiserror::Error)]
pub enum ParseError {
    #[error("invalid UTF-8 in file")]
    InvalidUtf8,
    #[error("malformed frontmatter: {0}")]
    BadFrontmatter(String),
    #[error("parser internal error: {0}")]
    Internal(String),
    #[error("unsupported file type: {0}")]
    Unsupported(String),
}
