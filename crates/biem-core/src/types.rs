use std::collections::HashMap;
use std::ops::Range;
use std::path::PathBuf;

/// Unique identifier for a document (file) in the index.
/// Monotonically increasing, assigned by the Registry.
pub type DocId = u32;

/// Unique identifier for a chunk within a document.
/// Monotonically increasing, assigned by the Registry.
pub type ChunkId = u32;

/// A namespaced key for a bitmap in the store.
/// Examples: "tag:work", "folder:/projects", "type:task", "link:ProjectAlpha"
pub type BitmapKey = String;

/// Timestamp as seconds since Unix epoch.
pub type Timestamp = i64;

/// The source type of an indexed document.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SourceType {
    Obsidian,
    // Future: Code, Confluence, etc.
}

/// Auto-detected note type based on structural analysis.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NoteType {
    /// Default — no strong structural signal.
    Note,
    /// High density of `[ ]` / `[x]` items.
    Task,
    /// High density of `[[links]]`, map-of-content pattern.
    Moc,
    /// Has url/isbn/source in frontmatter.
    Reference,
    // Future: Person, Meeting, etc.
}

/// The category of a bitmap key, for catalog queries.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BitmapCategory {
    Tag,
    Folder,
    Link,
    Type,
    Source,
}

// ── Chunk model ──────────────────────────────────────────────────

/// A chunk boundary identified by the parser.
/// Flexible enough for both documents and code.
#[derive(Debug, Clone)]
pub struct Chunk {
    /// Byte range within the source file.
    pub byte_range: Range<usize>,
    /// What kind of chunk this is.
    pub kind: ChunkKind,
    /// Human-readable label (heading text, function name, class name).
    pub label: Option<String>,
    /// Nesting depth (heading depth for markdown, scope depth for code).
    pub depth: u8,
    /// Structured metadata specific to the chunk kind.
    pub metadata: ChunkMetadata,
}

/// The kind of chunk — document or code.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChunkKind {
    // Document chunks
    Section,
    Frontmatter,
    Body,

    // Code chunks (future)
    Function,
    Method,
    Class,
    Module,
    Import,
    Constant,
}

/// Kind-specific metadata. Avoids polluting every chunk
/// with fields only relevant to one type.
#[derive(Debug, Clone, Default)]
pub struct ChunkMetadata {
    /// For code: function signature, class declaration line.
    pub signature: Option<String>,
    /// For code: language identifier.
    pub language: Option<String>,
    /// For code: visibility/export status.
    pub visibility: Option<Visibility>,
}

/// Visibility of a code symbol.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Visibility {
    Public,
    Private,
    /// e.g., `pub(crate)` in Rust.
    Internal,
}

// ── Link model ───────────────────────────────────────────────────

/// A reference to another document (link target).
#[derive(Debug, Clone)]
pub struct LinkRef {
    /// The target as written in the source, e.g. "ProjectAlpha" from `[[ProjectAlpha]]`.
    pub target: String,
    /// Optional display text, e.g. "my project" from `[[ProjectAlpha|my project]]`.
    pub display: Option<String>,
    /// Byte position of the link in the source file.
    pub byte_offset: usize,
}

// ── Parse result ─────────────────────────────────────────────────

/// The complete output of parsing a single file.
#[derive(Debug, Clone)]
pub struct ParseResult {
    /// Chunks identified in the file (at minimum, one chunk = the whole file).
    pub chunks: Vec<Chunk>,
    /// Tags extracted from frontmatter and inline (`#tag`).
    pub tags: Vec<String>,
    /// Links to other documents.
    pub links: Vec<LinkRef>,
    /// Auto-detected note type, if confident enough.
    pub auto_type: Option<NoteType>,
    /// Parsed YAML frontmatter as key-value pairs.
    pub frontmatter: HashMap<String, serde_json::Value>,
}

// ── Change events ────────────────────────────────────────────────

/// A filesystem change event from the watcher.
#[derive(Debug, Clone)]
pub struct ChangeEvent {
    pub path: PathBuf,
    pub kind: ChangeKind,
}

/// The kind of filesystem change.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChangeKind {
    Created,
    Modified,
    Deleted,
    Renamed { from: PathBuf },
}
