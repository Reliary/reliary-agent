//! MCP over SSE transport — runs on the same axum server as the proxy (port :9090).
//
// Shared state with proxy: session hashes, anti-decision DB, response cache.
// Stdio MCP (mcp.rs) remains as the always-available fallback.
//
// Protocol (MCP 2024-11-05 SSE transport):
//   1. Client: GET /mcp/sse
//   2. Server: SSE event "endpoint" with /mcp/messages?sessionId=xxx
//   3. Client: POST /mcp/messages?sessionId=xxx (JSON-RPC body)
//   4. Server: SSE event "message" with JSON-RPC response
//
// Cleanup: sessions auto-expire after 5 min idle. SSE disconnect cleans up immediately.

use std::collections::HashMap;
use std::sync::{Mutex, LazyLock};
use std::time::{Duration, Instant};
use std::convert::Infallible;
use axum::{
    Json, extract::Query,
    http::StatusCode,
    response::{IntoResponse, sse::{Event, Sse}},
};
use serde_json::Value;
use bytes::Bytes;
use tokio::sync::mpsc;
use tokio_stream::{wrappers::UnboundedReceiverStream, StreamExt as TokioStreamExt};
use futures_util::stream::Stream;

// ── Session structures ──

struct SseSession {
    tx: mpsc::UnboundedSender<McpEvent>,
    created: Instant,
}

enum McpEvent {
    Response(String),
}

static SSE_SESSIONS: LazyLock<Mutex<HashMap<String, SseSession>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

const SESSION_TTL: Duration = Duration::from_secs(300);

fn prune_stale(guard: &mut HashMap<String, SseSession>) {
    let now = Instant::now();
    guard.retain(|_id, sess| now.duration_since(sess.created) < SESSION_TTL);
}

/// GET /mcp/sse — establish SSE connection, return stream.
pub async fn sse_handler() -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    let session_id = uuid::generate();
    let (tx, rx) = mpsc::unbounded_channel::<McpEvent>();

    // Register session
    {
        let mut guard = SSE_SESSIONS.lock().unwrap_or_else(|e| e.into_inner());
        prune_stale(&mut guard);
        guard.insert(session_id.clone(), SseSession { tx, created: Instant::now() });
    }

    // Build endpoint event
    let endpoint_msg = format!(
        "event: endpoint\ndata: /mcp/messages?sessionId={}\n\n",
        session_id
    );

    // Convert channel to stream with cleanup on drop
    let cleanup_id = session_id;
    let rx_stream = UnboundedReceiverStream::new(rx);

    let stream = rx_stream.map(move |event| {
        let McpEvent::Response(data) = event;
        Ok(Event::default().data(data).event("message"))
    });

    // Chain endpoint event + response stream
    let endpoint = futures_util::stream::once(async { Ok(Event::default().data(endpoint_msg)) });
    let combined = endpoint.chain(stream);

    // On drop: clean up session
    let drop_guard = SessionDrop { id: cleanup_id };
    let combined = DropStream { inner: Box::pin(combined), _guard: drop_guard };

    Sse::new(combined)
}

struct SessionDrop { id: String }
impl Drop for SessionDrop {
    fn drop(&mut self) {
        if let Ok(mut guard) = SSE_SESSIONS.lock() {
            guard.remove(&self.id);
        }
    }
}

use std::pin::Pin;
use std::task::{Context, Poll};
struct DropStream<S> {
    inner: Pin<Box<S>>,
    _guard: SessionDrop,
}

impl<S: Stream> Stream for DropStream<S> {
    type Item = S::Item;
    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        self.inner.as_mut().poll_next(cx)
    }
}

/// POST /mcp/messages?sessionId=xxx — receive JSON-RPC, dispatch, queue response.
pub async fn messages_handler(
    Query(params): Query<HashMap<String, String>>,
    body: Bytes,
) -> axum::response::Response {
    let session_id = params.get("sessionId").cloned().unwrap_or_default();
    if session_id.is_empty() {
        return (StatusCode::BAD_REQUEST, Json(serde_json::json!({"error": "missing sessionId"}))).into_response();
    }

    let msg: Value = match serde_json::from_slice(&body) {
        Ok(v) => v,
        Err(e) => return (StatusCode::BAD_REQUEST, Json(serde_json::json!({"error": format!("json parse: {}", e)}))).into_response(),
    };

    let id = msg.get("id").and_then(|v| v.as_u64()).unwrap_or(0);
    let method = msg.get("method").and_then(|v| v.as_str()).unwrap_or("");

    let response = match method {
        "initialize" => serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "result": {
                "protocolVersion": "2024-11-05",
                "capabilities": { "tools": {} },
                "serverInfo": { "name": "reliary", "version": env!("CARGO_PKG_VERSION") }
            }
        }),
        "notifications/initialized" => {
            return StatusCode::OK.into_response();
        }
        "tools/list" => {
            let tools = crate::mcp::tool_definitions();
            serde_json::json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": { "tools": tools }
            })
        }
        "tools/call" => {
            let params = match msg.get("params") {
                Some(p) => p,
                None => {
                    return (StatusCode::BAD_REQUEST, Json(serde_json::json!({
                        "jsonrpc": "2.0", "id": id,
                        "error": {"code": -32602, "message": "missing params"}
                    }))).into_response();
                }
            };
            let name = params.get("name").and_then(|v| v.as_str()).unwrap_or("");
            let args = params.get("arguments").and_then(|v| v.as_object()).cloned().unwrap_or_default();

            match crate::mcp::dispatch_tool_call(name, &args) {
                crate::mcp::DispatchResult::Success(result) => {
                    serde_json::json!({
                        "jsonrpc": "2.0",
                        "id": id,
                        "result": result
                    })
                }
                crate::mcp::DispatchResult::Error(code, message) => {
                    serde_json::json!({
                        "jsonrpc": "2.0",
                        "id": id,
                        "error": {"code": code, "message": message}
                    })
                }
            }
        }
        _ => {
            if method.starts_with("notifications/") {
                return StatusCode::OK.into_response();
            }
            serde_json::json!({
                "jsonrpc": "2.0",
                "id": id,
                "error": {"code": -32601, "message": format!("method not found: {}", method)}
            })
        }
    };

    // Queue response for delivery via SSE
    if let Ok(json_str) = serde_json::to_string(&response) {
        let sent = {
            let guard = SSE_SESSIONS.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(sess) = guard.get(&session_id) {
                sess.tx.send(McpEvent::Response(json_str)).is_ok()
            } else {
                false
            }
        };
        if !sent {
            return (StatusCode::NOT_FOUND, Json(serde_json::json!({
                "error": "session not found or closed"
            }))).into_response();
        }
    }

    StatusCode::OK.into_response()
}

pub mod uuid {
    use std::sync::atomic::{AtomicU64, Ordering};

    static COUNTER: AtomicU64 = AtomicU64::new(0);

    pub fn generate() -> String {
        let count = COUNTER.fetch_add(1, Ordering::Relaxed);
        let pid = std::process::id() as u64;
        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        format!("{:x}-{:x}-{:x}", pid, ts, count)
    }
}
