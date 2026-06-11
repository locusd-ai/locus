//! MCP (Model Context Protocol) server for BIEM.
//!
//! Exposes 7 tools: locus_search, locus_inspect, locus_status, locus_filters,
//! locus_graph, locus_remote_ingest, locus_semantic.
//! Runs over stdio transport for integration with LLM clients.

use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock};

use rmcp::{
    ServerHandler,
    handler::server::{router::tool::ToolRouter, wrapper::Parameters},
    tool, tool_handler, tool_router,
};
use schemars::JsonSchema;
use serde::Deserialize;

use locus_core::graph::{
    CentralityAlgorithm, Direction, EdgeFilter, ExpandSpec, GraphOp, GraphQueryEngine,
    GraphQueryRequest,
};
use locus_core::query::{QueryEngine, QueryRequest};
use locus_core::semantic::{Embedder, SemanticQueryRequest};
use locus_embed::{FastEmbedEmbedder, UsearchVectorStore};
use locus_ingest::IngestionPipeline;
use locus_query::{parse_filter, BitmapQueryEngine, Filter};

// ── Tool parameter types ─────────────────────────────────────────

#[derive(Debug, Deserialize, JsonSchema)]
pub struct SearchParams {
    /// Filter expression string using the Locus query DSL.
    /// Terms are `namespace:value` pairs (e.g. `tag:work`, `folder:crates/locus-query`).
    /// Namespaces: tag, folder, type, source, lang, kind, topic.
    /// Operators: AND, OR, NOT (case-insensitive). Parentheses for grouping.
    /// Precedence: NOT > AND > OR.
    /// Example: `"tag:work AND (type:task OR type:note) AND NOT folder:archive"`
    /// Use `locus_filters` to discover available keys.
    /// If omitted, falls back to `filters` array; if both absent, all indexed docs are matched.
    #[serde(default)]
    pub filter: Option<String>,
    /// Legacy: bitmap filter keys to combine (e.g. ["tag:work", "type:task"]).
    /// Ignored when `filter` is provided.
    #[serde(default)]
    pub filters: Vec<String>,
    /// Boolean operation for legacy `filters`: "and" (default) or "or"
    #[serde(default = "default_op")]
    pub op: String,
    /// Maximum results to return
    #[serde(default = "default_limit")]
    pub limit: u32,
}

fn default_op() -> String {
    "and".into()
}

fn default_limit() -> u32 {
    20
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct SemanticParams {
    /// Natural language query text for vector similarity search.
    pub query: String,
    /// Optional filter expression to pre-filter the bitmap index before vector search.
    /// Same syntax as `locus_search` `filter` param.
    /// Example: `"source:code AND lang:rust AND NOT kind:test"`
    /// If omitted, all indexed documents are candidates.
    #[serde(default)]
    pub filter: Option<String>,
    /// Maximum number of results to return (default 10).
    #[serde(default = "default_top_k")]
    pub top_k: usize,
    /// Apply cross-encoder reranker for higher quality (default false, slower).
    #[serde(default)]
    pub rerank: bool,
}

fn default_top_k() -> usize {
    10
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct InspectParams {
    /// Absolute or relative file path to inspect
    pub file_path: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct FiltersParams {
    /// Optional category filter: "tag", "folder", "link", "type", "source"
    pub category: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct GraphParams {
    /// Operation: "neighbours", "expand", "path", "central", "stats"
    pub operation: String,
    /// Source doc ID (required for neighbours, expand, path)
    pub doc_id: Option<u32>,
    /// Target doc ID (required for path)
    pub to_doc_id: Option<u32>,
    /// Number of hops for expand (default 1)
    #[serde(default = "default_hops")]
    pub hops: u8,
    /// Traversal direction: "outgoing" (default), "incoming", "both"
    #[serde(default = "default_direction")]
    pub direction: String,
    /// Centrality algorithm: "pagerank" (default), "indegree", "outdegree"
    #[serde(default = "default_algorithm")]
    pub algorithm: String,
    /// Maximum results (default 10)
    #[serde(default = "default_k")]
    pub k: u32,
}

fn default_hops() -> u8 { 1 }
fn default_direction() -> String { "outgoing".into() }
fn default_algorithm() -> String { "pagerank".into() }
fn default_k() -> u32 { 10 }

#[derive(Debug, Deserialize, JsonSchema)]
pub struct RemoteIngestParams {
    /// Synthetic document path, e.g. `confluence://SPACE/12345.confluence`
    pub path: String,
    /// Base64-encoded raw document bytes (as returned by a remote source's fetch_content)
    pub content_b64: String,
}

// ── Lazy semantic state ─────────────────────────────────────────

/// Initialised once on first `locus_semantic` call; cached for daemon lifetime.
struct SemanticState {
    embedder: FastEmbedEmbedder,
    vector_store: UsearchVectorStore,
}

// ── MCP Server ───────────────────────────────────────────────────

/// The BIEM MCP server handler.
pub struct BiemMcpServer {
    engine: Arc<dyn QueryEngine>,
    graph: Option<Arc<dyn GraphQueryEngine>>,
    ingest: Option<Arc<Mutex<IngestionPipeline>>>,
    /// Concrete engine for semantic queries (inherent method, not on QueryEngine trait).
    semantic_engine: Option<Arc<BitmapQueryEngine>>,
    /// Data directory: vectors.usearch lives at `<data_dir>/vectors.usearch`.
    semantic_data_dir: Option<PathBuf>,
    /// Lazy-init semantic state (embedder + vector store). OnceLock because model load is slow.
    semantic_state: OnceLock<Result<SemanticState, String>>,
    tool_router: ToolRouter<Self>,
}

impl BiemMcpServer {
    pub fn new(engine: Arc<dyn QueryEngine>) -> Self {
        let tool_router = Self::tool_router();
        Self {
            engine,
            graph: None,
            ingest: None,
            semantic_engine: None,
            semantic_data_dir: None,
            semantic_state: OnceLock::new(),
            tool_router,
        }
    }

    pub fn with_graph(mut self, g: Arc<dyn GraphQueryEngine>) -> Self {
        self.graph = Some(g);
        self
    }

    pub fn with_ingest(mut self, p: Arc<Mutex<IngestionPipeline>>) -> Self {
        self.ingest = Some(p);
        self
    }

    /// Wire a concrete `BitmapQueryEngine` for semantic queries and provide the data directory
    /// so the vector store path can be resolved (`<data_dir>/vectors.usearch`).
    pub fn with_semantic(mut self, engine: Arc<BitmapQueryEngine>, data_dir: PathBuf) -> Self {
        self.semantic_engine = Some(engine);
        self.semantic_data_dir = Some(data_dir);
        self
    }

    // ── Internal helpers ──────────────────────────────────────────

    /// Build a match-all filter by enumerating source: keys via the query engine.
    fn build_match_all_filter(&self) -> Filter {
        match self.engine.list_filter_keys(Some("source")) {
            Ok(entries) if !entries.is_empty() => {
                Filter::Or(entries.into_iter().map(|e| Filter::Key(e.key)).collect())
            }
            _ => Filter::Or(vec![]),
        }
    }

    /// Parse an expression string into a Filter.
    /// Returns an Err string (JSON) on failure.
    fn parse_filter_or_err(expr: &str) -> Result<Filter, String> {
        parse_filter(expr).map_err(|e| {
            serde_json::json!({
                "error": "filter parse error",
                "message": e.to_string()
            })
            .to_string()
        })
    }
}

#[tool_handler(router = self.tool_router)]
impl ServerHandler for BiemMcpServer {
    fn get_info(&self) -> rmcp::model::ServerInfo {
        let mut implementation = rmcp::model::Implementation::default();
        implementation.name = "locus".into();
        implementation.version = env!("CARGO_PKG_VERSION").into();
        let mut info = rmcp::model::ServerInfo::default();
        info.server_info = implementation;
        info.capabilities = rmcp::model::ServerCapabilities::builder()
            .enable_tools()
            .build();
        info
    }
}

#[tool_router(router = tool_router)]
impl BiemMcpServer {
    /// Search the Locus index using bitmap filters. Returns structural pointers
    /// to matching documents and their chunks (not content).
    ///
    /// `filter` (preferred): expression syntax — `namespace:value` terms with AND/OR/NOT
    /// and parentheses. Example: `"tag:work AND (type:task OR type:note) AND NOT folder:archive"`.
    /// Namespaces: tag, folder, type, source, lang, kind, topic. Precedence: NOT > AND > OR.
    ///
    /// `filters` (legacy): flat key list combined with `op` (and/or). Ignored when `filter` set.
    ///
    /// If neither is provided, matches all indexed documents.
    /// Use `locus_filters` to discover available keys.
    #[tool(name = "locus_search", description = "Search the Locus index. Use filter=\"tag:work AND (type:task OR type:note)\" for expression syntax, or the legacy filters array. Supports NOT and parentheses.")]
    fn search(&self, params: Parameters<SearchParams>) -> String {
        let params = params.0;

        let filter = if let Some(ref expr) = params.filter {
            match Self::parse_filter_or_err(expr) {
                Ok(f) => f,
                Err(json_err) => return json_err,
            }
        } else if !params.filters.is_empty() {
            if params.filters.len() == 1 {
                Filter::Key(params.filters[0].clone())
            } else {
                let keys: Vec<Filter> = params
                    .filters
                    .iter()
                    .map(|k| Filter::Key(k.clone()))
                    .collect();
                match params.op.as_str() {
                    "or" => Filter::Or(keys),
                    _ => Filter::And(keys),
                }
            }
        } else {
            self.build_match_all_filter()
        };

        let result = self.engine.query(QueryRequest {
            filter,
            limit: Some(params.limit),
            offset: None,
        });

        match result {
            Ok(qr) => serde_json::to_string(&qr).unwrap_or_else(|e| format!(r#"{{"error":"{}"}}"#, e)),
            Err(e) => format!(r#"{{"error":"{}"}}"#, e),
        }
    }

    /// Semantic (vector similarity) search over the Locus index.
    ///
    /// Embeds the query with a local ONNX model (BGE-small-en-v1.5, no API key needed) and
    /// searches within the bitmap-pre-filtered candidate set. The model is loaded on first call
    /// and cached; subsequent calls are fast.
    ///
    /// `query`: required natural-language search text.
    /// `filter`: optional expression (same syntax as locus_search). Defaults to all indexed docs.
    /// `top_k`: max results (default 10).
    /// `rerank`: apply cross-encoder reranker (default false; slower but higher quality).
    ///
    /// Requires locusd started with `--semantic` to wire the embedding engine.
    #[tool(name = "locus_semantic", description = "Semantic vector search — finds documents by meaning using local ONNX embeddings. Use filter= to scope (e.g. \"source:code AND lang:rust\"). Requires locusd --semantic.")]
    fn semantic(&self, params: Parameters<SemanticParams>) -> String {
        let params = params.0;

        let Some(sem_engine) = &self.semantic_engine else {
            return r#"{"error":"semantic engine not wired — start locusd with --semantic flag"}"#
                .into();
        };

        let filter = if let Some(ref expr) = params.filter {
            match Self::parse_filter_or_err(expr) {
                Ok(f) => f,
                Err(json_err) => return json_err,
            }
        } else {
            self.build_match_all_filter()
        };

        let Some(data_dir) = &self.semantic_data_dir else {
            return r#"{"error":"semantic data_dir not configured — internal error"}"#.into();
        };

        // Lazy-init: expensive model load happens only once
        let state = self.semantic_state.get_or_init(|| {
            let embedder = FastEmbedEmbedder::new()
                .map_err(|e| format!("failed to load embedding model: {e}"))?;
            let vector_path = data_dir.join("vectors.usearch");
            let dim = embedder.dimension();
            let vector_store = UsearchVectorStore::new(&vector_path, dim)
                .map_err(|e| format!("failed to open vector store: {e}"))?;
            Ok(SemanticState { embedder, vector_store })
        });

        let state = match state {
            Ok(s) => s,
            Err(msg) => {
                return serde_json::json!({
                    "error": "semantic init failed",
                    "message": msg
                })
                .to_string();
            }
        };

        let request = SemanticQueryRequest {
            filter,
            query_text: params.query,
            top_k: params.top_k,
            rerank: params.rerank,
            graph_expand: None,
        };

        match sem_engine.semantic_query(
            &request,
            &state.embedder,
            &state.vector_store,
            None,
        ) {
            Ok(r) => serde_json::to_string(&r)
                .unwrap_or_else(|e| format!(r#"{{"error":"{}"}}"#, e)),
            Err(e) => format!(r#"{{"error":"{}"}}"#, e),
        }
    }

    /// Inspect a specific file in the Locus index. Returns doc metadata,
    /// chunks, and associated bitmap keys.
    #[tool(name = "locus_inspect", description = "Inspect a file in the Locus index")]
    fn inspect(&self, params: Parameters<InspectParams>) -> String {
        let params = params.0;
        let path = std::path::PathBuf::from(&params.file_path);

        match self.engine.inspect(&path) {
            Ok(Some(result)) => serde_json::to_string(&result).unwrap_or_else(|e| format!(r#"{{"error":"{}"}}"#, e)),
            Ok(None) => format!(r#"{{"error":"not found","path":"{}"}}"#, params.file_path),
            Err(e) => format!(r#"{{"error":"{}"}}"#, e),
        }
    }

    /// Get Locus index status: document count, bitmap count, tombstone count.
    /// Includes graph statistics when a graph engine is wired.
    #[tool(name = "locus_status", description = "Get Locus index status")]
    fn status(&self) -> String {
        match self.engine.status() {
            Ok(s) => {
                let mut val = serde_json::to_value(&s).unwrap_or_default();
                if let Some(g) = &self.graph {
                    if let Ok(gs) = g.stats() {
                        if let Ok(gv) = serde_json::to_value(&gs) {
                            val["graph"] = gv;
                        }
                    }
                }
                val.to_string()
            }
            Err(e) => format!(r#"{{"error":"{}"}}"#, e),
        }
    }

    /// List available bitmap filter keys. Use this to discover what filters
    /// can be passed to locus_search. Optionally filter by category.
    #[tool(name = "locus_filters", description = "List available bitmap filter keys")]
    fn filters(&self, params: Parameters<FiltersParams>) -> String {
        let params = params.0;
        match self.engine.list_filter_keys(params.category.as_deref()) {
            Ok(entries) => serde_json::to_string(&entries).unwrap_or_else(|e| format!(r#"{{"error":"{}"}}"#, e)),
            Err(e) => format!(r#"{{"error":"{}"}}"#, e),
        }
    }

    /// Graph traversal and centrality queries. Operations:
    /// - "neighbours": direct neighbours of doc_id (optional direction)
    /// - "expand": multi-hop expansion from doc_id (hops, direction)
    /// - "path": shortest path from doc_id to to_doc_id
    /// - "central": top-k nodes by centrality (algorithm, k)
    /// - "stats": graph statistics (node/edge counts by category)
    #[tool(name = "locus_graph", description = "Graph traversal and centrality queries")]
    fn graph_query(&self, params: Parameters<GraphParams>) -> String {
        let params = params.0;

        let Some(engine) = &self.graph else {
            return r#"{"error":"graph engine not wired — start locusd with graph support"}"#
                .into();
        };

        let direction = match params.direction.as_str() {
            "incoming" | "in" => Direction::Incoming,
            "both" => Direction::Both,
            _ => Direction::Outgoing,
        };

        let result: Result<serde_json::Value, String> = match params.operation.as_str() {
            "neighbours" => {
                let Some(doc_id) = params.doc_id else {
                    return r#"{"error":"doc_id required for neighbours"}"#.into();
                };
                engine
                    .query(GraphQueryRequest {
                        op: GraphOp::Neighbours { from: doc_id, direction },
                        edge_filter: EdgeFilter::Any,
                        limit: Some(params.k),
                    })
                    .map(|r| serde_json::to_value(&r).unwrap_or_default())
                    .map_err(|e| e.to_string())
            }
            "expand" => {
                let Some(doc_id) = params.doc_id else {
                    return r#"{"error":"doc_id required for expand"}"#.into();
                };
                let spec = ExpandSpec {
                    hops: params.hops,
                    direction,
                    edge_filter: EdgeFilter::Any,
                    max_nodes: Some(500),
                    include_seeds: false,
                };
                engine
                    .query(GraphQueryRequest {
                        op: GraphOp::Expand { seeds: vec![doc_id], spec },
                        edge_filter: EdgeFilter::Any,
                        limit: None,
                    })
                    .map(|r| serde_json::to_value(&r).unwrap_or_default())
                    .map_err(|e| e.to_string())
            }
            "path" => {
                let Some(from_id) = params.doc_id else {
                    return r#"{"error":"doc_id required for path"}"#.into();
                };
                let Some(to_id) = params.to_doc_id else {
                    return r#"{"error":"to_doc_id required for path"}"#.into();
                };
                engine
                    .shortest_path(from_id, to_id, &EdgeFilter::Any)
                    .map(|ids| serde_json::json!({ "path": ids }))
                    .map_err(|e| e.to_string())
            }
            "central" => {
                let algo = match params.algorithm.as_str() {
                    "indegree" | "in" => CentralityAlgorithm::InDegree,
                    "outdegree" | "out" => CentralityAlgorithm::OutDegree,
                    _ => CentralityAlgorithm::PageRank { iterations: 100, damping: 0.85 },
                };
                engine
                    .centrality(algo, None)
                    .map(|scores| {
                        let top: Vec<_> = scores
                            .into_iter()
                            .take(params.k as usize)
                            .map(|(id, score)| serde_json::json!({ "doc_id": id, "score": score }))
                            .collect();
                        serde_json::json!({ "nodes": top })
                    })
                    .map_err(|e| e.to_string())
            }
            "stats" => engine
                .stats()
                .map(|s| serde_json::to_value(&s).unwrap_or_default())
                .map_err(|e| e.to_string()),
            other => {
                return format!(
                    r#"{{"error":"unknown operation '{other}'. Valid: neighbours, expand, path, central, stats"}}"#
                )
            }
        };

        match result {
            Ok(v) => v.to_string(),
            Err(e) => format!(r#"{{"error":"{}"}}"#, e),
        }
    }

    /// Ingest a remote document directly from an LLM session.
    /// Accepts a synthetic path and base64-encoded content bytes.
    /// The pipeline selects the correct parser based on the path extension
    /// (.confluence, .jira, .slack) and performs an upsert (insert / update / skip).
    /// Requires locusd to be started with the --webhook flag to wire the pipeline.
    #[tool(name = "locus_remote_ingest", description = "Ingest a remote document (Confluence page, Jira issue, Slack bundle) directly into the index")]
    fn remote_ingest(&self, params: Parameters<RemoteIngestParams>) -> String {
        let params = params.0;

        let Some(pipeline) = &self.ingest else {
            return r#"{"error":"ingestion pipeline not wired — start locusd with --webhook"}"#.into();
        };

        let content = match base64_decode(&params.content_b64) {
            Ok(b) => b,
            Err(e) => return format!(r#"{{"error":"invalid base64: {e}"}}"#),
        };

        let path = std::path::PathBuf::from(&params.path);

        let result = pipeline
            .lock()
            .map_err(|_| "pipeline mutex poisoned".to_string())
            .and_then(|mut guard| guard.upsert_document(path, content).map_err(|e| e.to_string()));

        match result {
            Ok(r) => {
                let action = match r.action {
                    locus_ingest::IngestAction::Indexed => "indexed",
                    locus_ingest::IngestAction::Updated => "updated",
                    locus_ingest::IngestAction::Skipped => "skipped",
                    locus_ingest::IngestAction::Tombstoned => "tombstoned",
                    locus_ingest::IngestAction::Moved => "moved",
                };
                serde_json::json!({
                    "path": params.path,
                    "action": action,
                    "bitmaps_updated": r.bitmaps_updated,
                })
                .to_string()
            }
            Err(e) => format!(r#"{{"error":"{}"}}"#, e),
        }
    }
}

fn base64_decode(s: &str) -> Result<Vec<u8>, String> {
    const TABLE: [u8; 128] = {
        let mut t = [255u8; 128];
        let mut i = 0u8;
        while i < 26 { t[(b'A' + i) as usize] = i; i += 1; }
        i = 0;
        while i < 26 { t[(b'a' + i) as usize] = 26 + i; i += 1; }
        i = 0;
        while i < 10 { t[(b'0' + i) as usize] = 52 + i; i += 1; }
        t[b'+' as usize] = 62;
        t[b'/' as usize] = 63;
        t[b'=' as usize] = 0;
        t
    };
    let s = s.trim();
    let bytes = s.as_bytes();
    if bytes.len() % 4 != 0 {
        return Err(format!("length {} not a multiple of 4", bytes.len()));
    }
    let mut out = Vec::with_capacity((bytes.len() / 4) * 3);
    let mut i = 0;
    while i < bytes.len() {
        let [b0, b1, b2, b3] = [bytes[i], bytes[i+1], bytes[i+2], bytes[i+3]];
        for &b in &[b0, b1, b2, b3] {
            if b >= 128 || (TABLE[b as usize] == 255 && b != b'=') {
                return Err(format!("invalid character: {:?}", b as char));
            }
        }
        let n = ((TABLE[b0 as usize] as u32) << 18)
            | ((TABLE[b1 as usize] as u32) << 12)
            | ((TABLE[b2 as usize] as u32) << 6)
            | (TABLE[b3 as usize] as u32);
        out.push((n >> 16) as u8);
        if b2 != b'=' { out.push((n >> 8) as u8); }
        if b3 != b'=' { out.push(n as u8); }
        i += 4;
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::sync::Arc;

    use locus_bitmap::memory::InMemoryBitmapStore;
    use locus_core::bitmap::BitmapStore;
    use locus_core::registry::{NewDoc, Registry};
    use locus_core::types::{DocType, SourceType};
    use locus_query::BitmapQueryEngine;
    use locus_registry::memory::InMemoryRegistry;

    fn make_server() -> BiemMcpServer {
        let mut bitmap_store = InMemoryBitmapStore::new();
        let mut registry = InMemoryRegistry::new();

        let d1 = registry
            .insert_doc(NewDoc {
                file_path: PathBuf::from("/vault/work/task1.md"),
                source_type: SourceType::Obsidian,
                blake3_hash: [1; 32],
                auto_type: Some(DocType::Task),
            })
            .unwrap();
        let d2 = registry
            .insert_doc(NewDoc {
                file_path: PathBuf::from("/vault/work/note1.md"),
                source_type: SourceType::Obsidian,
                blake3_hash: [2; 32],
                auto_type: None,
            })
            .unwrap();

        bitmap_store.insert_id("tag:work", d1).unwrap();
        bitmap_store.insert_id("tag:work", d2).unwrap();
        bitmap_store.insert_id("type:task", d1).unwrap();
        bitmap_store.insert_id("source:obsidian", d1).unwrap();
        bitmap_store.insert_id("source:obsidian", d2).unwrap();

        let engine: Arc<dyn QueryEngine> = Arc::new(
            BitmapQueryEngine::new(Box::new(bitmap_store), Box::new(registry)),
        );
        BiemMcpServer::new(engine)
    }

    #[test]
    fn test_search_returns_valid_json() {
        let server = make_server();
        let result = server.search(Parameters(SearchParams {
            filter: None,
            filters: vec!["tag:work".into()],
            op: "and".into(),
            limit: 20,
        }));
        let json: serde_json::Value = serde_json::from_str(&result).unwrap();
        assert_eq!(json["total_matching"], 2);
        assert!(json["matches"].as_array().unwrap().len() == 2);
    }

    #[test]
    fn test_search_and_filter() {
        let server = make_server();
        let result = server.search(Parameters(SearchParams {
            filter: None,
            filters: vec!["tag:work".into(), "type:task".into()],
            op: "and".into(),
            limit: 20,
        }));
        let json: serde_json::Value = serde_json::from_str(&result).unwrap();
        assert_eq!(json["total_matching"], 1);
    }

    #[test]
    fn test_search_or_filter() {
        let server = make_server();
        let result = server.search(Parameters(SearchParams {
            filter: None,
            filters: vec!["tag:work".into(), "type:task".into()],
            op: "or".into(),
            limit: 20,
        }));
        let json: serde_json::Value = serde_json::from_str(&result).unwrap();
        assert_eq!(json["total_matching"], 2);
    }

    #[test]
    fn test_search_expression_and() {
        let server = make_server();
        let result = server.search(Parameters(SearchParams {
            filter: Some("tag:work AND type:task".into()),
            filters: vec![],
            op: "and".into(),
            limit: 20,
        }));
        let json: serde_json::Value = serde_json::from_str(&result).unwrap();
        assert_eq!(json["total_matching"], 1);
    }

    #[test]
    fn test_search_expression_or() {
        let server = make_server();
        let result = server.search(Parameters(SearchParams {
            filter: Some("tag:work OR type:task".into()),
            filters: vec![],
            op: "and".into(),
            limit: 20,
        }));
        let json: serde_json::Value = serde_json::from_str(&result).unwrap();
        assert_eq!(json["total_matching"], 2);
    }

    #[test]
    fn test_search_expression_not() {
        let server = make_server();
        // tag:work (2 docs) AND NOT type:task (removes d1) → 1 doc (d2 = note1.md)
        let result = server.search(Parameters(SearchParams {
            filter: Some("tag:work AND NOT type:task".into()),
            filters: vec![],
            op: "and".into(),
            limit: 20,
        }));
        let json: serde_json::Value = serde_json::from_str(&result).unwrap();
        assert_eq!(json["total_matching"], 1);
        let matches = json["matches"].as_array().unwrap();
        assert!(matches[0]["file_path"].as_str().unwrap().contains("note1"));
    }

    #[test]
    fn test_search_expression_invalid_returns_error() {
        let server = make_server();
        let result = server.search(Parameters(SearchParams {
            filter: Some("barekeynocodon".into()),
            filters: vec![],
            op: "and".into(),
            limit: 20,
        }));
        let json: serde_json::Value = serde_json::from_str(&result).unwrap();
        assert_eq!(json["error"], "filter parse error");
        assert!(json["message"].is_string());
    }

    #[test]
    fn test_search_empty_defaults_to_match_all() {
        let server = make_server();
        let result = server.search(Parameters(SearchParams {
            filter: None,
            filters: vec![],
            op: "and".into(),
            limit: 20,
        }));
        let json: serde_json::Value = serde_json::from_str(&result).unwrap();
        // source:obsidian covers both docs
        assert_eq!(json["total_matching"], 2);
    }

    #[test]
    fn test_semantic_not_wired_returns_error() {
        let server = make_server(); // no .with_semantic()
        let result = server.semantic(Parameters(SemanticParams {
            query: "test query".into(),
            filter: None,
            top_k: 10,
            rerank: false,
        }));
        let json: serde_json::Value = serde_json::from_str(&result).unwrap();
        assert!(json["error"].is_string());
        assert!(json["error"].as_str().unwrap().contains("not wired"));
    }

    #[test]
    fn test_semantic_invalid_filter_returns_error() {
        // We can test filter parsing without needing a real semantic engine:
        // create a server with a minimal semantic_engine wired but no real data dir
        // — the filter parse error is returned before the engine is consulted.
        let mut bitmap_store = InMemoryBitmapStore::new();
        let mut registry = InMemoryRegistry::new();
        let _ = registry.insert_doc(locus_core::registry::NewDoc {
            file_path: PathBuf::from("/vault/a.md"),
            source_type: SourceType::Obsidian,
            blake3_hash: [1; 32],
            auto_type: None,
        }).unwrap();
        bitmap_store.insert_id("source:obsidian", 1).unwrap();

        let bqe = Arc::new(BitmapQueryEngine::new(Box::new(bitmap_store), Box::new(registry)));
        let engine: Arc<dyn QueryEngine> = bqe.clone();
        let server = BiemMcpServer::new(engine)
            .with_semantic(bqe, PathBuf::from("/nonexistent/data"));

        let result = server.semantic(Parameters(SemanticParams {
            query: "test".into(),
            filter: Some("bare_term_no_colon".into()),
            top_k: 5,
            rerank: false,
        }));
        let json: serde_json::Value = serde_json::from_str(&result).unwrap();
        assert_eq!(json["error"], "filter parse error");
    }

    #[test]
    fn test_inspect_found() {
        let server = make_server();
        let result = server.inspect(Parameters(InspectParams {
            file_path: "/vault/work/task1.md".into(),
        }));
        let json: serde_json::Value = serde_json::from_str(&result).unwrap();
        assert_eq!(json["source_type"], "Obsidian");
        assert!(json["bitmap_keys"].as_array().unwrap().len() > 0);
    }

    #[test]
    fn test_inspect_not_found() {
        let server = make_server();
        let result = server.inspect(Parameters(InspectParams {
            file_path: "/vault/nonexistent.md".into(),
        }));
        let json: serde_json::Value = serde_json::from_str(&result).unwrap();
        assert_eq!(json["error"], "not found");
    }

    #[test]
    fn test_status_returns_valid_json() {
        let server = make_server();
        let result = server.status();
        let json: serde_json::Value = serde_json::from_str(&result).unwrap();
        assert_eq!(json["total_documents"], 2);
        assert_eq!(json["total_bitmaps"], 3); // tag:work, type:task, source:obsidian
        assert_eq!(json["tombstoned"], 0);
    }

    #[test]
    fn test_filters_all() {
        let server = make_server();
        let result = server.filters(Parameters(FiltersParams { category: None }));
        let json: serde_json::Value = serde_json::from_str(&result).unwrap();
        let entries = json.as_array().unwrap();
        assert_eq!(entries.len(), 3);
        for entry in entries {
            assert!(entry["key"].is_string());
            assert!(entry["cardinality"].is_number());
        }
    }

    #[test]
    fn test_filters_by_category() {
        let server = make_server();
        let result = server.filters(Parameters(FiltersParams {
            category: Some("tag".into()),
        }));
        let json: serde_json::Value = serde_json::from_str(&result).unwrap();
        let entries = json.as_array().unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0]["key"], "tag:work");
        assert_eq!(entries[0]["cardinality"], 2);
    }

    #[test]
    fn test_graph_tool_returns_error_when_not_wired() {
        let server = make_server(); // no .with_graph()
        let result = server.graph_query(Parameters(GraphParams {
            operation: "stats".into(),
            doc_id: None,
            to_doc_id: None,
            hops: 1,
            direction: "outgoing".into(),
            algorithm: "pagerank".into(),
            k: 10,
        }));
        let json: serde_json::Value = serde_json::from_str(&result).unwrap();
        assert!(json["error"].is_string(), "expected error when graph not wired");
    }

    #[test]
    fn test_graph_tool_any_op_without_engine_returns_error() {
        let server = make_server();
        for op in &["bogus", "neighbours", "stats"] {
            let result = server.graph_query(Parameters(GraphParams {
                operation: op.to_string(),
                doc_id: None,
                to_doc_id: None,
                hops: 1,
                direction: "outgoing".into(),
                algorithm: "pagerank".into(),
                k: 10,
            }));
            let json: serde_json::Value = serde_json::from_str(&result).unwrap();
            assert!(
                json["error"].is_string(),
                "expected error JSON for op '{op}' when graph not wired"
            );
        }
    }

    #[test]
    fn test_status_includes_no_graph_key_when_not_wired() {
        let server = make_server();
        let result = server.status();
        let json: serde_json::Value = serde_json::from_str(&result).unwrap();
        assert_eq!(json["total_documents"], 2);
        assert!(json.get("graph").is_none(), "graph key should be absent when not wired");
    }
}
