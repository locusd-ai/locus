//! MCP (Model Context Protocol) server for BIEM.
//!
//! Exposes 4 tools: biem_search, biem_inspect, biem_status, biem_filters.
//! Runs over stdio transport for integration with LLM clients.

use std::sync::Arc;

use rmcp::{
    ServerHandler,
    handler::server::{router::tool::ToolRouter, wrapper::Parameters},
    tool, tool_handler, tool_router,
};
use schemars::JsonSchema;
use serde::Deserialize;

use biem_core::query::{Filter, QueryEngine, QueryRequest};

// ── Tool parameter types ─────────────────────────────────────────

#[derive(Debug, Deserialize, JsonSchema)]
pub struct SearchParams {
    /// Bitmap filter keys to AND together (e.g. ["tag:work", "type:task"])
    pub filters: Vec<String>,
    /// Boolean operation: "and" (default) or "or"
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
pub struct InspectParams {
    /// Absolute or relative file path to inspect
    pub file_path: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct FiltersParams {
    /// Optional category filter: "tag", "folder", "link", "type", "source"
    pub category: Option<String>,
}

// ── MCP Server ───────────────────────────────────────────────────

/// The BIEM MCP server handler.
///
/// Delegates all read/query operations to a `dyn QueryEngine`.
pub struct BiemMcpServer {
    engine: Arc<dyn QueryEngine>,
    tool_router: ToolRouter<Self>,
}

impl BiemMcpServer {
    pub fn new(engine: Arc<dyn QueryEngine>) -> Self {
        let tool_router = Self::tool_router();
        Self { engine, tool_router }
    }
}

#[tool_handler(router = self.tool_router)]
impl ServerHandler for BiemMcpServer {}

#[tool_router(router = tool_router)]
impl BiemMcpServer {
    /// Search the BIEM index using bitmap filters. Returns structural pointers
    /// to matching documents and their chunks (not content).
    /// Use biem_filters first to discover available filter keys.
    #[tool(name = "biem_search", description = "Search the BIEM index using bitmap filters")]
    fn search(&self, params: Parameters<SearchParams>) -> String {
        let params = params.0;
        let filter = if params.filters.is_empty() {
            Filter::Key("source:obsidian".into())
        } else if params.filters.len() == 1 {
            Filter::Key(params.filters[0].clone())
        } else {
            let keys: Vec<Filter> = params.filters.iter().map(|k| Filter::Key(k.clone())).collect();
            match params.op.as_str() {
                "or" => Filter::Or(keys),
                _ => Filter::And(keys),
            }
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

    /// Inspect a specific file in the BIEM index. Returns doc metadata,
    /// chunks, and associated bitmap keys.
    #[tool(name = "biem_inspect", description = "Inspect a file in the BIEM index")]
    fn inspect(&self, params: Parameters<InspectParams>) -> String {
        let params = params.0;
        let path = std::path::PathBuf::from(&params.file_path);

        match self.engine.inspect(&path) {
            Ok(Some(result)) => serde_json::to_string(&result).unwrap_or_else(|e| format!(r#"{{"error":"{}"}}"#, e)),
            Ok(None) => format!(r#"{{"error":"not found","path":"{}"}}"#, params.file_path),
            Err(e) => format!(r#"{{"error":"{}"}}"#, e),
        }
    }

    /// Get BIEM index status: document count, bitmap count, tombstone count.
    #[tool(name = "biem_status", description = "Get BIEM index status")]
    fn status(&self) -> String {
        match self.engine.status() {
            Ok(s) => serde_json::to_string(&s).unwrap_or_else(|e| format!(r#"{{"error":"{}"}}"#, e)),
            Err(e) => format!(r#"{{"error":"{}"}}"#, e),
        }
    }

    /// List available bitmap filter keys. Use this to discover what filters
    /// can be passed to biem_search. Optionally filter by category.
    #[tool(name = "biem_filters", description = "List available bitmap filter keys")]
    fn filters(&self, params: Parameters<FiltersParams>) -> String {
        let params = params.0;
        match self.engine.list_filter_keys(params.category.as_deref()) {
            Ok(entries) => serde_json::to_string(&entries).unwrap_or_else(|e| format!(r#"{{"error":"{}"}}"#, e)),
            Err(e) => format!(r#"{{"error":"{}"}}"#, e),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::sync::Arc;

    use biem_bitmap::memory::InMemoryBitmapStore;
    use biem_core::bitmap::BitmapStore;
    use biem_core::registry::{NewDoc, Registry};
    use biem_core::types::{NoteType, SourceType};
    use biem_query::BitmapQueryEngine;
    use biem_registry::memory::InMemoryRegistry;

    fn make_server() -> BiemMcpServer {
        let mut bitmap_store = InMemoryBitmapStore::new();
        let mut registry = InMemoryRegistry::new();

        let d1 = registry
            .insert_doc(NewDoc {
                file_path: PathBuf::from("/vault/work/task1.md"),
                source_type: SourceType::Obsidian,
                blake3_hash: [1; 32],
                auto_type: Some(NoteType::Task),
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
            filters: vec!["tag:work".into(), "type:task".into()],
            op: "or".into(),
            limit: 20,
        }));
        let json: serde_json::Value = serde_json::from_str(&result).unwrap();
        assert_eq!(json["total_matching"], 2);
    }

    #[test]
    fn test_search_empty_filters_defaults_to_source_obsidian() {
        let server = make_server();
        let result = server.search(Parameters(SearchParams {
            filters: vec![],
            op: "and".into(),
            limit: 20,
        }));
        let json: serde_json::Value = serde_json::from_str(&result).unwrap();
        assert_eq!(json["total_matching"], 2);
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
        // Each entry should have key and cardinality
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
}
