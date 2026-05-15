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
        let ts_lang: tree_sitter::Language = tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into();
        let tsx_lang: tree_sitter::Language = tree_sitter_typescript::LANGUAGE_TSX.into();
        languages.insert("ts".into(), ts_lang.clone());
        languages.insert("tsx".into(), tsx_lang);
        // Also register .js/.jsx using the TSX grammar (superset)
        languages.insert("js".into(), ts_lang.clone());
        languages.insert("jsx".into(), ts_lang);
        languages.insert("py".into(), tree_sitter_python::LANGUAGE.into());
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

        // Walk the AST root children based on language
        let root = tree.root_node();
        match lang_name {
            "rust" => walk_rust_nodes(root, source, lang_name, 0, &mut chunks, &mut tags, &mut imports),
            "typescript" | "javascript" => walk_typescript_nodes(root, source, lang_name, 0, &mut chunks, &mut tags, &mut imports),
            "python" => walk_python_nodes(root, source, lang_name, 0, &mut chunks, &mut tags, &mut imports),
            _ => {}
        }

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
                    if is_async_function(&child, source) && !tags.contains(&"async:true".to_string()) {
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
fn is_async_function(node: &tree_sitter::Node, source: &str) -> bool {
    // Check for "async" keyword child
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.kind() == "async" {
            return true;
        }
    }
    // Fallback: check source text for async keyword
    let text = &source[node.start_byte()..node.end_byte().min(node.start_byte() + 50)];
    text.contains("async fn") || text.starts_with("async ")
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

// ── TypeScript / JavaScript AST walking ──────────────────────────

/// Walk TypeScript/JavaScript AST nodes and extract chunks, tags, and imports.
fn walk_typescript_nodes(
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
            // function declaration: `function foo() {}`
            "function_declaration" => {
                if let Some(chunk) = extract_ts_function(&child, source, lang, depth) {
                    add_tag_once(tags, "kind:function");
                    if ts_is_async(&child) {
                        add_tag_once(tags, "async:true");
                    }
                    chunks.push(chunk);
                }
            }
            // arrow function in variable declaration is handled via lexical_declaration / variable_declaration
            "lexical_declaration" | "variable_declaration" => {
                extract_ts_variable_declarations(&child, source, lang, depth, chunks, tags);
            }
            // class declaration
            "class_declaration" => {
                if let Some(chunk) = extract_ts_class(&child, source, lang, depth) {
                    add_tag_once(tags, "kind:class");
                    chunks.push(chunk);
                }
                // Extract methods inside the class body
                if let Some(body) = child.child_by_field_name("body") {
                    extract_ts_class_members(&body, source, lang, depth + 1, chunks, tags);
                }
            }
            // interface declaration
            "interface_declaration" => {
                if let Some(chunk) = extract_ts_named(&child, source, lang, ChunkKind::Class, depth) {
                    add_tag_once(tags, "kind:interface");
                    chunks.push(chunk);
                }
            }
            // type alias: `type Foo = ...`
            "type_alias_declaration" => {
                if let Some(chunk) = extract_ts_named(&child, source, lang, ChunkKind::Constant, depth) {
                    add_tag_once(tags, "kind:type");
                    chunks.push(chunk);
                }
            }
            // enum declaration (TS enums)
            "enum_declaration" => {
                if let Some(chunk) = extract_ts_named(&child, source, lang, ChunkKind::Class, depth) {
                    add_tag_once(tags, "kind:enum");
                    chunks.push(chunk);
                }
            }
            // import statement
            "import_statement" => {
                extract_ts_import(&child, source, imports);
            }
            // export statement — may wrap a declaration
            "export_statement" => {
                walk_ts_export(&child, source, lang, depth, chunks, tags, imports);
            }
            _ => {}
        }
    }
}

/// Extract a TS/JS function declaration.
fn extract_ts_function(
    node: &tree_sitter::Node,
    source: &str,
    lang: &str,
    depth: u8,
) -> Option<Chunk> {
    let name = node.child_by_field_name("name")?;
    let label = name.utf8_text(source.as_bytes()).ok()?.to_string();
    let vis = ts_node_visibility(node);
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

/// Extract a named TS/JS declaration (interface, type alias, enum).
fn extract_ts_named(
    node: &tree_sitter::Node,
    source: &str,
    lang: &str,
    kind: ChunkKind,
    depth: u8,
) -> Option<Chunk> {
    let name = node.child_by_field_name("name")?;
    let label = name.utf8_text(source.as_bytes()).ok()?.to_string();
    let vis = ts_node_visibility(node);

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

/// Extract a class declaration.
fn extract_ts_class(
    node: &tree_sitter::Node,
    source: &str,
    lang: &str,
    depth: u8,
) -> Option<Chunk> {
    let name = node.child_by_field_name("name")?;
    let label = name.utf8_text(source.as_bytes()).ok()?.to_string();
    let vis = ts_node_visibility(node);

    Some(Chunk {
        byte_range: node.byte_range(),
        kind: ChunkKind::Class,
        label: Some(label),
        depth,
        metadata: ChunkMetadata {
            signature: None,
            language: Some(lang.to_string()),
            visibility: Some(vis),
        },
    })
}

/// Extract class methods from a class body.
fn extract_ts_class_members(
    body: &tree_sitter::Node,
    source: &str,
    lang: &str,
    depth: u8,
    chunks: &mut Vec<Chunk>,
    tags: &mut Vec<String>,
) {
    let mut cursor = body.walk();
    for child in body.children(&mut cursor) {
        if child.kind() == "method_definition" || child.kind() == "public_field_definition" {
            if let Some(name) = child.child_by_field_name("name") {
                let label = name.utf8_text(source.as_bytes()).unwrap_or("").to_string();
                if !label.is_empty() && child.kind() == "method_definition" {
                    let sig = extract_function_signature(&child, source);
                    chunks.push(Chunk {
                        byte_range: child.byte_range(),
                        kind: ChunkKind::Method,
                        label: Some(label),
                        depth,
                        metadata: ChunkMetadata {
                            signature: Some(sig),
                            language: Some(lang.to_string()),
                            visibility: Some(Visibility::Public),
                        },
                    });
                    add_tag_once(tags, "kind:method");
                }
            }
        }
    }
}

/// Extract variable declarations — detects arrow functions and constants.
fn extract_ts_variable_declarations(
    node: &tree_sitter::Node,
    source: &str,
    lang: &str,
    depth: u8,
    chunks: &mut Vec<Chunk>,
    tags: &mut Vec<String>,
) {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.kind() == "variable_declarator" {
            let name = child.child_by_field_name("name");
            let value = child.child_by_field_name("value");

            if let (Some(name_node), Some(value_node)) = (name, value) {
                let label = name_node.utf8_text(source.as_bytes()).unwrap_or("").to_string();
                if label.is_empty() {
                    continue;
                }

                let is_arrow = value_node.kind() == "arrow_function";
                let is_fn_expr = value_node.kind() == "function" || value_node.kind() == "function_expression";

                if is_arrow || is_fn_expr {
                    let vis = ts_node_visibility(node);
                    let sig = extract_function_signature(node, source);
                    chunks.push(Chunk {
                        byte_range: node.byte_range(),
                        kind: ChunkKind::Function,
                        label: Some(label),
                        depth,
                        metadata: ChunkMetadata {
                            signature: Some(sig),
                            language: Some(lang.to_string()),
                            visibility: Some(vis),
                        },
                    });
                    add_tag_once(tags, "kind:function");
                    if is_arrow && ts_is_async(&value_node) {
                        add_tag_once(tags, "async:true");
                    }
                } else {
                    // Regular constant
                    let vis = ts_node_visibility(node);
                    chunks.push(Chunk {
                        byte_range: node.byte_range(),
                        kind: ChunkKind::Constant,
                        label: Some(label),
                        depth,
                        metadata: ChunkMetadata {
                            signature: None,
                            language: Some(lang.to_string()),
                            visibility: Some(vis),
                        },
                    });
                }
            }
        }
    }
}

/// Extract import source from an import statement.
/// `import { Foo } from 'bar'` → target = "bar"
fn extract_ts_import(node: &tree_sitter::Node, source: &str, imports: &mut Vec<LinkRef>) {
    let text = node.utf8_text(source.as_bytes()).unwrap_or("");

    // Find the source string node
    if let Some(src) = node.child_by_field_name("source") {
        let raw = src.utf8_text(source.as_bytes()).unwrap_or("");
        // Strip quotes
        let module = raw.trim_matches(|c| c == '\'' || c == '"');
        if !module.is_empty() {
            // For scoped packages like @scope/pkg, keep the full name
            // For relative imports, skip (start with . or ..)
            if !module.starts_with('.') {
                // Extract package name (first segment or @scope/pkg)
                let pkg = if module.starts_with('@') {
                    // @scope/pkg/sub → @scope/pkg
                    let parts: Vec<&str> = module.splitn(3, '/').collect();
                    if parts.len() >= 2 {
                        format!("{}/{}", parts[0], parts[1])
                    } else {
                        module.to_string()
                    }
                } else {
                    // pkg/sub → pkg
                    module.split('/').next().unwrap_or(module).to_string()
                };

                imports.push(LinkRef {
                    target: pkg,
                    display: Some(text.trim().to_string()),
                    byte_offset: node.start_byte(),
                });
            }
        }
    }
}

/// Handle export statements — they wrap declarations.
fn walk_ts_export(
    node: &tree_sitter::Node,
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
            "function_declaration" => {
                if let Some(mut chunk) = extract_ts_function(&child, source, lang, depth) {
                    chunk.metadata.visibility = Some(Visibility::Public);
                    add_tag_once(tags, "kind:function");
                    chunks.push(chunk);
                }
            }
            "class_declaration" => {
                if let Some(mut chunk) = extract_ts_class(&child, source, lang, depth) {
                    chunk.metadata.visibility = Some(Visibility::Public);
                    add_tag_once(tags, "kind:class");
                    chunks.push(chunk);
                }
                if let Some(body) = child.child_by_field_name("body") {
                    extract_ts_class_members(&body, source, lang, depth + 1, chunks, tags);
                }
            }
            "interface_declaration" => {
                if let Some(mut chunk) = extract_ts_named(&child, source, lang, ChunkKind::Class, depth) {
                    chunk.metadata.visibility = Some(Visibility::Public);
                    add_tag_once(tags, "kind:interface");
                    chunks.push(chunk);
                }
            }
            "type_alias_declaration" => {
                if let Some(mut chunk) = extract_ts_named(&child, source, lang, ChunkKind::Constant, depth) {
                    chunk.metadata.visibility = Some(Visibility::Public);
                    add_tag_once(tags, "kind:type");
                    chunks.push(chunk);
                }
            }
            "enum_declaration" => {
                if let Some(mut chunk) = extract_ts_named(&child, source, lang, ChunkKind::Class, depth) {
                    chunk.metadata.visibility = Some(Visibility::Public);
                    add_tag_once(tags, "kind:enum");
                    chunks.push(chunk);
                }
            }
            "lexical_declaration" | "variable_declaration" => {
                extract_ts_variable_declarations(&child, source, lang, depth, chunks, tags);
                // Mark last-added chunks as exported (public)
                if let Some(last) = chunks.last_mut() {
                    last.metadata.visibility = Some(Visibility::Public);
                }
            }
            _ => {}
        }
    }
}

/// Determine visibility for TS/JS nodes.
/// In TS/JS, top-level exported items are public; everything else is private (module-scoped).
fn ts_node_visibility(node: &tree_sitter::Node) -> Visibility {
    // Check if parent is an export_statement
    if let Some(parent) = node.parent() {
        if parent.kind() == "export_statement" {
            return Visibility::Public;
        }
    }
    Visibility::Private
}

/// Check if a TS/JS function/arrow is async.
fn ts_is_async(node: &tree_sitter::Node) -> bool {
    // For arrow_function and function_declaration, check for "async" keyword child
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.kind() == "async" {
            return true;
        }
    }
    false
}

/// ── Python AST walking ───────────────────────────────────────────

/// Walk Python AST nodes and extract chunks, tags, and imports.
fn walk_python_nodes(
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
            "function_definition" => {
                extract_py_function(&child, source, lang, depth, chunks, tags);
            }
            "class_definition" => {
                extract_py_class(&child, source, lang, depth, chunks, tags);
            }
            "import_statement" => {
                extract_py_import(&child, source, imports);
            }
            "import_from_statement" => {
                extract_py_import_from(&child, source, imports);
            }
            "expression_statement" => {
                // Module-level assignments (constants by convention: ALL_CAPS)
                if depth == 0 {
                    extract_py_module_constant(&child, source, lang, chunks);
                }
            }
            "decorated_definition" => {
                // Decorators wrap function/class definitions
                extract_py_decorated(&child, source, lang, depth, chunks, tags, imports);
            }
            _ => {}
        }
    }
}

/// Extract a Python function definition.
fn extract_py_function(
    node: &tree_sitter::Node,
    source: &str,
    lang: &str,
    depth: u8,
    chunks: &mut Vec<Chunk>,
    tags: &mut Vec<String>,
) {
    let name = match node.child_by_field_name("name") {
        Some(n) => n.utf8_text(source.as_bytes()).unwrap_or("").to_string(),
        None => return,
    };

    let vis = py_visibility(&name);
    let is_async = {
        let mut c = node.walk();
        let result = node.children(&mut c).any(|ch| ch.kind() == "async");
        result
    };

    let sig = extract_function_signature(node, source);
    let kind = if depth > 0 { ChunkKind::Method } else { ChunkKind::Function };

    if is_async {
        add_tag_once(tags, "async:true");
    }

    let tag = if depth > 0 { "kind:method" } else { "kind:function" };
    add_tag_once(tags, tag);

    chunks.push(Chunk {
        byte_range: node.byte_range(),
        kind,
        label: Some(name),
        depth,
        metadata: ChunkMetadata {
            signature: Some(sig),
            language: Some(lang.to_string()),
            visibility: Some(vis),
        },
    });
}

/// Extract a Python class definition.
fn extract_py_class(
    node: &tree_sitter::Node,
    source: &str,
    lang: &str,
    depth: u8,
    chunks: &mut Vec<Chunk>,
    tags: &mut Vec<String>,
) {
    let name = match node.child_by_field_name("name") {
        Some(n) => n.utf8_text(source.as_bytes()).unwrap_or("").to_string(),
        None => return,
    };

    let vis = py_visibility(&name);
    add_tag_once(tags, "kind:class");

    chunks.push(Chunk {
        byte_range: node.byte_range(),
        kind: ChunkKind::Class,
        label: Some(name),
        depth,
        metadata: ChunkMetadata {
            signature: None,
            language: Some(lang.to_string()),
            visibility: Some(vis),
        },
    });

    // Walk class body for methods
    if let Some(body) = node.child_by_field_name("body") {
        let mut cursor = body.walk();
        for child in body.children(&mut cursor) {
            match child.kind() {
                "function_definition" => {
                    extract_py_function(&child, source, lang, depth + 1, chunks, tags);
                }
                "decorated_definition" => {
                    extract_py_decorated(&child, source, lang, depth + 1, chunks, tags, &mut Vec::new());
                }
                _ => {}
            }
        }
    }
}

/// Extract `import foo` statements.
fn extract_py_import(node: &tree_sitter::Node, source: &str, imports: &mut Vec<LinkRef>) {
    let text = node.utf8_text(source.as_bytes()).unwrap_or("");
    // `import foo` or `import foo.bar` → target = "foo"
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.kind() == "dotted_name" {
            let module = child.utf8_text(source.as_bytes()).unwrap_or("");
            let top = module.split('.').next().unwrap_or(module);
            if !top.is_empty() {
                imports.push(LinkRef {
                    target: top.to_string(),
                    display: Some(text.trim().to_string()),
                    byte_offset: node.start_byte(),
                });
            }
            return;
        }
    }
}

/// Extract `from foo import bar` statements.
fn extract_py_import_from(node: &tree_sitter::Node, source: &str, imports: &mut Vec<LinkRef>) {
    let text = node.utf8_text(source.as_bytes()).unwrap_or("");
    // Find the module name
    if let Some(module_node) = node.child_by_field_name("module_name") {
        let module = module_node.utf8_text(source.as_bytes()).unwrap_or("");
        let top = module.split('.').next().unwrap_or(module);
        // Skip relative imports (starting with .)
        if !top.is_empty() && !top.starts_with('.') {
            imports.push(LinkRef {
                target: top.to_string(),
                display: Some(text.trim().to_string()),
                byte_offset: node.start_byte(),
            });
        }
    }
}

/// Extract module-level constants (ALL_CAPS assignments).
fn extract_py_module_constant(
    node: &tree_sitter::Node,
    source: &str,
    lang: &str,
    chunks: &mut Vec<Chunk>,
) {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.kind() == "assignment" {
            if let Some(left) = child.child_by_field_name("left") {
                let name = left.utf8_text(source.as_bytes()).unwrap_or("");
                // Convention: ALL_CAPS = constant
                if !name.is_empty() && name.chars().all(|c| c.is_uppercase() || c == '_' || c.is_ascii_digit()) {
                    chunks.push(Chunk {
                        byte_range: node.byte_range(),
                        kind: ChunkKind::Constant,
                        label: Some(name.to_string()),
                        depth: 0,
                        metadata: ChunkMetadata {
                            signature: None,
                            language: Some(lang.to_string()),
                            visibility: Some(py_visibility(name)),
                        },
                    });
                }
            }
        }
    }
}

/// Handle decorated definitions (decorators wrapping functions/classes).
fn extract_py_decorated(
    node: &tree_sitter::Node,
    source: &str,
    lang: &str,
    depth: u8,
    chunks: &mut Vec<Chunk>,
    tags: &mut Vec<String>,
    _imports: &mut Vec<LinkRef>,
) {
    let mut decorators = Vec::new();
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.kind() == "decorator" {
            let dec_text = child.utf8_text(source.as_bytes()).unwrap_or("");
            let dec_name = dec_text.trim().trim_start_matches('@');
            decorators.push(dec_name.to_string());
        }
    }

    let mut cursor2 = node.walk();
    for child in node.children(&mut cursor2) {
        match child.kind() {
            "function_definition" => {
                extract_py_function(&child, source, lang, depth, chunks, tags);
                // Add decorator convention tags
                for dec in &decorators {
                    if dec.starts_with("pytest.fixture") {
                        add_tag_once(tags, "convention:fixture");
                    } else if dec.starts_with("app.route") || dec.starts_with("router.") {
                        add_tag_once(tags, "convention:route");
                    } else if dec == "staticmethod" {
                        add_tag_once(tags, "convention:staticmethod");
                    } else if dec == "classmethod" {
                        add_tag_once(tags, "convention:classmethod");
                    } else if dec == "property" {
                        add_tag_once(tags, "convention:property");
                    }
                }
            }
            "class_definition" => {
                extract_py_class(&child, source, lang, depth, chunks, tags);
            }
            _ => {}
        }
    }
}

/// Python visibility heuristic: names starting with _ are private.
fn py_visibility(name: &str) -> Visibility {
    if name.starts_with('_') {
        Visibility::Private
    } else {
        Visibility::Public
    }
}

/// Helper: add a tag only if not already present.
fn add_tag_once(tags: &mut Vec<String>, tag: &str) {
    if !tags.iter().any(|t| t == tag) {
        tags.push(tag.to_string());
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

    // ── TypeScript tests ─────────────────────────────────────────

    #[test]
    fn test_can_parse_typescript_files() {
        let parser = CodeParser::new();
        assert!(parser.can_parse(Path::new("src/index.ts")));
        assert!(parser.can_parse(Path::new("src/App.tsx")));
        assert!(parser.can_parse(Path::new("src/util.js")));
        assert!(parser.can_parse(Path::new("src/Component.jsx")));
    }

    #[test]
    fn test_parse_ts_function_and_export() {
        let parser = CodeParser::new();
        let content = b"function internal() { return 1; }
export function greet(name: string): string {
    return `Hello, ${name}!`;
}
";
        let result = parser.parse(Path::new("utils.ts"), content).unwrap();

        assert!(result.tags.contains(&"lang:typescript".to_string()));
        assert!(result.tags.contains(&"kind:function".to_string()));

        let fns: Vec<_> = result.chunks.iter().filter(|c| c.kind == ChunkKind::Function).collect();
        assert_eq!(fns.len(), 2);

        let internal = fns.iter().find(|c| c.label.as_deref() == Some("internal")).unwrap();
        assert_eq!(internal.metadata.visibility, Some(Visibility::Private));

        let greet = fns.iter().find(|c| c.label.as_deref() == Some("greet")).unwrap();
        assert_eq!(greet.metadata.visibility, Some(Visibility::Public));
    }

    #[test]
    fn test_parse_ts_class_with_methods() {
        let parser = CodeParser::new();
        let content = b"export class UserService {
    constructor(private db: Database) {}

    async getUser(id: string): Promise<User> {
        return this.db.find(id);
    }

    deleteUser(id: string): void {
        this.db.delete(id);
    }
}
";
        let result = parser.parse(Path::new("service.ts"), content).unwrap();

        assert!(result.tags.contains(&"kind:class".to_string()));
        assert!(result.tags.contains(&"kind:method".to_string()));

        let class = result.chunks.iter().find(|c| c.kind == ChunkKind::Class).unwrap();
        assert_eq!(class.label.as_deref(), Some("UserService"));
        assert_eq!(class.metadata.visibility, Some(Visibility::Public));

        let methods: Vec<_> = result.chunks.iter().filter(|c| c.kind == ChunkKind::Method).collect();
        assert!(methods.len() >= 2, "expected at least 2 methods, got {}", methods.len());
    }

    #[test]
    fn test_parse_ts_interface_and_type() {
        let parser = CodeParser::new();
        let content = b"export interface User {
    id: string;
    name: string;
}

export type UserId = string;

interface Internal {
    secret: boolean;
}
";
        let result = parser.parse(Path::new("types.ts"), content).unwrap();

        assert!(result.tags.contains(&"kind:interface".to_string()));
        assert!(result.tags.contains(&"kind:type".to_string()));

        let user = result.chunks.iter().find(|c| c.label.as_deref() == Some("User")).unwrap();
        assert_eq!(user.metadata.visibility, Some(Visibility::Public));

        let internal = result.chunks.iter().find(|c| c.label.as_deref() == Some("Internal")).unwrap();
        assert_eq!(internal.metadata.visibility, Some(Visibility::Private));
    }

    #[test]
    fn test_parse_ts_imports() {
        let parser = CodeParser::new();
        let content = b"import { useState, useEffect } from 'react';
import express from 'express';
import { Foo } from './local';
import { Bar } from '@scope/pkg';

export function App() { return null; }
";
        let result = parser.parse(Path::new("App.tsx"), content).unwrap();

        let targets: Vec<_> = result.links.iter().map(|l| l.target.as_str()).collect();
        assert!(targets.contains(&"react"), "should have react import");
        assert!(targets.contains(&"express"), "should have express import");
        assert!(targets.contains(&"@scope/pkg"), "should have scoped import");
        // Relative imports should be skipped
        assert!(!targets.iter().any(|t| t.starts_with(".")), "should skip relative imports");
    }

    #[test]
    fn test_parse_ts_arrow_functions() {
        let parser = CodeParser::new();
        let content = b"export const fetchData = async (url: string) => {
    const res = await fetch(url);
    return res.json();
};

const helper = () => 42;
";
        let result = parser.parse(Path::new("api.ts"), content).unwrap();

        assert!(result.tags.contains(&"kind:function".to_string()));

        let fns: Vec<_> = result.chunks.iter().filter(|c| c.kind == ChunkKind::Function).collect();
        assert_eq!(fns.len(), 2);

        let fetch = fns.iter().find(|c| c.label.as_deref() == Some("fetchData")).unwrap();
        assert_eq!(fetch.metadata.visibility, Some(Visibility::Public));

        let helper = fns.iter().find(|c| c.label.as_deref() == Some("helper")).unwrap();
        assert_eq!(helper.metadata.visibility, Some(Visibility::Private));
    }

    #[test]
    fn test_parse_tsx_without_errors() {
        let parser = CodeParser::new();
        let content = b"import React from 'react';

interface Props {
    name: string;
}

export function Greeting({ name }: Props) {
    return <div>Hello, {name}!</div>;
}
";
        let result = parser.parse(Path::new("Greeting.tsx"), content).unwrap();
        assert!(result.tags.contains(&"lang:typescript".to_string()));
        assert!(!result.chunks.is_empty(), "should extract chunks from TSX");
    }

    #[test]
    fn test_parse_ts_enum() {
        let parser = CodeParser::new();
        let content = b"export enum Direction {
    Up,
    Down,
    Left,
    Right,
}
";
        let result = parser.parse(Path::new("enums.ts"), content).unwrap();
        assert!(result.tags.contains(&"kind:enum".to_string()));
        let e = result.chunks.iter().find(|c| c.label.as_deref() == Some("Direction")).unwrap();
        assert_eq!(e.kind, ChunkKind::Class);
        assert_eq!(e.metadata.visibility, Some(Visibility::Public));
    }

    // ── Python tests ─────────────────────────────────────────────

    #[test]
    fn test_can_parse_python_files() {
        let parser = CodeParser::new();
        assert!(parser.can_parse(Path::new("main.py")));
        assert!(parser.can_parse(Path::new("src/utils.py")));
    }

    #[test]
    fn test_parse_py_function() {
        let parser = CodeParser::new();
        let content = b"def greet(name: str) -> str:
    return f'Hello, {name}!'

def _internal():
    pass
";
        let result = parser.parse(Path::new("utils.py"), content).unwrap();

        assert!(result.tags.contains(&"lang:python".to_string()));
        assert!(result.tags.contains(&"kind:function".to_string()));

        let fns: Vec<_> = result.chunks.iter().filter(|c| c.kind == ChunkKind::Function).collect();
        assert_eq!(fns.len(), 2);

        let greet = fns.iter().find(|c| c.label.as_deref() == Some("greet")).unwrap();
        assert_eq!(greet.metadata.visibility, Some(Visibility::Public));

        let internal = fns.iter().find(|c| c.label.as_deref() == Some("_internal")).unwrap();
        assert_eq!(internal.metadata.visibility, Some(Visibility::Private));
    }

    #[test]
    fn test_parse_py_class_with_methods() {
        let parser = CodeParser::new();
        let content = b"class UserService:
    def __init__(self, db):
        self.db = db

    def get_user(self, user_id: str):
        return self.db.find(user_id)

    def _validate(self, data):
        pass
";
        let result = parser.parse(Path::new("service.py"), content).unwrap();

        assert!(result.tags.contains(&"kind:class".to_string()));
        assert!(result.tags.contains(&"kind:method".to_string()));

        let class = result.chunks.iter().find(|c| c.kind == ChunkKind::Class).unwrap();
        assert_eq!(class.label.as_deref(), Some("UserService"));
        assert_eq!(class.depth, 0);

        let methods: Vec<_> = result.chunks.iter().filter(|c| c.kind == ChunkKind::Method).collect();
        assert_eq!(methods.len(), 3);

        let validate = methods.iter().find(|c| c.label.as_deref() == Some("_validate")).unwrap();
        assert_eq!(validate.metadata.visibility, Some(Visibility::Private));
        assert_eq!(validate.depth, 1);
    }

    #[test]
    fn test_parse_py_imports() {
        let parser = CodeParser::new();
        let content = b"import os
import sys
from collections import OrderedDict
from .local import helper
from flask import Flask

def main():
    pass
";
        let result = parser.parse(Path::new("app.py"), content).unwrap();

        let targets: Vec<_> = result.links.iter().map(|l| l.target.as_str()).collect();
        assert!(targets.contains(&"os"), "should have os import");
        assert!(targets.contains(&"sys"), "should have sys import");
        assert!(targets.contains(&"collections"), "should have collections import");
        assert!(targets.contains(&"flask"), "should have flask import");
        // Relative imports should be skipped
        assert!(!targets.iter().any(|t| t.starts_with(".")), "should skip relative imports");
    }

    #[test]
    fn test_parse_py_constants() {
        let parser = CodeParser::new();
        let content = b"MAX_SIZE = 1024
DEFAULT_NAME = 'test'
_PRIVATE_CONST = True

def func():
    pass
";
        let result = parser.parse(Path::new("config.py"), content).unwrap();

        let consts: Vec<_> = result.chunks.iter().filter(|c| c.kind == ChunkKind::Constant).collect();
        assert!(consts.len() >= 2, "expected at least 2 constants, got {}", consts.len());

        let private = consts.iter().find(|c| c.label.as_deref() == Some("_PRIVATE_CONST"));
        if let Some(p) = private {
            assert_eq!(p.metadata.visibility, Some(Visibility::Private));
        }
    }

    #[test]
    fn test_parse_py_decorated_functions() {
        let parser = CodeParser::new();
        let content = b"import pytest

@pytest.fixture
def db_connection():
    return connect()

@app.route('/users')
def list_users():
    return []
";
        let result = parser.parse(Path::new("test_app.py"), content).unwrap();

        let fns: Vec<_> = result.chunks.iter().filter(|c| c.kind == ChunkKind::Function).collect();
        assert!(fns.len() >= 2, "expected at least 2 functions, got {}", fns.len());

        // Convention tags from decorators
        assert!(result.tags.contains(&"convention:fixture".to_string()));
        assert!(result.tags.contains(&"convention:route".to_string()));
    }

    #[test]
    fn test_parse_py_test_file_detection() {
        let parser = CodeParser::new();
        let content = b"def test_something():
    assert True
";
        let result = parser.parse(Path::new("tests/test_utils.py"), content).unwrap();
        assert_eq!(result.auto_type, Some(DocType::TestFile));
    }
}
