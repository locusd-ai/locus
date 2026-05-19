//! Webhook endpoint for push-based ingestion from remote sources.
//!
//! POST /v1/webhook/ingest
//!   Body: { "path": "confluence://SPACE/12345.confluence", "content_b64": "<base64>" }
//!   Response: { "action": "indexed|updated|skipped", "bitmaps_updated": N }
//!
//! Callers (Confluence/Jira/Slack integrations, CI pipelines, etc.) POST content
//! directly rather than polling. `locusd` forwards bytes to `IngestionPipeline::upsert_document`.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Json, Response};
use axum::routing::post;
use axum::Router;
use serde::{Deserialize, Serialize};

use locus_ingest::{IngestAction, IngestionPipeline};

// ── Shared state ─────────────────────────────────────────────────────────────

pub type WebhookState = Arc<Mutex<IngestionPipeline>>;

// ── Router ────────────────────────────────────────────────────────────────────

pub fn router(pipeline: WebhookState) -> Router {
    Router::new()
        .route("/v1/webhook/ingest", post(webhook_ingest))
        .with_state(pipeline)
}

// ── Request / Response ────────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
struct IngestRequest {
    /// Synthetic document path, e.g. `confluence://SPACE/12345.confluence`
    path: String,
    /// Base64-encoded raw document bytes (as returned by `RemoteSource::fetch_content`)
    content_b64: String,
}

#[derive(Debug, Serialize)]
struct IngestResponse {
    path: String,
    action: String,
    bitmaps_updated: u32,
}

struct WebhookError(String, StatusCode);

impl IntoResponse for WebhookError {
    fn into_response(self) -> Response {
        let body = serde_json::json!({ "error": self.0 });
        (self.1, Json(body)).into_response()
    }
}

// ── Handler ───────────────────────────────────────────────────────────────────

async fn webhook_ingest(
    State(pipeline): State<WebhookState>,
    Json(req): Json<IngestRequest>,
) -> Result<impl IntoResponse, WebhookError> {
    let path = PathBuf::from(&req.path);

    let content = base64_decode(&req.content_b64).map_err(|e| {
        WebhookError(
            format!("invalid base64: {e}"),
            StatusCode::UNPROCESSABLE_ENTITY,
        )
    })?;

    let result = tokio::task::spawn_blocking(move || {
        let mut guard = pipeline.lock().map_err(|_| "pipeline mutex poisoned".to_string())?;
        guard.upsert_document(path, content).map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| WebhookError(format!("spawn error: {e}"), StatusCode::INTERNAL_SERVER_ERROR))?
    .map_err(|e| WebhookError(e, StatusCode::INTERNAL_SERVER_ERROR))?;

    let action_str = match result.action {
        IngestAction::Indexed => "indexed",
        IngestAction::Updated => "updated",
        IngestAction::Skipped => "skipped",
        IngestAction::Tombstoned => "tombstoned",
        IngestAction::Moved => "moved",
    };

    Ok(Json(IngestResponse {
        path: req.path,
        action: action_str.to_string(),
        bitmaps_updated: result.bitmaps_updated,
    }))
}

// ── Base64 decoder ────────────────────────────────────────────────────────────

fn base64_decode(s: &str) -> Result<Vec<u8>, String> {
    const TABLE: [u8; 128] = {
        let mut t = [255u8; 128];
        let mut i = 0u8;
        // A-Z = 0-25
        while i < 26 {
            t[(b'A' + i) as usize] = i;
            i += 1;
        }
        i = 0;
        while i < 26 {
            t[(b'a' + i) as usize] = 26 + i;
            i += 1;
        }
        i = 0;
        while i < 10 {
            t[(b'0' + i) as usize] = 52 + i;
            i += 1;
        }
        t[b'+' as usize] = 62;
        t[b'/' as usize] = 63;
        t[b'=' as usize] = 0; // padding
        t
    };

    let s = s.trim();
    let bytes = s.as_bytes();
    if bytes.len() % 4 != 0 {
        return Err(format!("base64 length {} not a multiple of 4", bytes.len()));
    }

    let mut out = Vec::with_capacity((bytes.len() / 4) * 3);
    let mut i = 0;
    while i < bytes.len() {
        let b0 = bytes[i];
        let b1 = bytes[i + 1];
        let b2 = bytes[i + 2];
        let b3 = bytes[i + 3];

        for &b in &[b0, b1, b2, b3] {
            if b >= 128 || (TABLE[b as usize] == 255 && b != b'=') {
                return Err(format!("invalid base64 character: {:?}", b as char));
            }
        }

        let n = ((TABLE[b0 as usize] as u32) << 18)
            | ((TABLE[b1 as usize] as u32) << 12)
            | ((TABLE[b2 as usize] as u32) << 6)
            | (TABLE[b3 as usize] as u32);

        out.push((n >> 16) as u8);
        if b2 != b'=' {
            out.push((n >> 8) as u8);
        }
        if b3 != b'=' {
            out.push(n as u8);
        }
        i += 4;
    }

    Ok(out)
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    use axum::body::Body;
    use axum::http::{Method, Request};
    use tower::ServiceExt;

    use locus_bitmap::memory::InMemoryBitmapStore;
    use locus_ingest::IngestionPipeline;
    use locus_parser::markdown::MarkdownParser;
    use locus_registry::memory::InMemoryRegistry;

    fn make_pipeline() -> WebhookState {
        let pipeline = IngestionPipeline::new(
            vec![Box::new(MarkdownParser)],
            Box::new(InMemoryRegistry::new()),
            Box::new(InMemoryBitmapStore::new()),
        );
        Arc::new(Mutex::new(pipeline))
    }

    async fn body_json(response: axum::http::Response<Body>) -> serde_json::Value {
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        serde_json::from_slice(&bytes).unwrap()
    }

    fn b64(s: &[u8]) -> String {
        const TABLE: &[u8] =
            b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        let mut out = String::new();
        let mut i = 0;
        while i < s.len() {
            let b0 = s[i] as u32;
            let b1 = s.get(i + 1).copied().unwrap_or(0) as u32;
            let b2 = s.get(i + 2).copied().unwrap_or(0) as u32;
            let n = (b0 << 16) | (b1 << 8) | b2;
            out.push(TABLE[((n >> 18) & 63) as usize] as char);
            out.push(TABLE[((n >> 12) & 63) as usize] as char);
            if i + 1 < s.len() {
                out.push(TABLE[((n >> 6) & 63) as usize] as char);
            } else {
                out.push('=');
            }
            if i + 2 < s.len() {
                out.push(TABLE[(n & 63) as usize] as char);
            } else {
                out.push('=');
            }
            i += 3;
        }
        out
    }

    #[tokio::test]
    async fn test_webhook_ingest_new_document() {
        let pipeline = make_pipeline();
        let app = router(pipeline);

        let md = b"# Hello\n\nThis is a test note.";
        let body = serde_json::json!({
            "path": "/vault/remote/hello.md",
            "content_b64": b64(md),
        });

        let req = Request::builder()
            .method(Method::POST)
            .uri("/v1/webhook/ingest")
            .header("content-type", "application/json")
            .body(Body::from(body.to_string()))
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let json = body_json(resp).await;
        assert_eq!(json["action"], "indexed");
    }

    #[tokio::test]
    async fn test_webhook_ingest_idempotent() {
        let pipeline = make_pipeline();

        let md = b"# Hello\n\nSame content.";
        let body = serde_json::json!({
            "path": "/vault/remote/hello.md",
            "content_b64": b64(md),
        });

        // First ingest
        let app = router(pipeline.clone());
        let req = Request::builder()
            .method(Method::POST)
            .uri("/v1/webhook/ingest")
            .header("content-type", "application/json")
            .body(Body::from(body.to_string()))
            .unwrap();
        app.oneshot(req).await.unwrap();

        // Second ingest (same content) → skipped
        let app2 = router(pipeline.clone());
        let req2 = Request::builder()
            .method(Method::POST)
            .uri("/v1/webhook/ingest")
            .header("content-type", "application/json")
            .body(Body::from(body.to_string()))
            .unwrap();
        let resp2 = app2.oneshot(req2).await.unwrap();
        let json2 = body_json(resp2).await;
        assert_eq!(json2["action"], "skipped");
    }

    #[tokio::test]
    async fn test_webhook_bad_base64() {
        let pipeline = make_pipeline();
        let app = router(pipeline);

        let body = serde_json::json!({
            "path": "/vault/remote/hello.md",
            "content_b64": "!!!not-base64!!!",
        });

        let req = Request::builder()
            .method(Method::POST)
            .uri("/v1/webhook/ingest")
            .header("content-type", "application/json")
            .body(Body::from(body.to_string()))
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::UNPROCESSABLE_ENTITY);
    }

    #[test]
    fn test_base64_roundtrip() {
        let inputs = [
            b"".as_slice(),
            b"a",
            b"ab",
            b"abc",
            b"Hello, World!",
            b"\x00\xff\xfe",
        ];
        for input in inputs {
            let encoded = b64(input);
            let decoded = base64_decode(&encoded).unwrap();
            assert_eq!(decoded, input, "roundtrip failed for {input:?}");
        }
    }
}
