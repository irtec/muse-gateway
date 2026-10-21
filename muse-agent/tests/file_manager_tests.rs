use axum::{
    body::Body,
    http::{header, Request, StatusCode},
};
use hmac::Hmac;
use muse_agent::{create_file_manager_router, FileManagerConfig};
use pbkdf2::pbkdf2;
use sha2::Sha256;
use std::fs;
use tempfile::tempdir;
use tower::ServiceExt;

fn compute_test_hash(pass: &str, salt: &[u8]) -> String {
    let mut derived_key = [0u8; 32];
    let _ = pbkdf2::<Hmac<Sha256>>(pass.as_bytes(), salt, 200_000, &mut derived_key);
    hex::encode(derived_key)
}

#[tokio::test]
async fn test_file_manager_auth_and_directory_listing() {
    let dir = tempdir().unwrap();
    let root_path = dir.path().to_str().unwrap().to_string();

    // Create files and directories for testing
    fs::create_dir(dir.path().join("subfolder")).unwrap();
    fs::write(dir.path().join("subfolder").join("test.txt"), "hello world").unwrap();
    fs::write(
        dir.path().join("readme.md"),
        "# Title Header\nSome markdown text",
    )
    .unwrap();

    let salt_bytes = b"testsalt12345678";
    let salt_hex = hex::encode(salt_bytes);
    let hash_hex = compute_test_hash("mypassword", salt_bytes);

    let config = FileManagerConfig {
        root_dir: root_path,
        auth_user: "testuser".to_string(),
        auth_salt: salt_hex,
        auth_hash: hash_hex,
        ..FileManagerConfig::default()
    };

    let app = create_file_manager_router(config);

    // 1. Unauthenticated request -> 401 Unauthorized
    let req = Request::builder().uri("/").body(Body::empty()).unwrap();
    let resp = app.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(
        resp.headers().get(header::WWW_AUTHENTICATE).unwrap(),
        r#"Basic realm="File Server""#
    );

    // 2. Invalid credentials -> 401 Unauthorized
    let wrong_auth = format!(
        "Basic {}",
        base64::Engine::encode(
            &base64::engine::general_purpose::STANDARD,
            "testuser:wrongpass"
        )
    );
    let req = Request::builder()
        .uri("/")
        .header(header::AUTHORIZATION, wrong_auth)
        .body(Body::empty())
        .unwrap();
    let resp = app.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);

    // 3. Valid credentials -> 200 OK with directory listing
    let valid_auth = format!(
        "Basic {}",
        base64::Engine::encode(
            &base64::engine::general_purpose::STANDARD,
            "testuser:mypassword"
        )
    );
    let req = Request::builder()
        .uri("/")
        .header(header::AUTHORIZATION, valid_auth.clone())
        .body(Body::empty())
        .unwrap();
    let resp = app.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body_bytes = axum::body::to_bytes(resp.into_body(), 1024 * 1024)
        .await
        .unwrap();
    let body_str = String::from_utf8(body_bytes.to_vec()).unwrap();
    assert!(body_str.contains("File Server"));
    assert!(body_str.contains("subfolder"));
    assert!(body_str.contains("readme.md"));
    // Ensure URLs are clean and not double-encoded (%2E or %2D)
    assert!(body_str.contains(r#"href="/readme.md""#));
    assert!(body_str.contains(r#"href="/subfolder/""#));

    // 4. Markdown rendering request -> 200 OK rendered HTML
    let req = Request::builder()
        .uri("/readme.md")
        .header(header::AUTHORIZATION, valid_auth.clone())
        .body(Body::empty())
        .unwrap();
    let resp = app.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body_bytes = axum::body::to_bytes(resp.into_body(), 1024 * 1024)
        .await
        .unwrap();
    let body_str = String::from_utf8(body_bytes.to_vec()).unwrap();
    assert!(body_str.contains("Title Header"));
    assert!(body_str.contains("Raw"));

    // 5. Raw markdown request -> 200 OK with ?raw=1 query
    let req = Request::builder()
        .uri("/readme.md?raw=1")
        .header(header::AUTHORIZATION, valid_auth.clone())
        .body(Body::empty())
        .unwrap();
    let resp = app.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body_bytes = axum::body::to_bytes(resp.into_body(), 1024 * 1024)
        .await
        .unwrap();
    let body_str = String::from_utf8(body_bytes.to_vec()).unwrap();
    assert!(body_str.contains("Render"));
    assert!(body_str.contains("# Title Header"));

    // 6. Non-existent file request -> 404 Not Found
    let req = Request::builder()
        .uri("/does-not-exist.txt")
        .header(header::AUTHORIZATION, valid_auth.clone())
        .body(Body::empty())
        .unwrap();
    let resp = app.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

#[test]
fn test_safe_resolve_prevents_path_traversal() {
    let dir = tempdir().unwrap();
    let root = dir.path();

    fs::create_dir(root.join("docs")).unwrap();
    fs::write(root.join("docs").join("hello.txt"), "content").unwrap();

    // Normal safe resolve
    let resolved = muse_agent::file_manager::auth::safe_resolve(root, "docs/hello.txt");
    assert!(resolved.is_some());
    assert_eq!(
        resolved.unwrap(),
        root.join("docs").join("hello.txt").canonicalize().unwrap()
    );

    // Path traversal attempt with ..
    let traversal = muse_agent::file_manager::auth::safe_resolve(root, "../../etc/passwd");
    assert!(traversal.is_none());

    // Path traversal attempt with sneaky relative path
    let traversal2 = muse_agent::file_manager::auth::safe_resolve(root, "docs/../../");
    assert!(traversal2.is_none() || traversal2.unwrap() == root.canonicalize().unwrap());
}
