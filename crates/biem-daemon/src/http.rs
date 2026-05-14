//! HTTP API server for BIEM.
//!
//! Exposes 4 endpoints:
//!   POST /v1/search     — bitmap-filtered document search
//!   GET  /v1/inspect    — inspect a single indexed file
//!   GET  /v1/status     — index health summary
//!   GET  /v1/filters    — list available bitmap filter keys

use std::sync::Arc;

use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Json, Response};
use axum::routing::{get, post};
use axum::Router;
use serde::Deserialize;

use biem_core::query::{Filter, QueryEngine, QueryError, QueryRequest};

// ── Shared state ─────────────────────────────────────────────────

pub type AppState = Arc<dyn QueryEngine>;

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

pub fn router(engine: Arc<dyn QueryEngine>) -> Router {
    Router::new()
        .route("/v1/search", post(search))
        .route("/v1/inspect", get(inspect))
        .route("/v1/status", get(status))
        .route("/v1/filters", get(filters))
        .with_state(engine)
}

// ── Request types ────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
struct SearchBody {
    /// Bitmap filter keys (e.g. ["tag:work", "type:task"])
    filters: Vec<String>,
    /// Boolean operation: "and" (default) or "or"
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
struct InspectQuery {
    /// File path to inspect
    path: String,
}

#[derive(Debug, Deserialize)]
struct FiltersQuery {
    /// Optional category filter: "tag", "folder", "link", "type", "source"
    category: Option<String>,
}

// ── Handlers ─────────────────────────────────────────────────────

async fn search(
    State(engine): State<AppState>,
    Json(body): Json<SearchBody>,
) -> Result<impl IntoResponse, AppError> {
    let engine = engine.clone();
    let result = tokio::task::spawn_blocking(move || {
        let filter = if body.filters.is_empty() {
            Filter::Key("source:obsidian".into())
        } else if body.filters.len() == 1 {
            Filter::Key(body.filters[0].clone())
        } else {
            let keys: Vec<Filter> = body.filters.iter().map(|k| Filter::Key(k.clone())).collect();
            match body.op.as_str() {
                "or" => Filter::Or(keys),
                _ => Filter::And(keys),
            }
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

    use biem_bitmap::memory::InMemoryBitmapStore;
    use biem_core::bitmap::BitmapStore;
    use biem_core::registry::{NewDoc, Registry};
    use biem_core::types::{DocType, SourceType};
    use biem_query::BitmapQueryEngine;
    use biem_registry::memory::InMemoryRegistry;

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
