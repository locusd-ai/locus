//! HTTP API server for BIEM.
//!
//! Exposes 5 endpoints:
//!   POST /v1/search     — bitmap-filtered document search
//!   POST /v1/semantic   — semantic (vector similarity) search
//!   GET  /v1/inspect    — inspect a single indexed file
//!   GET  /v1/status     — index health summary
//!   GET  /v1/filters    — list available bitmap filter keys
//!
//! Design note: `semantic_query` is an inherent method on `BitmapQueryEngine`, not on the
//! `QueryEngine` trait. Rather than polluting the trait (which is a shared contract) or
//! duplicating state, the semantic endpoint gets its own `SemanticState` that holds an
//! `Arc<BitmapQueryEngine>` directly. The bitmap `AppState` (Arc<dyn QueryEngine>) is
//! unchanged; the semantic state is merged into the router only when wired.

use std::path::PathBuf;
use std::sync::{Arc, OnceLock};

use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Json, Response};
use axum::routing::{get, post};
use axum::Router;
use serde::Deserialize;

use locus_core::query::{Filter, QueryEngine, QueryError, QueryRequest};
use locus_core::semantic::{Embedder, SemanticQueryRequest};
use locus_embed::{FastEmbedEmbedder, UsearchVectorStore};
use locus_query::{parse_filter, BitmapQueryEngine};

// ── Shared state ─────────────────────────────────────────────────

pub type AppState = Arc<dyn QueryEngine>;

/// State for the semantic endpoint. Held separately because `semantic_query`
/// is not on the `QueryEngine` trait.
pub struct SemanticHttpState {
    pub engine: Arc<BitmapQueryEngine>,
    pub data_dir: PathBuf,
    /// Lazy-init: OnceLock so the model is loaded only on first request.
    pub state: OnceLock<Result<SemanticInner, String>>,
}

pub struct SemanticInner {
    pub embedder: FastEmbedEmbedder,
    pub vector_store: UsearchVectorStore,
}

impl SemanticHttpState {
    pub fn new(engine: Arc<BitmapQueryEngine>, data_dir: PathBuf) -> Arc<Self> {
        Arc::new(Self {
            engine,
            data_dir,
            state: OnceLock::new(),
        })
    }

    fn get_or_init(&self) -> &Result<SemanticInner, String> {
        self.state.get_or_init(|| {
            let embedder = FastEmbedEmbedder::new()
                .map_err(|e| format!("failed to load embedding model: {e}"))?;
            let vector_path = self.data_dir.join("vectors.usearch");
            let dim = embedder.dimension();
            let vector_store = UsearchVectorStore::new(&vector_path, dim)
                .map_err(|e| format!("failed to open vector store: {e}"))?;
            Ok(SemanticInner { embedder, vector_store })
        })
    }
}

// ── Error handling ───────────────────────────────────────────────

struct AppError(QueryError);

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        let body = serde_json::json!({ "error": self.0.to_string() });
        (StatusCode::INTERNAL_SERVER_ERROR, Json(body)).into_response()
    }
}

impl From<QueryError> for AppError {
    fn from(e: QueryError) -> Self {
        Self(e)
    }
}

// ── Router ───────────────────────────────────────────────────────

/// Build the standard router (bitmap search + inspect + status + filters).
pub fn router(engine: Arc<dyn QueryEngine>) -> Router {
    Router::new()
        .route("/v1/search", post(search))
        .route("/v1/inspect", get(inspect))
        .route("/v1/status", get(status))
        .route("/v1/filters", get(filters))
        .with_state(engine)
}

/// Build a router that also includes POST /v1/semantic.
pub fn router_with_semantic(
    engine: Arc<dyn QueryEngine>,
    semantic: Arc<SemanticHttpState>,
) -> Router {
    router(engine).merge(semantic_router(semantic))
}

/// Build only the semantic sub-router (can be merged into any router).
pub fn semantic_router(semantic: Arc<SemanticHttpState>) -> Router {
    Router::new()
        .route("/v1/semantic", post(semantic_search))
        .with_state(semantic)
}

// ── Request types ────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
struct SearchBody {
    /// Filter expression string (preferred).
    /// Syntax: `namespace:value` terms with AND/OR/NOT and parentheses.
    /// Example: `"tag:work AND (type:task OR type:note) AND NOT folder:archive"`
    #[serde(default)]
    filter: Option<String>,
    /// Legacy: bitmap filter keys (e.g. ["tag:work", "type:task"])
    #[serde(default)]
    filters: Vec<String>,
    /// Boolean operation for legacy filters: "and" (default) or "or"
    #[serde(default = "default_op")]
    op: String,
    /// Maximum results to return
    #[serde(default = "default_limit")]
    limit: u32,
    /// Offset for pagination
    #[serde(default)]
    offset: Option<u32>,
}

fn default_op() -> String {
    "and".into()
}

fn default_limit() -> u32 {
    20
}

#[derive(Debug, Deserialize)]
struct SemanticBody {
    /// Natural language query text.
    query: String,
    /// Optional filter expression. Same syntax as SearchBody.filter.
    #[serde(default)]
    filter: Option<String>,
    /// Maximum results to return (default 10).
    #[serde(default = "default_top_k")]
    top_k: usize,
    /// Apply cross-encoder reranker (default false).
    #[serde(default)]
    rerank: bool,
}

fn default_top_k() -> usize {
    10
}

#[derive(Debug, Deserialize)]
struct InspectQuery {
    /// File path to inspect
    path: String,
}

#[derive(Debug, Deserialize)]
struct FiltersQuery {
    /// Optional category filter: "tag", "folder", "link", "type", "source"
    category: Option<String>,
}

// ── Helpers ───────────────────────────────────────────────────────

/// Build a match-all filter from the engine's source keys.
fn build_match_all(engine: &Arc<dyn QueryEngine>) -> Filter {
    match engine.list_filter_keys(Some("source")) {
        Ok(entries) if !entries.is_empty() => {
            Filter::Or(entries.into_iter().map(|e| Filter::Key(e.key)).collect())
        }
        _ => Filter::Or(vec![]),
    }
}

// ── Handlers ─────────────────────────────────────────────────────

async fn search(
    State(engine): State<AppState>,
    Json(body): Json<SearchBody>,
) -> Result<impl IntoResponse, AppError> {
    let result = tokio::task::spawn_blocking(move || {
        let filter = if let Some(ref expr) = body.filter {
            match parse_filter(expr) {
                Ok(f) => f,
                Err(e) => {
                    return Err(QueryError::Semantic(format!("filter parse error: {e}")));
                }
            }
        } else if !body.filters.is_empty() {
            if body.filters.len() == 1 {
                Filter::Key(body.filters[0].clone())
            } else {
                let keys: Vec<Filter> = body.filters.iter().map(|k| Filter::Key(k.clone())).collect();
                match body.op.as_str() {
                    "or" => Filter::Or(keys),
                    _ => Filter::And(keys),
                }
            }
        } else {
            build_match_all(&engine)
        };

        engine.query(QueryRequest {
            filter,
            limit: Some(body.limit),
            offset: body.offset,
        })
    })
    .await
    .expect("spawn_blocking panicked")?;

    Ok(Json(serde_json::to_value(result).unwrap()))
}

async fn semantic_search(
    State(sem): State<Arc<SemanticHttpState>>,
    Json(body): Json<SemanticBody>,
) -> Response {
    // Resolve filter before spawn_blocking (parse_filter is cheap)
    let filter = if let Some(ref expr) = body.filter {
        match parse_filter(expr) {
            Ok(f) => f,
            Err(e) => {
                let body = serde_json::json!({
                    "error": "filter parse error",
                    "message": e.to_string()
                });
                return (StatusCode::BAD_REQUEST, Json(body)).into_response();
            }
        }
    } else {
        // match-all: enumerate source keys via the engine
        match sem.engine.list_filter_keys(Some("source")) {
            Ok(entries) if !entries.is_empty() => {
                Filter::Or(entries.into_iter().map(|e| Filter::Key(e.key)).collect())
            }
            _ => Filter::Or(vec![]),
        }
    };

    let result = tokio::task::spawn_blocking(move || {
        // Lazy init happens here (inside spawn_blocking so it doesn't block the async executor)
        let state = sem.get_or_init();
        match state {
            Err(msg) => {
                return Err(format!("semantic init failed: {msg}"));
            }
            Ok(inner) => {
                let request = SemanticQueryRequest {
                    filter,
                    query_text: body.query,
                    top_k: body.top_k,
                    rerank: body.rerank,
                    graph_expand: None,
                };
                sem.engine
                    .semantic_query(&request, &inner.embedder, &inner.vector_store, None)
                    .map_err(|e| e.to_string())
            }
        }
    })
    .await
    .expect("spawn_blocking panicked");

    match result {
        Ok(r) => Json(serde_json::to_value(r).unwrap()).into_response(),
        Err(msg) => {
            let body = serde_json::json!({ "error": msg });
            (StatusCode::INTERNAL_SERVER_ERROR, Json(body)).into_response()
        }
    }
}

async fn inspect(
    State(engine): State<AppState>,
    Query(params): Query<InspectQuery>,
) -> Result<Response, AppError> {
    let engine = engine.clone();
    let result = tokio::task::spawn_blocking(move || {
        let path = std::path::PathBuf::from(&params.path);
        engine.inspect(&path)
    })
    .await
    .expect("spawn_blocking panicked")?;

    match result {
        Some(r) => Ok(Json(serde_json::to_value(r).unwrap()).into_response()),
        None => Ok((
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({ "error": "not found" })),
        )
            .into_response()),
    }
}

async fn status(State(engine): State<AppState>) -> Result<impl IntoResponse, AppError> {
    let engine = engine.clone();
    let result = tokio::task::spawn_blocking(move || engine.status())
        .await
        .expect("spawn_blocking panicked")?;

    Ok(Json(serde_json::to_value(result).unwrap()))
}

async fn filters(
    State(engine): State<AppState>,
    Query(params): Query<FiltersQuery>,
) -> Result<impl IntoResponse, AppError> {
    let engine = engine.clone();
    let result = tokio::task::spawn_blocking(move || {
        engine.list_filter_keys(params.category.as_deref())
    })
    .await
    .expect("spawn_blocking panicked")?;

    Ok(Json(serde_json::to_value(result).unwrap()))
}

// ── Tests ────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    use axum::body::Body;
    use axum::http::{Method, Request};
    use tower::ServiceExt;

    use locus_bitmap::memory::InMemoryBitmapStore;
    use locus_core::bitmap::BitmapStore;
    use locus_core::registry::{NewDoc, Registry};
    use locus_core::types::{DocType, SourceType};
    use locus_query::BitmapQueryEngine;
    use locus_registry::memory::InMemoryRegistry;

    fn make_engine() -> Arc<dyn QueryEngine> {
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

        Arc::new(BitmapQueryEngine::new(
            Box::new(bitmap_store),
            Box::new(registry),
        ))
    }

    async fn body_json(response: axum::http::Response<Body>) -> serde_json::Value {
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        serde_json::from_slice(&bytes).unwrap()
    }

    #[tokio::test]
    async fn test_search_post() {
        let app = router(make_engine());
        let req = Request::builder()
            .method(Method::POST)
            .uri("/v1/search")
            .header("content-type", "application/json")
            .body(Body::from(r#"{"filters":["tag:work"]}"#))
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let json = body_json(resp).await;
        assert_eq!(json["total_matching"], 2);
    }

    #[tokio::test]
    async fn test_search_expression_filter() {
        let app = router(make_engine());
        let req = Request::builder()
            .method(Method::POST)
            .uri("/v1/search")
            .header("content-type", "application/json")
            .body(Body::from(r#"{"filter":"tag:work AND type:task"}"#))
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let json = body_json(resp).await;
        assert_eq!(json["total_matching"], 1);
    }

    #[tokio::test]
    async fn test_search_expression_not_filter() {
        let app = router(make_engine());
        let req = Request::builder()
            .method(Method::POST)
            .uri("/v1/search")
            .header("content-type", "application/json")
            .body(Body::from(r#"{"filter":"tag:work AND NOT type:task"}"#))
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let json = body_json(resp).await;
        assert_eq!(json["total_matching"], 1);
    }

    #[tokio::test]
    async fn test_search_invalid_filter_expression() {
        let app = router(make_engine());
        let req = Request::builder()
            .method(Method::POST)
            .uri("/v1/search")
            .header("content-type", "application/json")
            .body(Body::from(r#"{"filter":"bareterm"}"#))
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        // Filter parse error becomes a QueryError::Semantic → 500
        assert_eq!(resp.status(), StatusCode::INTERNAL_SERVER_ERROR);
        let json = body_json(resp).await;
        assert!(json["error"].is_string());
    }

    #[tokio::test]
    async fn test_search_and_filter() {
        let app = router(make_engine());
        let req = Request::builder()
            .method(Method::POST)
            .uri("/v1/search")
            .header("content-type", "application/json")
            .body(Body::from(
                r#"{"filters":["tag:work","type:task"],"op":"and"}"#,
            ))
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        let json = body_json(resp).await;
        assert_eq!(json["total_matching"], 1);
    }

    #[tokio::test]
    async fn test_search_empty_defaults_to_match_all() {
        let app = router(make_engine());
        let req = Request::builder()
            .method(Method::POST)
            .uri("/v1/search")
            .header("content-type", "application/json")
            .body(Body::from(r#"{}"#))
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let json = body_json(resp).await;
        assert_eq!(json["total_matching"], 2);
    }

    #[tokio::test]
    async fn test_semantic_endpoint_with_in_memory_store() {
        use locus_core::registry::{NewChunk, Registry};
        use locus_core::semantic::{EmbedError, Embedder, EmbeddingVector, VectorStore};
        use locus_core::types::{ChunkKind, ChunkMetadata};
        use locus_embed::InMemoryVectorStore;

        // Build a small in-memory index with vectors
        let mut bitmap_store = InMemoryBitmapStore::new();
        let mut registry = InMemoryRegistry::new();

        let d1 = registry
            .insert_doc(NewDoc {
                file_path: PathBuf::from("/vault/alpha.md"),
                source_type: SourceType::Obsidian,
                blake3_hash: [10; 32],
                auto_type: None,
            })
            .unwrap();

        bitmap_store.insert_id("source:obsidian", d1).unwrap();

        let chunks = registry
            .replace_chunks(
                d1,
                vec![NewChunk {
                    doc_id: d1,
                    kind: ChunkKind::Section,
                    byte_start: 0,
                    byte_end: 50,
                    label: Some("Alpha section".into()),
                    depth: 1,
                    metadata: ChunkMetadata::default(),
                }],
            )
            .unwrap();
        let c1 = chunks[0];

        // Use a stub embedder for the vector store
        struct ConstEmbedder;
        impl Embedder for ConstEmbedder {
            fn embed(&self, texts: &[&str]) -> Result<Vec<EmbeddingVector>, EmbedError> {
                Ok(texts.iter().map(|_| vec![1.0_f32, 0.0, 0.0]).collect())
            }
            fn dimension(&self) -> usize { 3 }
        }

        let vector_store = InMemoryVectorStore::new();
        vector_store.upsert(c1, &vec![1.0_f32, 0.0, 0.0]).unwrap();

        let bqe = Arc::new(BitmapQueryEngine::new(
            Box::new(bitmap_store),
            Box::new(registry),
        ));

        // Run semantic_query directly (no HTTP layer) to verify the plumbing works
        let request = SemanticQueryRequest {
            filter: Filter::Key("source:obsidian".into()),
            query_text: "alpha".into(),
            top_k: 5,
            rerank: false,
            graph_expand: None,
        };
        let embedder = ConstEmbedder;
        let result = bqe
            .semantic_query(&request, &embedder, &vector_store, None)
            .unwrap();

        assert_eq!(result.bitmap_candidates, 1);
        assert_eq!(result.vector_searched, 1);
        assert_eq!(result.pointers.len(), 1);
        assert!((result.pointers[0].score - 1.0).abs() < 0.001);
    }

    #[tokio::test]
    async fn test_inspect_found() {
        let app = router(make_engine());
        let req = Request::builder()
            .method(Method::GET)
            .uri("/v1/inspect?path=/vault/work/task1.md")
            .body(Body::empty())
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let json = body_json(resp).await;
        assert_eq!(json["source_type"], "Obsidian");
    }

    #[tokio::test]
    async fn test_inspect_not_found() {
        let app = router(make_engine());
        let req = Request::builder()
            .method(Method::GET)
            .uri("/v1/inspect?path=/vault/nope.md")
            .body(Body::empty())
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn test_status() {
        let app = router(make_engine());
        let req = Request::builder()
            .method(Method::GET)
            .uri("/v1/status")
            .body(Body::empty())
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let json = body_json(resp).await;
        assert_eq!(json["total_documents"], 2);
        assert_eq!(json["total_bitmaps"], 3);
    }

    #[tokio::test]
    async fn test_filters_all() {
        let app = router(make_engine());
        let req = Request::builder()
            .method(Method::GET)
            .uri("/v1/filters")
            .body(Body::empty())
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let json = body_json(resp).await;
        let entries = json.as_array().unwrap();
        assert_eq!(entries.len(), 3);
    }

    #[tokio::test]
    async fn test_filters_by_category() {
        let app = router(make_engine());
        let req = Request::builder()
            .method(Method::GET)
            .uri("/v1/filters?category=tag")
            .body(Body::empty())
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        let json = body_json(resp).await;
        let entries = json.as_array().unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0]["key"], "tag:work");
    }
}
