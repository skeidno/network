use axum::body::Body;
use axum::extract::State;
use axum::http::header::{
    CACHE_CONTROL, CONTENT_SECURITY_POLICY, CONTENT_TYPE, REFERRER_POLICY, WWW_AUTHENTICATE,
    X_CONTENT_TYPE_OPTIONS, X_FRAME_OPTIONS,
};
use axum::http::{HeaderMap, HeaderValue, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use base64::Engine;
use serde_json::{json, Value};
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::sync::Mutex;

use crate::core::AppState;

pub type Shared = Arc<Mutex<AppState>>;

/// The WebGUI assets are compiled into the binary so a single executable is enough.
mod assets {
    include!(concat!(env!("OUT_DIR"), "/web_assets.rs"));
}

pub fn asset_bytes(relative: &str) -> Option<&'static [u8]> {
    assets::ASSETS
        .iter()
        .find(|asset| asset.path == relative)
        .map(|asset| asset.content)
}

/// Fallback source when the app runs next to a source checkout (development only).
fn asset_from_disk(relative: &str) -> Option<Vec<u8>> {
    let root = std::env::var("NETWORK_MANAGER_WEB_ROOT").unwrap_or_default();
    if root.is_empty() {
        return None;
    }
    std::fs::read(std::path::Path::new(&root).join(relative)).ok()
}

fn random_bytes(count: usize) -> Vec<u8> {
    use rand::RngCore;
    let mut buffer = vec![0u8; count];
    rand::thread_rng().fill_bytes(&mut buffer);
    buffer
}

/// URL-safe token, equivalent to Python `secrets.token_urlsafe(32)`.
pub fn random_session_token() -> String {
    let alphabet = data_encoding::BASE64URL_NOPAD;
    alphabet.encode(&random_bytes(32))
}

fn constant_time_eq(left: &str, right: &str) -> bool {
    let left = left.as_bytes();
    let right = right.as_bytes();
    if left.len() != right.len() {
        return false;
    }
    let mut diff = 0u8;
    for (a, b) in left.iter().zip(right.iter()) {
        diff |= a ^ b;
    }
    diff == 0
}

fn security_headers(mut response: Response) -> Response {
    let headers = response.headers_mut();
    headers.insert(CACHE_CONTROL, HeaderValue::from_static("no-store"));
    headers.insert(X_CONTENT_TYPE_OPTIONS, HeaderValue::from_static("nosniff"));
    headers.insert(X_FRAME_OPTIONS, HeaderValue::from_static("DENY"));
    headers.insert(REFERRER_POLICY, HeaderValue::from_static("no-referrer"));
    headers.insert(
        CONTENT_SECURITY_POLICY,
        HeaderValue::from_static(
            "default-src 'self'; style-src 'self' 'unsafe-inline'; img-src 'self'; connect-src 'self'",
        ),
    );
    response
}

fn bearer_is_valid(state: &AppState, headers: &HeaderMap) -> bool {
    if state.access_password.is_empty() {
        return true;
    }
    let Some(raw) = headers.get(axum::http::header::AUTHORIZATION) else {
        return false;
    };
    let Ok(raw) = raw.to_str() else {
        return false;
    };
    let Some(encoded) = raw.strip_prefix("Basic ") else {
        return false;
    };
    let Ok(decoded) = base64::engine::general_purpose::STANDARD.decode(encoded.trim()) else {
        return false;
    };
    let Ok(text) = String::from_utf8(decoded) else {
        return false;
    };
    let Some((username, password)) = text.split_once(':') else {
        return false;
    };
    constant_time_eq(username, &state.access_username)
        && constant_time_eq(password, &state.access_password)
}

fn session_is_valid(state: &AppState, headers: &HeaderMap) -> bool {
    let provided = headers
        .get("X-Network-Session")
        .and_then(|value| value.to_str().ok())
        .unwrap_or("");
    constant_time_eq(provided, &state.session_token)
}

fn authorized(state: &AppState, headers: &HeaderMap) -> bool {
    bearer_is_valid(state, headers) && session_is_valid(state, headers)
}

fn unauthorized_json() -> Response {
    (
        StatusCode::FORBIDDEN,
        Json(json!({"ok": false, "error": "请求未授权"})),
    )
        .into_response()
}

fn unauthorized_basic() -> Response {
    let mut response = "需要 WebGUI 管理凭据".to_string().into_response();
    *response.status_mut() = StatusCode::UNAUTHORIZED;
    let headers = response.headers_mut();
    headers.insert(
        WWW_AUTHENTICATE,
        HeaderValue::from_static("Basic realm=\"Network Manager\""),
    );
    headers.insert(
        CONTENT_TYPE,
        HeaderValue::from_static("text/plain; charset=utf-8"),
    );
    response
}

fn mime_for(name: &str) -> &'static str {
    match name.rsplit('.').next().unwrap_or("") {
        "html" | "htm" => "text/html; charset=utf-8",
        "js" => "application/javascript; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "json" => "application/json; charset=utf-8",
        "svg" => "image/svg+xml; charset=utf-8",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "ico" => "image/x-icon",
        "woff" => "font/woff",
        "woff2" => "font/woff2",
        "ttf" => "font/ttf",
        "txt" => "text/plain; charset=utf-8",
        _ => "application/octet-stream",
    }
}

fn serve_static(state: &AppState, request_path: &str) -> Response {
    let decoded = percent_decode(request_path);
    let relative = decoded.trim_start_matches('/');
    let relative = if relative.is_empty() {
        "index.html"
    } else {
        relative
    };
    // Block traversal outside the embedded asset tree.
    if relative.contains("..") || relative.starts_with('/') || relative.contains('\\') {
        return StatusCode::NOT_FOUND.into_response();
    }
    let embedded = asset_bytes(relative);
    let disk = if embedded.is_none() {
        asset_from_disk(relative)
    } else {
        None
    };
    let body: &[u8] = match (embedded, disk.as_deref()) {
        (Some(bytes), _) => bytes,
        (None, Some(bytes)) => bytes,
        _ => return StatusCode::NOT_FOUND.into_response(),
    };
    let mut response = if relative == "index.html" {
        let text = String::from_utf8_lossy(body);
        let rendered = text.replace("__SESSION_TOKEN__", &state.session_token);
        rendered.into_response()
    } else {
        Body::from(body.to_vec()).into_response()
    };
    response
        .headers_mut()
        .insert(CONTENT_TYPE, HeaderValue::from_static(mime_for(relative)));
    security_headers(response)
}

fn percent_decode(value: &str) -> String {
    let bytes = value.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut index = 0usize;
    while index < bytes.len() {
        if bytes[index] == b'%' && index + 2 < bytes.len() {
            let hi = bytes[index + 1];
            let lo = bytes[index + 2];
            if let (Some(h), Some(l)) = (hex_val(hi), hex_val(lo)) {
                out.push((h << 4) | l);
                index += 3;
                continue;
            }
        }
        out.push(bytes[index]);
        index += 1;
    }
    String::from_utf8_lossy(&out).to_string()
}

fn hex_val(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

async fn call_api(
    State(shared): State<Shared>,
    headers: HeaderMap,
    Json(request): Json<Value>,
) -> Response {
    let method = request
        .get("method")
        .and_then(|value| value.as_str())
        .unwrap_or("")
        .to_string();
    let args = match request.get("args") {
        Some(Value::Array(items)) => items.clone(),
        Some(_) => {
            return security_headers((
                StatusCode::BAD_REQUEST,
                Json(json!({"ok": false, "error": "参数格式无效"})),
            )
                .into_response())
        }
        None => Vec::new(),
    };
    if method.is_empty() {
        return security_headers((
            StatusCode::BAD_REQUEST,
            Json(json!({"ok": false, "error": "缺少操作名称"})),
        )
            .into_response());
    }
    {
        let state = shared.lock().await;
        if !authorized(&state, &headers) {
            return security_headers(unauthorized_json());
        }
    }

    // Long-running flows run outside the state lock so the UI keeps polling.
    if method == "deploySshServer" {
        match crate::deploy::run(shared.clone(), args).await {
            Ok(result) => {
                return security_headers(
                    Json(json!({"ok": true, "result": result})).into_response(),
                )
            }
            Err(message) => {
                return security_headers((
                    StatusCode::BAD_REQUEST,
                    Json(json!({"ok": false, "error": message})),
                )
                    .into_response())
            }
        }
    }

    let mut state = shared.lock().await;
    match crate::methods::dispatch(&shared, &mut state, &method, args).await {
        Ok(result) => security_headers(Json(json!({"ok": true, "result": result})).into_response()),
        Err(message) => security_headers((
            StatusCode::BAD_REQUEST,
            Json(json!({"ok": false, "error": message})),
        )
            .into_response()),
    }
}

async fn read_api(State(shared): State<Shared>, headers: HeaderMap, uri: Uri) -> Response {
    let method = if uri.path() == "/api/logs" {
        "getLogs"
    } else {
        "getState"
    };
    let mut state = shared.lock().await;
    if !authorized(&state, &headers) {
        return security_headers(unauthorized_json());
    }
    match crate::methods::dispatch(&shared, &mut state, method, Vec::new()).await {
        Ok(result) => {
            let content_type = if method == "getLogs" {
                "text/plain; charset=utf-8"
            } else {
                "application/json; charset=utf-8"
            };
            let body = match result {
                Value::String(text) => text,
                other => other.to_string(),
            };
            let mut response = body.into_response();
            response
                .headers_mut()
                .insert(CONTENT_TYPE, HeaderValue::from_static(content_type));
            security_headers(response)
        }
        Err(message) => security_headers((
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({"ok": false, "error": message})),
        )
            .into_response()),
    }
}

async fn static_asset(State(shared): State<Shared>, headers: HeaderMap, uri: Uri) -> Response {
    let state = shared.lock().await;
    if !bearer_is_valid(&state, &headers) {
        return security_headers(unauthorized_basic());
    }
    serve_static(&state, uri.path())
}

async fn health() -> Response {
    security_headers(Json(json!({"ok": true})).into_response())
}

async fn options_preflight() -> Response {
    let mut response = StatusCode::NO_CONTENT.into_response();
    let headers = response.headers_mut();
    headers.insert(
        axum::http::header::ALLOW,
        HeaderValue::from_static("GET, POST, OPTIONS"),
    );
    headers.insert(
        axum::http::header::CONTENT_LENGTH,
        HeaderValue::from_static("0"),
    );
    response
}

pub fn router(shared: Shared) -> Router {
    Router::new()
        .route("/health", get(health))
        .route("/api/state", get(read_api))
        .route("/api/logs", get(read_api))
        .route("/api/call", post(call_api).options(options_preflight))
        .route("/", get(static_asset))
        .fallback(get(static_asset))
        .with_state(shared)
}

pub async fn serve(shared: Shared, host: &str, port: u16) -> Result<SocketAddr, String> {
    let address: SocketAddr = if host.contains(':') {
        host.parse()
            .map_err(|_| format!("监听地址无效：{host}"))?
    } else {
        format!("{host}:{port}")
            .parse()
            .map_err(|_| format!("监听地址无效：{host}:{port}"))?
    };
    let listener = tokio::net::TcpListener::bind(address)
        .await
        .map_err(|err| format!("监听 {address} 失败：{err}"))?;
    let bound = listener.local_addr().map_err(|err| err.to_string())?;
    tokio::spawn(async move {
        let app = router(shared);
        if let Err(err) = axum::serve(listener, app).await {
            eprintln!("WebGUI 服务异常：{err}");
        }
    });
    Ok(bound)
}

/// Display URL, mirroring Python `LocalWebServer.url`.
pub fn display_url(host: &str, port: u16) -> String {
    let shown = match host {
        "0.0.0.0" | "::" => "127.0.0.1",
        other => other,
    };
    if shown.contains(':') && !shown.starts_with('[') {
        format!("http://[{shown}]:{port}/")
    } else {
        format!("http://{shown}:{port}/")
    }
}

