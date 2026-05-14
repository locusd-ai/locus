//! CodeParser — Tree-sitter based code parser implementing the Parser trait.

use std::collections::HashMap;
use std::path::Path;

use biem_core::parser::{ParseError, Parser};
use biem_core::types::{
    Chunk, ChunkKind, ChunkMetadata, DocType, LinkRef, ParseResult, Visibility,
};

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

    /// Detect the language string from file extension.
    fn language_name(&self, path: &Path) -> &'static str {
        match path.extension().and_then(|e| e.to_str()) {
            Some("rs") => "rust",
            Some("ts" | "tsx") => "typescript",
            Some("py") => "python",
            Some("js" | "jsx") => "javascript",
            Some("go") => "go",
            Some("java") => "java",
            _ => "unknown",
        }
    }

    /// Detect auto doc type from path conventions.
    fn detect_doc_type(path: &Path) -> Option<DocType> {
        let name = path.file_stem()?.to_str()?;
        let path_str = path.to_str().unwrap_or("");

        // Test files
        if name.ends_with("_test")
            || name.ends_with("_tests")
            || name.starts_with("test_")
            || name == "tests"
            || path_str.contains("/tests/")
            || path_str.contains("/test/")
            || name.ends_with("_spec")
            || name.ends_with(".test")
            || name.ends_with(".spec")
        {
            return Some(DocType::TestFile);
        }

        // Config files
        let ext = path.extension()?.to_str()?;
        if matches!(ext, "toml" | "yaml" | "yml" | "json" | "ini" | "cfg")
            || name == "Cargo"
            || name == "Makefile"
            || name == "Dockerfile"
            || name.starts_with(".")
        {
            return Some(DocType::ConfigFile);
        }

        Some(DocType::SourceFile)
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
        let lang = self.language_for(path).ok_or_else(|| {
            ParseError::Unsupported(format!("no grammar for: {}", path.display()))
        })?;

        let lang_name = self.language_name(path);

        let mut ts_parser = tree_sitter::Parser::new();
        ts_parser
            .set_language(lang)
            .map_err(|e| ParseError::Internal(format!("tree-sitter language error: {e}")))?;

        let tree = ts_parser
            .parse(content, None)
            .ok_or_else(|| ParseError::Internal("tree-sitter parse failed".into()))?;

        let source = std::str::from_utf8(content).map_err(|_| ParseError::InvalidUtf8)?;

        let mut chunks = Vec::new();
        let mut tags = Vec::new();
        let mut imports = Vec::new();

        // Add language tag
        tags.push(format!("lang:{lang_name}"));

        // Walk the AST root children
        let root = tree.root_node();
        walk_rust_nodes(root, source, lang_name, 0, &mut chunks, &mut tags, &mut imports);

        let auto_type = Self::detect_doc_type(path);

        Ok(ParseResult {
            chunks,
            tags,
            links: imports,
            auto_type,
            frontmatter: HashMap::new(),
        })
    }
}

/// Walk Rust AST nodes and extract chunks, tags, and imports.
fn walk_rust_nodes(
    node: tree_sitter::Node,
    source: &str,
    lang: &str,
    depth: u8,
    chunks: &mut Vec<Chunk>,
    tags: &mut Vec<String>,
    imports: &mut Vec<LinkRef>,
) {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        match child.kind() {
            "function_item" => {
                if let Some(chunk) = extract_function(&child, source, lang, depth) {
                    // Add kind tag
                    if !tags.contains(&"kind:function".to_string()) {
                        tags.push("kind:function".to_string());
                    }
                    // Check for async
                    if is_async_function(&child) && !tags.contains(&"async:true".to_string()) {
                        tags.push("async:true".to_string());
                    }
                    chunks.push(chunk);
                }
            }
            "struct_item" => {
                if let Some(chunk) = extract_named_item(&child, source, lang, ChunkKind::Class, depth) {
                    if !tags.contains(&"kind:struct".to_string()) {
                        tags.push("kind:struct".to_string());
                    }
                    chunks.push(chunk);
                }
            }
            "enum_item" => {
                if let Some(chunk) = extract_named_item(&child, source, lang, ChunkKind::Class, depth) {
                    if !tags.contains(&"kind:enum".to_string()) {
                        tags.push("kind:enum".to_string());
                    }
                    chunks.push(chunk);
                }
            }
            "trait_item" => {
                if let Some(chunk) = extract_named_item(&child, source, lang, ChunkKind::Class, depth) {
                    if !tags.contains(&"kind:trait".to_string()) {
                        tags.push("kind:trait".to_string());
                    }
                    chunks.push(chunk);
                }
            }
            "impl_item" => {
                extract_impl_block(&child, source, lang, depth, chunks, tags);
            }
            "mod_item" => {
                if let Some(chunk) = extract_named_item(&child, source, lang, ChunkKind::Module, depth) {
                    chunks.push(chunk);
                    // Recurse into module body
                    if let Some(body) = child.child_by_field_name("body") {
                        walk_rust_nodes(body, source, lang, depth + 1, chunks, tags, imports);
                    }
                }
            }
            "use_declaration" => {
                extract_use_import(&child, source, imports);
            }
            "const_item" | "static_item" => {
                if let Some(chunk) = extract_named_item(&child, source, lang, ChunkKind::Constant, depth) {
                    chunks.push(chunk);
                }
            }
            _ => {}
        }
    }
}

/// Extract a function chunk with signature and visibility.
fn extract_function(
    node: &tree_sitter::Node,
    source: &str,
    lang: &str,
    depth: u8,
) -> Option<Chunk> {
    let name = node.child_by_field_name("name")?;
    let label = name.utf8_text(source.as_bytes()).ok()?.to_string();
    let vis = extract_visibility(node, source.as_bytes());
    let sig = extract_function_signature(node, source);

    Some(Chunk {
        byte_range: node.byte_range(),
        kind: ChunkKind::Function,
        label: Some(label),
        depth,
        metadata: ChunkMetadata {
            signature: Some(sig),
            language: Some(lang.to_string()),
            visibility: Some(vis),
        },
    })
}

/// Extract a named item (struct, enum, trait, const, module).
fn extract_named_item(
    node: &tree_sitter::Node,
    source: &str,
    lang: &str,
    kind: ChunkKind,
    depth: u8,
) -> Option<Chunk> {
    let name = node.child_by_field_name("name")?;
    let label = name.utf8_text(source.as_bytes()).ok()?.to_string();
    let vis = extract_visibility(node, source.as_bytes());

    Some(Chunk {
        byte_range: node.byte_range(),
        kind,
        label: Some(label),
        depth,
        metadata: ChunkMetadata {
            signature: None,
            language: Some(lang.to_string()),
            visibility: Some(vis),
        },
    })
}

/// Extract methods from an impl block.
fn extract_impl_block(
    node: &tree_sitter::Node,
    source: &str,
    lang: &str,
    depth: u8,
    chunks: &mut Vec<Chunk>,
    tags: &mut Vec<String>,
) {
    // Get the type being impl'd
    let impl_type = node
        .child_by_field_name("type")
        .and_then(|n| n.utf8_text(source.as_bytes()).ok())
        .map(|s| s.to_string());

    // Check if this is a trait implementation
    let trait_name = node
        .child_by_field_name("trait")
        .and_then(|n| n.utf8_text(source.as_bytes()).ok())
        .map(|s| s.to_string());

    // Walk the impl body for methods
    if let Some(body) = node.child_by_field_name("body") {
        let mut cursor = body.walk();
        for child in body.children(&mut cursor) {
            if child.kind() == "function_item" {
                if let Some(fn_name) = child.child_by_field_name("name") {
                    let label_text = fn_name.utf8_text(source.as_bytes()).unwrap_or("");
                    let qualified = match (&impl_type, &trait_name) {
                        (Some(ty), Some(tr)) => format!("<{ty} as {tr}>::{label_text}"),
                        (Some(ty), None) => format!("{ty}::{label_text}"),
                        _ => label_text.to_string(),
                    };
                    let vis = extract_visibility(&child, source.as_bytes());
                    let sig = extract_function_signature(&child, source);

                    chunks.push(Chunk {
                        byte_range: child.byte_range(),
                        kind: ChunkKind::Method,
                        label: Some(qualified),
                        depth: depth + 1,
                        metadata: ChunkMetadata {
                            signature: Some(sig),
                            language: Some(lang.to_string()),
                            visibility: Some(vis),
                        },
                    });

                    if !tags.contains(&"kind:method".to_string()) {
                        tags.push("kind:method".to_string());
                    }
                }
            }
        }
    }
}

/// Extract `use` statements as imports (stored as LinkRef for consistency).
fn extract_use_import(node: &tree_sitter::Node, source: &str, imports: &mut Vec<LinkRef>) {
    // Get the full use path text
    let text = node.utf8_text(source.as_bytes()).unwrap_or("");

    // Extract the crate name (first path segment after `use`)
    // e.g., `use std::collections::HashMap;` → target = "std"
    // e.g., `use serde::{Serialize, Deserialize};` → target = "serde"
    if let Some(arg) = node.child_by_field_name("argument") {
        let path_text = arg.utf8_text(source.as_bytes()).unwrap_or("");
        let crate_name = path_text.split("::").next().unwrap_or(path_text).trim();

        // Skip self/crate/super references
        if !matches!(crate_name, "self" | "crate" | "super" | "") {
            imports.push(LinkRef {
                target: crate_name.to_string(),
                display: Some(text.trim().to_string()),
                byte_offset: node.start_byte(),
            });
        }
    }
}

/// Determine visibility of a node by checking for visibility_modifier child.
fn extract_visibility(node: &tree_sitter::Node, source: &[u8]) -> Visibility {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.kind() == "visibility_modifier" {
            let text = child.utf8_text(source).unwrap_or("pub");
            return if text.contains("crate") {
                Visibility::Internal
            } else {
                Visibility::Public
            };
        }
    }
    Visibility::Private
}

/// Check if a function is async.
fn is_async_function(node: &tree_sitter::Node) -> bool {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.kind() == "async" {
            return true;
        }
    }
    false
}

/// Extract function signature (everything before the body block).
fn extract_function_signature(node: &tree_sitter::Node, source: &str) -> String {
    // Find the body (block) node and take everything before it
    if let Some(body) = node.child_by_field_name("body") {
        let sig_end = body.start_byte();
        let sig_start = node.start_byte();
        if sig_end > sig_start {
            let sig = &source[sig_start..sig_end];
            return sig.trim().to_string();
        }
    }
    // Fallback: first line
    let text = &source[node.start_byte()..node.end_byte()];
    text.lines().next().unwrap_or("").to_string()
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
    }

    #[test]
    fn test_cannot_parse_unknown_extension() {
        let parser = CodeParser::new();
        assert!(!parser.can_parse(Path::new("data.xyz")));
    }

    #[test]
    fn test_parse_simple_function() {
        let parser = CodeParser::new();
        let content = b"pub fn greet(name: &str) -> String {
    format!(\"Hello, {}!\", name)
}
";
        let result = parser.parse(Path::new("lib.rs"), content).unwrap();

        assert_eq!(result.chunks.len(), 1);
        let chunk = &result.chunks[0];
        assert_eq!(chunk.kind, ChunkKind::Function);
        assert_eq!(chunk.label.as_deref(), Some("greet"));
        assert_eq!(chunk.depth, 0);
        assert_eq!(chunk.metadata.language.as_deref(), Some("rust"));
        assert_eq!(chunk.metadata.visibility, Some(Visibility::Public));
        assert!(chunk.metadata.signature.as_ref().unwrap().contains("greet"));
        assert!(chunk.metadata.signature.as_ref().unwrap().contains("-> String"));

        assert!(result.tags.contains(&"lang:rust".to_string()));
        assert!(result.tags.contains(&"kind:function".to_string()));
    }

    #[test]
    fn test_parse_struct_and_impl() {
        let parser = CodeParser::new();
        let content = b"pub struct Counter {
    value: u32,
}

impl Counter {
    pub fn new() -> Self {
        Self { value: 0 }
    }

    fn increment(&mut self) {
        self.value += 1;
    }
}
";
        let result = parser.parse(Path::new("counter.rs"), content).unwrap();

        // Should have: struct Counter, Counter::new, Counter::increment
        assert_eq!(result.chunks.len(), 3);

        let struct_chunk = result.chunks.iter().find(|c| c.kind == ChunkKind::Class).unwrap();
        assert_eq!(struct_chunk.label.as_deref(), Some("Counter"));
        assert_eq!(struct_chunk.metadata.visibility, Some(Visibility::Public));

        let methods: Vec<_> = result.chunks.iter().filter(|c| c.kind == ChunkKind::Method).collect();
        assert_eq!(methods.len(), 2);

        let new_method = methods.iter().find(|c| c.label.as_ref().unwrap().contains("new")).unwrap();
        assert_eq!(new_method.label.as_deref(), Some("Counter::new"));
        assert_eq!(new_method.depth, 1);
        assert_eq!(new_method.metadata.visibility, Some(Visibility::Public));

        let inc_method = methods.iter().find(|c| c.label.as_ref().unwrap().contains("increment")).unwrap();
        assert_eq!(inc_method.label.as_deref(), Some("Counter::increment"));
        assert_eq!(inc_method.metadata.visibility, Some(Visibility::Private));
    }

    #[test]
    fn test_parse_use_imports() {
        let parser = CodeParser::new();
        let content = b"use std::collections::HashMap;
use serde::{Serialize, Deserialize};
use crate::types::MyType;

fn main() {}
";
        let result = parser.parse(Path::new("main.rs"), content).unwrap();

        // Should have imports for std, serde (not crate)
        let import_targets: Vec<_> = result.links.iter().map(|l| l.target.as_str()).collect();
        assert!(import_targets.contains(&"std"));
        assert!(import_targets.contains(&"serde"));
        assert!(!import_targets.contains(&"crate"));
    }

    #[test]
    fn test_parse_enum_and_trait() {
        let parser = CodeParser::new();
        let content = b"pub enum Color {
    Red,
    Green,
    Blue,
}

pub trait Paintable {
    fn paint(&self, color: Color);
}
";
        let result = parser.parse(Path::new("types.rs"), content).unwrap();

        assert_eq!(result.chunks.len(), 2);
        assert!(result.tags.contains(&"kind:enum".to_string()));
        assert!(result.tags.contains(&"kind:trait".to_string()));

        let enum_chunk = result.chunks.iter().find(|c| c.label.as_deref() == Some("Color")).unwrap();
        assert_eq!(enum_chunk.kind, ChunkKind::Class);

        let trait_chunk = result.chunks.iter().find(|c| c.label.as_deref() == Some("Paintable")).unwrap();
        assert_eq!(trait_chunk.kind, ChunkKind::Class);
    }

    #[test]
    fn test_parse_constants() {
        let parser = CodeParser::new();
        let content = b"pub const MAX_SIZE: usize = 1024;
static COUNTER: u32 = 0;
";
        let result = parser.parse(Path::new("constants.rs"), content).unwrap();

        assert_eq!(result.chunks.len(), 2);
        assert!(result.chunks.iter().all(|c| c.kind == ChunkKind::Constant));
    }

    #[test]
    fn test_detect_test_file() {
        let parser = CodeParser::new();
        let content = b"fn main() {}";
        let result = parser.parse(Path::new("src/parser_test.rs"), content).unwrap();
        assert_eq!(result.auto_type, Some(DocType::TestFile));
    }

    #[test]
    fn test_detect_source_file() {
        let parser = CodeParser::new();
        let content = b"fn main() {}";
        let result = parser.parse(Path::new("src/parser.rs"), content).unwrap();
        assert_eq!(result.auto_type, Some(DocType::SourceFile));
    }

    #[test]
    fn test_nested_depth() {
        let parser = CodeParser::new();
        let content = b"mod outer {
    pub fn inner_fn() {}
}
";
        let result = parser.parse(Path::new("nested.rs"), content).unwrap();

        let module = result.chunks.iter().find(|c| c.kind == ChunkKind::Module).unwrap();
        assert_eq!(module.depth, 0);

        let function = result.chunks.iter().find(|c| c.kind == ChunkKind::Function).unwrap();
        assert_eq!(function.depth, 1);
    }
}
