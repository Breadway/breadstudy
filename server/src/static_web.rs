use std::path::{Path, PathBuf};

use axum::{
    body::Body,
    extract::Path as UrlPath,
    http::{header, StatusCode},
    response::{IntoResponse, Response},
};

/// Serve the installed PWA from a directory, with `/` mapped to index.html.
///
/// Paths are joined defensively: a request path containing `..` or a NUL byte
/// is rejected rather than being allowed to escape the web root.
pub async fn serve(web_dir: PathBuf, path: Option<UrlPath<String>>) -> Response {
    let requested = path.map(|UrlPath(p)| p).unwrap_or_default();
    let relative = if requested.is_empty() {
        "index.html".to_string()
    } else {
        requested
    };

    if relative.split('/').any(|part| part == ".." || part.contains('\0')) {
        return StatusCode::BAD_REQUEST.into_response();
    }

    let mut file = web_dir.join(&relative);
    if file.is_dir() {
        file = file.join("index.html");
    }

    match tokio::fs::read(&file).await {
        Ok(bytes) => {
            let mime = mime_for(&file);
            Response::builder()
                .status(StatusCode::OK)
                .header(header::CONTENT_TYPE, mime)
                .header(header::CACHE_CONTROL, "no-cache")
                .body(Body::from(bytes))
                .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())
        }
        Err(_) => {
            // Unknown static path: fall back to the shell so deep links work.
            match tokio::fs::read(web_dir.join("index.html")).await {
                Ok(bytes) => Response::builder()
                    .status(StatusCode::OK)
                    .header(header::CONTENT_TYPE, "text/html; charset=utf-8")
                    .body(Body::from(bytes))
                    .unwrap_or_else(|_| StatusCode::NOT_FOUND.into_response()),
                Err(_) => StatusCode::NOT_FOUND.into_response(),
            }
        }
    }
}

fn mime_for(path: &Path) -> &'static str {
    match path.extension().and_then(|ext| ext.to_str()) {
        Some("html") => "text/html; charset=utf-8",
        Some("js") => "text/javascript; charset=utf-8",
        Some("css") => "text/css; charset=utf-8",
        Some("json") | Some("webmanifest") => "application/json",
        Some("png") => "image/png",
        Some("svg") => "image/svg+xml",
        Some("ico") => "image/x-icon",
        Some("woff2") => "font/woff2",
        _ => "application/octet-stream",
    }
}
