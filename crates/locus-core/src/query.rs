use std::path::{Path, PathBuf};

use crate::bitmap::BitmapError;
use crate::registry::{BitmapCatalogEntry, RegistryError};
use crate::types::{BitmapCategory, BitmapKey, ChunkId, DocId, Timestamp};

// ── Filter expression ────────────────────────────────────────────

/// A filter expression in a query.
#[derive(Debug, Clone)]
pub enum Filter {
    /// Match a single bitmap key, e.g. `Filter::Key("tag:work")`
    Key(BitmapKey),
    /// Boolean NOT of a filter
    Not(Box<Filter>),
    /// Boolean AND of multiple filters
    And(Vec<Filter>),
    /// Boolean OR of multiple filters
    Or(Vec<Filter>),
}

// ── Request / Response ───────────────────────────────────────────

/// A query request from any interface (CLI, MCP, HTTP).
#[derive(Debug, Clone)]
pub struct QueryRequest {
    /// The filter expression to resolve.
    pub filter: Filter,
    /// Maximum number of results to return.
    pub limit: Option<u32>,
    /// Offset for pagination.
    pub offset: Option<u32>,
}

/// A single match result — a pointer to relevant content.
#[derive(Debug, Clone, serde::Serialize)]
pub struct MatchPointer {
    pub doc_id: DocId,
    pub file_path: PathBuf,
    pub source_type: String,
    pub chunks: Vec<ChunkPointer>,
    pub matched_filters: Vec<BitmapKey>,
    pub auto_type: Option<String>,
    pub score: Option<f32>,
    pub last_modified: Timestamp,
}

/// A pointer to a specific chunk within a document.
///
/// The `children` field is populated by [`nest_chunks`] and reflects the
/// table-of-contents structure inferred from `depth`.  It is omitted from
/// serialization when empty (leaf nodes / flat results that were never nested).
#[derive(Debug, Clone, serde::Serialize)]
pub struct ChunkPointer {
    pub chunk_id: ChunkId,
    pub kind: String,
    pub byte_start: u32,
    pub byte_end: u32,
    pub label: Option<String>,
    /// Nesting depth: heading level for markdown (1 = H1), scope depth for
    /// code (0 for module-level, 1 for class members, etc.).
    /// Frontmatter and Body chunks have depth 0 and are always roots.
    pub depth: u8,
    /// Nested child pointers (table-of-contents shape derived from depth).
    /// Populated by [`nest_chunks`]; omitted from JSON when empty.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub children: Vec<ChunkPointer>,
}

/// Arrange a flat list of `ChunkPointer`s (in document byte order) into a
/// **nested tree** reflecting the heading / scope depth recorded at index time.
///
/// ## Algorithm
///
/// Input is sorted by `byte_start` first (defensive — the registry normally
/// returns chunks in `chunk_id` order which equals document order, but this
/// makes ordering explicit).
///
/// We maintain a **stack** of open ancestor nodes.  Each entry records the
/// `depth` of the node and the path (indices) to that node inside the output
/// tree, so we can append children to it.
///
/// For each incoming chunk:
/// 1. Pop stack entries whose `depth >= chunk.depth` — they are closed.
/// 2. If the stack is non-empty the chunk becomes the last child of the top;
///    otherwise it is a new root.
/// 3. Push the chunk onto the stack, unless it can never parent (see below).
///
/// `Frontmatter` and `Body` chunks are never used as parents — they are
/// containers of prose, not scopes. Structural chunks at depth 0 (top-level
/// code items: functions, classes, impl blocks) CAN parent deeper chunks,
/// since the code parser starts scope depth at 0.
///
/// ## Edge cases
/// - **Empty input** → returns empty Vec.
/// - **Skipped levels** (e.g. H1 immediately followed by H3) → H3 becomes a
///   child of H1 (the nearest strictly-shallower ancestor).
/// - **Depth drops by > 1** (e.g. H3 then H1) → the H3 subtree is closed and
///   H1 becomes a new root.
/// - **Multiple same-depth siblings** → all become children of the same parent.
/// - **Only Body / Frontmatter** (all depth 0) → all are roots, flat list.
/// - **Code files** (Class at depth 0, Methods at depth 1) → methods become
///   children of their class.
/// - **Equal depths in sequence** → siblings under the same parent.
pub fn nest_chunks(mut flat: Vec<ChunkPointer>) -> Vec<ChunkPointer> {
    if flat.is_empty() {
        return flat;
    }

    // Sort defensively by byte position.
    flat.sort_by_key(|c| c.byte_start);

    // We build the output tree by consuming `flat` and threading each node
    // into the right place.
    //
    // `roots` holds the top-level output nodes.
    // `stack` records the path to the currently-open ancestor at each depth
    // level: each entry is (depth, path) where path is a Vec<usize> of
    // child-indices from the root down to that node.
    //
    // We cannot hold `&mut ChunkPointer` in the stack while also mutating
    // `roots`, so we use index paths and a helper to navigate the tree.

    let mut roots: Vec<ChunkPointer> = Vec::new();
    // Stack entry: (node_depth, path_to_node)
    let mut stack: Vec<(u8, Vec<usize>)> = Vec::new();

    for chunk in flat {
        let chunk_depth = chunk.depth;
        // Frontmatter/Body hold prose, not scope — they never adopt children.
        let can_parent = chunk.kind != "Frontmatter" && chunk.kind != "Body";

        // Pop ancestors that are not strictly shallower than the current chunk.
        while let Some((top_depth, _)) = stack.last() {
            if *top_depth >= chunk_depth {
                stack.pop();
            } else {
                break;
            }
        }

        // Clone the parent path before mutating `roots` or `stack`.
        let parent_path_opt: Option<Vec<usize>> =
            stack.last().map(|(_, path)| path.clone());

        if let Some(parent_path) = parent_path_opt {
            // Append as child of the top-of-stack node.
            let parent = get_mut_at(roots.as_mut_slice(), &parent_path);
            parent.children.push(chunk);
            let child_idx = parent.children.len() - 1;
            if can_parent {
                let mut new_path = parent_path;
                new_path.push(child_idx);
                stack.push((chunk_depth, new_path));
            }
        } else {
            // No suitable ancestor — new root.
            roots.push(chunk);
            if can_parent {
                let root_idx = roots.len() - 1;
                stack.push((chunk_depth, vec![root_idx]));
            }
        }
    }

    roots
}

/// Navigate into a nested `ChunkPointer` tree via an index path.
///
/// Each element of `path` is a child index: `path[0]` indexes into `roots`,
/// `path[1]` indexes into `roots[path[0]].children`, and so on.
fn get_mut_at<'a>(roots: &'a mut [ChunkPointer], path: &[usize]) -> &'a mut ChunkPointer {
    assert!(!path.is_empty(), "path must not be empty");
    // Walk down the tree one level at a time.
    // We use a recursive helper that Rust's borrow checker can follow.
    get_mut_at_rec(roots, path)
}

fn get_mut_at_rec<'a>(nodes: &'a mut [ChunkPointer], path: &[usize]) -> &'a mut ChunkPointer {
    if path.len() == 1 {
        &mut nodes[path[0]]
    } else {
        get_mut_at_rec(&mut nodes[path[0]].children, &path[1..])
    }
}

/// The result of a query.
#[derive(Debug, serde::Serialize)]
pub struct QueryResult {
    pub matches: Vec<MatchPointer>,
    /// Total matching docs before limit/offset.
    pub total_matching: u32,
    /// Query duration in microseconds.
    pub query_time_us: u64,
}

/// The result of inspecting a single document in the index.
#[derive(Debug, Clone, serde::Serialize)]
pub struct InspectResult {
    pub doc_id: DocId,
    pub file_path: PathBuf,
    pub source_type: String,
    pub auto_type: Option<String>,
    pub blake3_hash: String,
    pub last_indexed: Timestamp,
    pub chunks: Vec<ChunkPointer>,
    pub bitmap_keys: Vec<BitmapKey>,
}

/// Summary status of the entire index.
#[derive(Debug, Clone, serde::Serialize)]
pub struct IndexStatus {
    pub total_documents: u32,
    pub total_bitmaps: usize,
    pub tombstoned: u64,
    pub next_doc_id: DocId,
    pub next_chunk_id: ChunkId,
}

/// A filter entry for discovery.
#[derive(Debug, Clone, serde::Serialize)]
pub struct FilterEntry {
    pub key: BitmapKey,
    pub cardinality: u32,
}

// ── Trait ─────────────────────────────────────────────────────────

/// Query engine that resolves bitmap filters and returns structural pointers.
pub trait QueryEngine: Send + Sync {
    fn query(&self, request: QueryRequest) -> Result<QueryResult, QueryError>;

    /// List available bitmap keys for filter discovery.
    fn list_filters(
        &self,
        category: Option<BitmapCategory>,
    ) -> Result<Vec<BitmapCatalogEntry>, QueryError>;

    /// Inspect a single file in the index by path.
    fn inspect(&self, path: &Path) -> Result<Option<InspectResult>, QueryError>;

    /// Get summary status of the index.
    fn status(&self) -> Result<IndexStatus, QueryError>;

    /// List available filter keys with cardinality, optionally by category prefix.
    fn list_filter_keys(&self, category: Option<&str>) -> Result<Vec<FilterEntry>, QueryError>;
}

// ── Error ─────────────────────────────────────────────────────────

#[derive(Debug, thiserror::Error)]
pub enum QueryError {
    #[error("unknown bitmap key: {0}")]
    UnknownFilter(BitmapKey),
    #[error(transparent)]
    Bitmap(#[from] BitmapError),
    #[error(transparent)]
    Registry(#[from] RegistryError),
    #[error("semantic error: {0}")]
    Semantic(String),
}

// ── Unit tests ────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn ptr(chunk_id: u32, byte_start: u32, byte_end: u32, depth: u8, label: &str) -> ChunkPointer {
        ChunkPointer {
            chunk_id,
            kind: "Section".to_string(),
            byte_start,
            byte_end,
            label: Some(label.to_string()),
            depth,
            children: vec![],
        }
    }

    fn ptr_kind(chunk_id: u32, byte_start: u32, byte_end: u32, depth: u8, kind: &str) -> ChunkPointer {
        ChunkPointer {
            chunk_id,
            kind: kind.to_string(),
            byte_start,
            byte_end,
            label: None,
            depth,
            children: vec![],
        }
    }

    #[test]
    fn empty_input() {
        let result = nest_chunks(vec![]);
        assert!(result.is_empty());
    }

    #[test]
    fn only_body_chunks_are_flat_roots() {
        // All depth-0 chunks → all remain roots, no nesting
        let flat = vec![
            ptr_kind(1, 0, 100, 0, "Frontmatter"),
            ptr_kind(2, 100, 500, 0, "Body"),
        ];
        let nested = nest_chunks(flat);
        assert_eq!(nested.len(), 2);
        assert!(nested[0].children.is_empty());
        assert!(nested[1].children.is_empty());
    }

    #[test]
    fn simple_h1_h2_h3_hierarchy() {
        // H1 → H2 → H3
        let flat = vec![
            ptr(1, 0, 400, 1, "Intro"),
            ptr(2, 50, 300, 2, "Sub"),
            ptr(3, 100, 200, 3, "SubSub"),
        ];
        let nested = nest_chunks(flat);
        assert_eq!(nested.len(), 1, "one root");
        assert_eq!(nested[0].chunk_id, 1);
        assert_eq!(nested[0].children.len(), 1);
        assert_eq!(nested[0].children[0].chunk_id, 2);
        assert_eq!(nested[0].children[0].children.len(), 1);
        assert_eq!(nested[0].children[0].children[0].chunk_id, 3);
    }

    #[test]
    fn skipped_levels_h1_then_h3() {
        // H1 at depth 1, then H3 at depth 3 (no H2) — H3 is child of H1
        let flat = vec![
            ptr(1, 0, 400, 1, "Title"),
            ptr(2, 50, 300, 3, "Deep"),
        ];
        let nested = nest_chunks(flat);
        assert_eq!(nested.len(), 1);
        assert_eq!(nested[0].children.len(), 1);
        assert_eq!(nested[0].children[0].chunk_id, 2);
    }

    #[test]
    fn depth_drops_by_more_than_one() {
        // H3 then H1 — H1 becomes a new root
        let flat = vec![
            ptr(1, 0, 50, 1, "A"),
            ptr(2, 50, 150, 3, "Deep"),
            ptr(3, 150, 300, 1, "B"),
        ];
        let nested = nest_chunks(flat);
        assert_eq!(nested.len(), 2, "two roots");
        assert_eq!(nested[0].chunk_id, 1);
        assert_eq!(nested[0].children.len(), 1);
        assert_eq!(nested[0].children[0].chunk_id, 2);
        assert_eq!(nested[1].chunk_id, 3);
        assert!(nested[1].children.is_empty());
    }

    #[test]
    fn multiple_same_depth_roots() {
        // Three H1 sections — all become roots
        let flat = vec![
            ptr(1, 0, 100, 1, "A"),
            ptr(2, 100, 200, 1, "B"),
            ptr(3, 200, 300, 1, "C"),
        ];
        let nested = nest_chunks(flat);
        assert_eq!(nested.len(), 3);
        for n in &nested {
            assert!(n.children.is_empty());
        }
    }

    #[test]
    fn equal_depths_are_siblings() {
        // H1 containing two H2 siblings
        let flat = vec![
            ptr(1, 0, 500, 1, "Root"),
            ptr(2, 50, 200, 2, "Child A"),
            ptr(3, 200, 400, 2, "Child B"),
        ];
        let nested = nest_chunks(flat);
        assert_eq!(nested.len(), 1);
        assert_eq!(nested[0].children.len(), 2);
        assert_eq!(nested[0].children[0].chunk_id, 2);
        assert_eq!(nested[0].children[1].chunk_id, 3);
    }

    #[test]
    fn code_file_class_and_methods() {
        // The code parser starts scope depth at 0: a top-level impl/class is
        // depth 0 and its methods are depth 1. Structural depth-0 chunks CAN
        // parent (unlike Frontmatter/Body).
        let flat = vec![
            ptr_kind(1, 0, 500, 0, "Class"),
            ptr_kind(2, 50, 200, 1, "Method"),
            ptr_kind(3, 200, 400, 1, "Method"),
        ];
        let nested = nest_chunks(flat);
        assert_eq!(nested.len(), 1, "class is root");
        assert_eq!(nested[0].children.len(), 2, "two methods as children");
    }

    #[test]
    fn code_file_toplevel_item_after_methods_is_new_root() {
        // impl block (depth 0) with two methods (depth 1), then a top-level
        // function (depth 0) — the function must close the impl scope and
        // become a sibling root, not a child.
        let flat = vec![
            ptr_kind(1, 0, 300, 0, "Class"),
            ptr_kind(2, 50, 150, 1, "Method"),
            ptr_kind(3, 150, 290, 1, "Method"),
            ptr_kind(4, 300, 400, 0, "Function"),
        ];
        let nested = nest_chunks(flat);
        assert_eq!(nested.len(), 2, "impl and function are sibling roots");
        assert_eq!(nested[0].children.len(), 2);
        assert_eq!(nested[1].chunk_id, 4);
        assert!(nested[1].children.is_empty());
    }

    #[test]
    fn body_never_parents_sections() {
        // Pre-heading Body prose (depth 0) followed by an H1 — the H1 must be
        // a root, not a child of the Body chunk.
        let flat = vec![
            ptr_kind(1, 0, 50, 0, "Body"),
            ptr(2, 50, 400, 1, "Title"),
        ];
        let nested = nest_chunks(flat);
        assert_eq!(nested.len(), 2);
        assert!(nested[0].children.is_empty());
        assert_eq!(nested[1].chunk_id, 2);
    }

    #[test]
    fn unsorted_input_gets_sorted_by_byte_start() {
        // Provide input out of order — output must be in document order
        let flat = vec![
            ptr(3, 200, 300, 2, "Sub"),
            ptr(1, 0, 400, 1, "Root"),
            ptr(2, 50, 200, 2, "Sub2"),
        ];
        let nested = nest_chunks(flat);
        // After sorting: chunk 1 (0..400) → root; chunk 2 (50..200) → child;
        // chunk 3 (200..300) → sibling child
        assert_eq!(nested.len(), 1);
        assert_eq!(nested[0].chunk_id, 1);
        assert_eq!(nested[0].children.len(), 2);
        assert_eq!(nested[0].children[0].chunk_id, 2);
        assert_eq!(nested[0].children[1].chunk_id, 3);
    }

    #[test]
    fn frontmatter_plus_sections() {
        // Frontmatter (depth 0) then H1 (depth 1) then H2 (depth 2)
        let flat = vec![
            ptr_kind(1, 0, 30, 0, "Frontmatter"),
            ptr(2, 30, 400, 1, "Title"),
            ptr(3, 60, 300, 2, "Sub"),
        ];
        let nested = nest_chunks(flat);
        // Frontmatter: root (depth 0, never parent)
        // Title (H1): root
        // Sub (H2): child of Title
        assert_eq!(nested.len(), 2);
        assert_eq!(nested[0].chunk_id, 1);
        assert!(nested[0].children.is_empty());
        assert_eq!(nested[1].chunk_id, 2);
        assert_eq!(nested[1].children.len(), 1);
        assert_eq!(nested[1].children[0].chunk_id, 3);
    }
}
