//! CodeParser — Tree-sitter based code parser implementing the Parser trait.

use std::collections::HashMap;
use std::path::Path;

use biem_core::parser::{ParseError, Parser};
use biem_core::types::ParseResult;

/// Tree-sitter based code parser.
///
/// Maintains a registry of file extensions → tree-sitter languages.
/// Currently supports Rust; TypeScript and Python planned.
pub struct CodeParser {
    /// Map of file extension (without dot) → tree-sitter language.
    languages: HashMap<String, tree_sitter::Language>,
}

impl CodeParser {
    /// Create a new CodeParser with default language support.
    pub fn new() -> Self {
        let mut languages = HashMap::new();

        // Rust
        languages.insert("rs".into(), tree_sitter_rust::LANGUAGE.into());

        Self { languages }
    }

    /// Register an additional language for a file extension.
    pub fn register_language(&mut self, extension: &str, language: tree_sitter::Language) {
        self.languages.insert(extension.to_string(), language);
    }

    /// Get the tree-sitter language for a file extension.
    fn language_for(&self, path: &Path) -> Option<&tree_sitter::Language> {
        path.extension()
            .and_then(|e| e.to_str())
            .and_then(|ext| self.languages.get(ext))
    }
}

impl Default for CodeParser {
    fn default() -> Self {
        Self::new()
    }
}

impl Parser for CodeParser {
    fn can_parse(&self, path: &Path) -> bool {
        self.language_for(path).is_some()
    }

    fn parse(&self, path: &Path, content: &[u8]) -> Result<ParseResult, ParseError> {
        let _lang = self.language_for(path).ok_or_else(|| {
            ParseError::Unsupported(format!(
                "no grammar for: {}",
                path.display()
            ))
        })?;

        // TODO: Step 2 — full AST walking and chunk extraction
        // For now, return an empty parse result as a skeleton.
        Ok(ParseResult {
            chunks: vec![],
            tags: vec![],
            links: vec![],
            auto_type: None,
            frontmatter: HashMap::new(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_can_parse_rust_files() {
        let parser = CodeParser::new();
        assert!(parser.can_parse(Path::new("src/main.rs")));
        assert!(parser.can_parse(Path::new("/foo/bar/lib.rs")));
    }

    #[test]
    fn test_cannot_parse_markdown() {
        let parser = CodeParser::new();
        assert!(!parser.can_parse(Path::new("README.md")));
        assert!(!parser.can_parse(Path::new("notes/todo.md")));
    }

    #[test]
    fn test_cannot_parse_unknown_extension() {
        let parser = CodeParser::new();
        assert!(!parser.can_parse(Path::new("data.xyz")));
        assert!(!parser.can_parse(Path::new("Makefile"))); // no extension
    }

    #[test]
    fn test_parse_returns_result() {
        let parser = CodeParser::new();
        let content = b"fn main() { println!(\"hello\"); }";
        let result = parser.parse(Path::new("main.rs"), content).unwrap();
        // Skeleton returns empty for now
        assert!(result.chunks.is_empty());
    }
}
