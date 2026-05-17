//! Code parser — Tree-sitter AST chunking for code intelligence.
//!
//! Implements the `Parser` trait for code files, using Tree-sitter grammars
//! to produce AST-aware chunks with code-specific metadata.

mod parser;

pub use parser::CodeParser;
