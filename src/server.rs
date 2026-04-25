//! axum-based HTTP server with SSE.

use std::convert::Infallible;
use std::time::Duration;

use axum::body::Body;
use axum::extract::{Path, State};
use axum::http::{header, HeaderMap, HeaderValue, Request, StatusCode};
use axum::middleware::{self, Next};
use axum::response::sse::{Event as SseEvent, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::Router;
use futures::stream::{Stream, StreamExt};
use include_dir::{include_dir, Dir};
use tokio_stream::wrappers::BroadcastStream;

use crate::security::{host_allowed, CSP_HEADER};
use crate::state::{Event as AppEvent, Store};

static STATIC_DIR: Dir<'_> = include_dir!("$CARGO_MANIFEST_DIR/static");

#[derive(Clone)]
pub struct AppState {
    pub store: Store,
    pub port: u16,
}

pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/", get(index))
        .route("/index.html", get(index))
        .route("/favicon.ico", get(favicon))
        .route("/static/{*path}", get(static_file))
        .route("/api/snapshot", get(snapshot))
        .route("/api/session/{id}", get(session))
        .route("/api/events", get(events))
        .with_state(state.clone())
        .layer(middleware::from_fn_with_state(state, host_check))
}

fn security_headers(mut resp: Response) -> Response {
    let h = resp.headers_mut();
    h.insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static(CSP_HEADER),
    );
    h.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    h.insert(header::REFERRER_POLICY, HeaderValue::from_static("no-referrer"));
    h.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    resp
}

async fn host_check(
    State(state): State<AppState>,
    headers: HeaderMap,
    req: Request<Body>,
    next: Next,
) -> Result<Response, Response> {
    let host = headers.get(header::HOST).and_then(|v| v.to_str().ok());
    if !host_allowed(host, state.port) {
        let resp = (StatusCode::FORBIDDEN, "Bad Host").into_response();
        return Err(security_headers(resp));
    }
    let resp = next.run(req).await;
    Ok(security_headers(resp))
}

async fn index() -> impl IntoResponse {
    serve_static_inner("index.html")
}

async fn favicon() -> impl IntoResponse {
    serve_static_inner("favicon.svg")
}

async fn static_file(Path(rel): Path<String>) -> impl IntoResponse {
    serve_static_inner(&rel)
}

fn serve_static_inner(rel: &str) -> Response {
    if rel.contains("..") {
        return (StatusCode::FORBIDDEN, "bad path").into_response();
    }
    match STATIC_DIR.get_file(rel) {
        Some(file) => {
            let mime = mime_guess::from_path(rel).first_or_octet_stream();
            let mut ct = mime.essence_str().to_string();
            if ct.starts_with("text/") || ct.contains("javascript") || ct.contains("json") {
                ct.push_str("; charset=utf-8");
            }
            let bytes = file.contents().to_vec();
            ([(header::CONTENT_TYPE, ct)], bytes).into_response()
        }
        None => (StatusCode::NOT_FOUND, "not found").into_response(),
    }
}

async fn snapshot(State(state): State<AppState>) -> Response {
    let payload = state.store.snapshot().await;
    let body = serde_json::to_vec(&payload).unwrap_or_else(|_| b"{}".to_vec());
    (
        [(header::CONTENT_TYPE, "application/json; charset=utf-8")],
        body,
    )
        .into_response()
}

async fn session(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Response {
    match state.store.session_summary(&id).await {
        Some(s) => {
            let body = serde_json::to_vec(&s).unwrap_or_else(|_| b"{}".to_vec());
            (
                [(header::CONTENT_TYPE, "application/json; charset=utf-8")],
                body,
            )
                .into_response()
        }
        None => (StatusCode::NOT_FOUND, "no such session").into_response(),
    }
}

async fn events(State(state): State<AppState>) -> Sse<impl Stream<Item = Result<SseEvent, Infallible>>> {
    let rx = state.store.subscribe();
    let stream = BroadcastStream::new(rx).filter_map(|res| async move {
        match res {
            Ok(ev) => {
                let (name, data) = match &ev {
                    AppEvent::SessionUpdate(s) => (
                        "session_update",
                        serde_json::to_string(s).unwrap_or_default(),
                    ),
                    AppEvent::TaskUpdate { sid, todos } => (
                        "task_update",
                        serde_json::to_string(&serde_json::json!({
                            "sid": sid,
                            "todos": todos
                        }))
                        .unwrap_or_default(),
                    ),
                    AppEvent::Heartbeat { ts } => (
                        "heartbeat",
                        serde_json::to_string(&serde_json::json!({ "ts": ts }))
                            .unwrap_or_default(),
                    ),
                };
                Some(Ok::<_, Infallible>(SseEvent::default().event(name).data(data)))
            }
            // Lagged or any other recv error: drop the event silently.
            Err(_) => None,
        }
    });
    Sse::new(stream).keep_alive(
        KeepAlive::new()
            .interval(Duration::from_secs(15))
            .text("keep-alive"),
    )
}
