//! `cameo://localhost/<boardId>/<rel-path>` — serves image bytes from a Board folder.
//!
//! Path canonicalization + traversal guard ported from Riff's `riff://` scheme:
//! reject `..`/absolute components, then verify the canonical path stays inside
//! the Board folder.

use crate::board::BoardRegistry;
use std::path::{Component, PathBuf};
use std::sync::Arc;
use tauri::http::{header, Request, Response, StatusCode, Uri};
use tauri::{Manager, UriSchemeContext};

fn parse_cameo_uri(uri: &Uri) -> Result<(String, String), (StatusCode, &'static str)> {
    let host = uri.host().unwrap_or_default();
    let path = uri.path().trim_start_matches('/');

    // Tauri/WebView2 represents custom protocols as `http://<scheme>.localhost/...`
    // on Windows. Keep board routing in the path and only support host-as-board
    // for legacy `cameo://<boardId>/<rel-path>` URLs used before this fix.
    if !host.is_empty() && host != "localhost" && !host.ends_with(".localhost") {
        if path.is_empty() {
            return Err((
                StatusCode::BAD_REQUEST,
                "missing image path in cameo:// URL",
            ));
        }
        return Ok((host.to_string(), path.to_string()));
    }

    let Some((board_raw, rel_raw)) = path.split_once('/') else {
        return Err((StatusCode::BAD_REQUEST, "missing boardId in cameo:// URL"));
    };
    if board_raw.is_empty() || rel_raw.is_empty() {
        return Err((
            StatusCode::BAD_REQUEST,
            "missing boardId or image path in cameo:// URL",
        ));
    }
    let board_id = urlencoding::decode(board_raw)
        .map(|c| c.into_owned())
        .unwrap_or_else(|_| board_raw.to_string());
    Ok((board_id, rel_raw.to_string()))
}

pub fn handle_cameo_uri<R: tauri::Runtime>(
    ctx: UriSchemeContext<'_, R>,
    request: Request<Vec<u8>>,
) -> Response<Vec<u8>> {
    let uri = request.uri();

    let (board_id, rel_raw) = match parse_cameo_uri(uri) {
        Ok(parsed) => parsed,
        Err((code, msg)) => return error_response(code, msg),
    };

    let app = ctx.app_handle();
    let registry = app.state::<Arc<BoardRegistry>>();
    let folder = match registry.folder(&board_id) {
        Some(f) => f,
        None => return error_response(StatusCode::NOT_FOUND, "unknown board"),
    };

    let rel = urlencoding::decode(&rel_raw)
        .map(|c| c.into_owned())
        .unwrap_or_else(|_| rel_raw.to_string());

    let rel_path = PathBuf::from(&rel);
    for comp in rel_path.components() {
        if !matches!(comp, Component::Normal(_)) {
            return error_response(StatusCode::FORBIDDEN, "path traversal blocked");
        }
    }

    let abs = folder.join(&rel_path);
    let canonical = match std::fs::canonicalize(&abs) {
        Ok(p) => p,
        Err(_) => return error_response(StatusCode::NOT_FOUND, "file not found"),
    };
    let base_canonical = match std::fs::canonicalize(&folder) {
        Ok(p) => p,
        Err(_) => return error_response(StatusCode::NOT_FOUND, "board folder gone"),
    };
    if !canonical.starts_with(&base_canonical) {
        return error_response(StatusCode::FORBIDDEN, "escape attempt blocked");
    }

    let bytes = match std::fs::read(&canonical) {
        Ok(b) => b,
        Err(e) => return error_response(StatusCode::INTERNAL_SERVER_ERROR, &e.to_string()),
    };
    let mime = mime_guess::from_path(&canonical)
        .first_or_octet_stream()
        .to_string();

    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, mime)
        // The webview origin (http://localhost:1420 in dev, tauri://localhost in
        // prod) fetches this cross-origin for textures.
        .header("Access-Control-Allow-Origin", "*")
        .header("Cache-Control", "no-store")
        .body(bytes)
        .unwrap_or_else(|_| error_response(StatusCode::INTERNAL_SERVER_ERROR, "build response"))
}

fn error_response(code: StatusCode, msg: &str) -> Response<Vec<u8>> {
    Response::builder()
        .status(code)
        .header(header::CONTENT_TYPE, "text/plain; charset=utf-8")
        .header("Access-Control-Allow-Origin", "*")
        .body(msg.as_bytes().to_vec())
        .unwrap()
}

#[cfg(test)]
mod tests {
    use super::parse_cameo_uri;
    use tauri::http::Uri;

    #[test]
    fn parses_path_scoped_cameo_url() {
        let uri: Uri = "cameo://localhost/board-1/gen-20260526.png"
            .parse()
            .unwrap();
        let (board, rel) = parse_cameo_uri(&uri).unwrap();
        assert_eq!(board, "board-1");
        assert_eq!(rel, "gen-20260526.png");
    }

    #[test]
    fn parses_windows_webview2_custom_protocol_shape() {
        let uri: Uri = "http://cameo.localhost/board-1/gen-20260526.png"
            .parse()
            .unwrap();
        let (board, rel) = parse_cameo_uri(&uri).unwrap();
        assert_eq!(board, "board-1");
        assert_eq!(rel, "gen-20260526.png");
    }

    #[test]
    fn keeps_legacy_host_scoped_urls_working() {
        let uri: Uri = "cameo://board-1/gen-20260526.png".parse().unwrap();
        let (board, rel) = parse_cameo_uri(&uri).unwrap();
        assert_eq!(board, "board-1");
        assert_eq!(rel, "gen-20260526.png");
    }
}
