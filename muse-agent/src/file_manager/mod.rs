pub mod auth;
pub mod config;
pub mod template;

use axum::{
    body::Body,
    extract::{Query, Request, State},
    http::{header, HeaderMap, StatusCode, Uri},
    response::{Html, IntoResponse, Response},
    routing::get,
    Router,
};
use percent_encoding::{percent_decode_str, utf8_percent_encode, AsciiSet, CONTROLS};
use std::path::Path;
use std::sync::Arc;
use std::time::UNIX_EPOCH;
use tower::ServiceExt;
use tower_http::services::ServeFile;
use tracing::warn;

// RFC 3986 path segment safe characters:
// Tidak meng-encode huruf, angka, `-`, `_`, `.`, `~`
const PATH_SEGMENT: &AsciiSet = &CONTROLS
    .add(b' ')
    .add(b'"')
    .add(b'#')
    .add(b'<')
    .add(b'>')
    .add(b'?')
    .add(b'`')
    .add(b'{')
    .add(b'}')
    .add(b'/')
    .add(b'%');

pub use config::FileManagerConfig;

#[derive(Clone)]
pub struct FileManagerState {
    pub config: Arc<FileManagerConfig>,
}

#[derive(serde::Deserialize)]
pub struct FileQuery {
    pub raw: Option<String>,
}

pub fn create_file_manager_router(config: FileManagerConfig) -> Router {
    let state = FileManagerState {
        config: Arc::new(config),
    };

    Router::new()
        .route("/health", get(handle_health))
        .route("/healthz", get(handle_health))
        .route("/", get(handle_root))
        .route(
            "/internal/sync/manifest",
            get(crate::server::internal_sync_manifest_fm),
        )
        .route(
            "/internal/sync/download",
            get(crate::server::internal_sync_download_fm),
        )
        .route("/*path", get(handle_path))
        .with_state(state)
}

async fn handle_health() -> impl IntoResponse {
    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "application/json")],
        "{\"status\":\"healthy\",\"ok\":true,\"service\":\"file-manager\"}\n",
    )
}

async fn handle_root(
    State(state): State<FileManagerState>,
    headers: HeaderMap,
    query: Query<FileQuery>,
    req: Request,
) -> Response {
    serve_route(state, "", headers, query, req).await
}

async fn handle_path(
    State(state): State<FileManagerState>,
    uri: Uri,
    headers: HeaderMap,
    query: Query<FileQuery>,
    req: Request,
) -> Response {
    let raw_path = uri.path().trim_start_matches('/');
    let decoded_path = percent_decode_str(raw_path).decode_utf8_lossy().to_string();
    serve_route(state, &decoded_path, headers, query, req).await
}

async fn serve_route(
    state: FileManagerState,
    rel_path: &str,
    headers: HeaderMap,
    Query(query): Query<FileQuery>,
    req: Request,
) -> Response {
    let auth_header = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok());

    if !auth::verify_basic_auth(auth_header, &state.config) {
        return Response::builder()
            .status(StatusCode::UNAUTHORIZED)
            .header(header::WWW_AUTHENTICATE, r#"Basic realm="File Server""#)
            .header(header::CONTENT_TYPE, "text/plain; charset=utf-8")
            .body(Body::from("Unauthorized\n"))
            .unwrap();
    }

    let root = state.config.root_path();
    let fs_path = match auth::safe_resolve(&root, rel_path) {
        Some(p) => p,
        None => {
            return Response::builder()
                .status(StatusCode::NOT_FOUND)
                .header(header::CONTENT_TYPE, "text/plain; charset=utf-8")
                .body(Body::from("404 Not Found\n"))
                .unwrap();
        }
    };

    if !fs_path.exists() {
        return Response::builder()
            .status(StatusCode::NOT_FOUND)
            .header(header::CONTENT_TYPE, "text/plain; charset=utf-8")
            .body(Body::from("404 Not Found\n"))
            .unwrap();
    }

    if fs_path.is_dir() {
        return serve_directory(&root, &fs_path, rel_path);
    }

    // Check if markdown file
    let ext = fs_path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_lowercase();

    if ext == "md" || ext == "markdown" {
        let is_raw = query.raw.as_deref() == Some("1");
        return serve_markdown(&root, &fs_path, rel_path, is_raw);
    }

    // General files: use tower-http ServeFile for Range, ETag, etc.
    let mut resp = match ServeFile::new(&fs_path).oneshot(req).await {
        Ok(r) => r.into_response(),
        Err(e) => {
            warn!("Failed to serve file {:?}: {}", fs_path, e);
            return Response::builder()
                .status(StatusCode::INTERNAL_SERVER_ERROR)
                .body(Body::from("Error serving file\n"))
                .unwrap();
        }
    };

    // Special handling for .html: ensure proper Content-Type for inline rendering
    if ext == "html" || ext == "htm" {
        resp.headers_mut().insert(
            header::CONTENT_TYPE,
            "text/html; charset=utf-8".parse().unwrap(),
        );
        resp.headers_mut()
            .insert(header::CONTENT_DISPOSITION, "inline".parse().unwrap());
    }

    resp
}

fn serve_directory(_root: &Path, dir_path: &Path, rel_path: &str) -> Response {
    let clean_rel = rel_path.trim_matches('/');
    let parts: Vec<&str> = if clean_rel.is_empty() {
        Vec::new()
    } else {
        clean_rel.split('/').filter(|s| !s.is_empty()).collect()
    };

    // Breadcrumbs
    let mut crumbs_html = vec![r#"<a href="/">Root</a>"#.to_string()];
    let mut accum = String::new();
    for (i, p) in parts.iter().enumerate() {
        accum.push('/');
        accum.push_str(p);
        let enc_href = encode_uri_path(&accum);
        if i == parts.len() - 1 {
            crumbs_html.push(html_escape(p));
        } else {
            crumbs_html.push(format!(r#"<a href="{enc_href}/">{}</a>"#, html_escape(p)));
        }
    }
    let crumb = crumbs_html.join(" › ");

    let mut rows = Vec::new();

    // Up one directory if not at root
    if !parts.is_empty() {
        let up_href = if parts.len() == 1 {
            "/".to_string()
        } else {
            let parent_parts = &parts[..parts.len() - 1];
            format!("/{}/", parent_parts.join("/"))
        };
        rows.push(format!(
            r#"<a class="file-row parent-row" href="{up_href}">
  <div class="badge b-dir">▲</div>
  <div class="col-name"><b>..</b><span>Up one directory</span></div>
  <div class="col-size">-</div>
  <div class="col-date">-</div>
  <div class="col-action">‹</div>
</a>"#
        ));
    }

    let entries = match std::fs::read_dir(dir_path) {
        Ok(e) => e,
        Err(_) => {
            return Response::builder()
                .status(StatusCode::FORBIDDEN)
                .body(Body::from("Access denied / unreadable\n"))
                .unwrap();
        }
    };

    let mut dir_entries = Vec::new();
    let mut file_entries = Vec::new();

    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        if name.starts_with('.') {
            continue;
        }
        let is_dir = entry.file_type().map(|t| t.is_dir()).unwrap_or(false);
        if is_dir {
            dir_entries.push(name);
        } else {
            file_entries.push((name, entry.path()));
        }
    }

    dir_entries.sort_by_key(|a| a.to_lowercase());
    file_entries.sort_by_key(|a| a.0.to_lowercase());

    for name in dir_entries {
        let esc_name = html_escape(&name);
        let href = if clean_rel.is_empty() {
            format!("/{}/", encode_uri_path(&name))
        } else {
            format!(
                "/{}/{}/",
                encode_uri_path(clean_rel),
                encode_uri_path(&name)
            )
        };
        rows.push(format!(
            r#"<a class="file-row" href="{href}">
  <div class="badge b-dir">DIR</div>
  <div class="col-name"><b>{esc_name}</b><span>Folder</span></div>
  <div class="col-size">-</div>
  <div class="col-date">Direktori</div>
  <div class="col-action">›</div>
</a>"#
        ));
    }

    for (name, path) in file_entries {
        let esc_name = html_escape(&name);
        let (badge_cls, badge_lbl) = template::get_badge(&name, false);

        let (size_str, dt_str) = if let Ok(meta) = path.metadata() {
            let size = auth::human_size(meta.len());
            let mtime = meta
                .modified()
                .ok()
                .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                .map(|d| format_timestamp(d.as_secs()))
                .unwrap_or_else(|| "-".to_string());
            (size, mtime)
        } else {
            ("-".to_string(), "-".to_string())
        };

        let href = if clean_rel.is_empty() {
            format!("/{}", encode_uri_path(&name))
        } else {
            format!("/{}/{}", encode_uri_path(clean_rel), encode_uri_path(&name))
        };

        rows.push(format!(
            r#"<a class="file-row" href="{href}">
  <div class="badge {badge_cls}">{badge_lbl}</div>
  <div class="col-name"><b>{esc_name}</b><span>{size_str} • {dt_str}</span></div>
  <div class="col-size">{size_str}</div>
  <div class="col-date">{dt_str}</div>
  <div class="col-action">⭳</div>
</a>"#
        ));
    }

    let count_label = format!("{} items", rows.len());
    let page_html = template::render_html_page(
        "File Server",
        "File Server",
        &crumb,
        &rows.join("\n"),
        &count_label,
    );

    Html(page_html).into_response()
}

fn serve_markdown(_root: &Path, fs_path: &Path, rel_path: &str, raw: bool) -> Response {
    let raw_text = match std::fs::read_to_string(fs_path) {
        Ok(t) => t,
        Err(_) => {
            return Response::builder()
                .status(StatusCode::FORBIDDEN)
                .body(Body::from("Access denied / unreadable\n"))
                .unwrap();
        }
    };

    let filename = fs_path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("document.md");

    let clean_rel = rel_path.trim_matches('/');
    let parent = Path::new(clean_rel).parent().unwrap_or(Path::new(""));
    let back = if parent.as_os_str().is_empty() {
        "/".to_string()
    } else {
        format!("/{}/", encode_uri_path(&parent.to_string_lossy()))
    };

    let file_href = format!("/{}", encode_uri_path(clean_rel));

    let (raw_href, raw_label, body_html) = if raw {
        let esc = html_escape(&raw_text);
        let block = format!(
            r##"<div class="code-wrapper">
  <div class="code-header">
    <span>RAW MARKDOWN</span>
    <button class="btn-copy" onclick="copyCode(this)">
      <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><rect width="14" height="14" x="8" y="8" rx="2" ry="2"/><path d="M4 16c-1.1 0-2-.9-2-2V4c0-1.1.9-2 2-2h10c1.1 0 2 .9 2 2"/></svg>
      Copy All
    </button>
  </div>
  <pre><code class="language-markdown">{}</code></pre>
</div>"##,
            esc
        );
        (file_href, "Render", block)
    } else {
        let rendered = template::render_markdown_content(&raw_text);
        (format!("{}?raw=1", file_href), "Raw", rendered)
    };

    let page_html = template::render_markdown_page(
        &html_escape(filename),
        &back,
        &raw_href,
        raw_label,
        &body_html,
    );

    Html(page_html).into_response()
}

fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#x27;")
}

fn encode_uri_path(path: &str) -> String {
    path.split('/')
        .map(|seg| utf8_percent_encode(seg, PATH_SEGMENT).to_string())
        .collect::<Vec<_>>()
        .join("/")
}

fn format_timestamp(secs: u64) -> String {
    // Converts epoch seconds to DD Mon YYYY HH:MM format
    // Standard calendar calculation without external chrono dependency
    let days = secs / 86400;
    let rem_secs = secs % 86400;
    let hour = rem_secs / 3600;
    let minute = (rem_secs % 3600) / 60;

    // Gregorian date calculation from Unix epoch day 0 (1970-01-01)
    let z = days + 719468;
    let era = z / 146097;
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };

    let months = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    let mon = if (1..=12).contains(&m) {
        months[(m - 1) as usize]
    } else {
        "???"
    };

    format!("{:02} {} {} {:02}:{:02}", d, mon, y, hour, minute)
}
