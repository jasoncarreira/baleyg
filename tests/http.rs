use axum::{
    Router,
    body::{Body, to_bytes},
    http::{Request, StatusCode},
};
use baleyg::{auth, http, indexer::IndexOptions, model::*, store::Store};
use serde_json::{Value, json};
use std::sync::{Arc, atomic::AtomicBool};
use tower::ServiceExt;
const TOKEN: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
fn setup() -> (tempfile::TempDir, Store, Arc<http::DaemonState>, Router) {
    let dir = tempfile::tempdir().unwrap();
    let workspace = dir.path().join("workspace");
    std::fs::create_dir(&workspace).unwrap();
    let store = Store::open(&dir.path().join("state"), &workspace).unwrap();
    let state = http::new(
        store.clone(),
        IndexOptions::new(workspace),
        TOKEN.into(),
        "127.0.0.1:7331".parse().unwrap(),
    )
    .unwrap();
    let router = http::router(state.clone());
    (dir, store, state, router)
}
async fn call(app: &Router, method: &str, path: &str, body: Value) -> (StatusCode, Value) {
    let req = Request::builder()
        .method(method)
        .uri(path)
        .header("host", "127.0.0.1:7331")
        .header("authorization", format!("Bearer {TOKEN}"))
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))
        .unwrap();
    let response = app.clone().oneshot(req).await.unwrap();
    let code = response.status();
    let bytes = to_bytes(response.into_body(), 2 * 1024 * 1024)
        .await
        .unwrap();
    (code, serde_json::from_slice(&bytes).unwrap_or(Value::Null))
}
#[tokio::test]
async fn guards_public_and_private() {
    let (_d, _store, _state, app) = setup();
    for (path, host, origin, token, want) in [
        ("/healthz", "127.0.0.1:7331", None, None, 200),
        ("/", "localhost:7331", None, None, 200),
        ("/api/status", "127.0.0.1:7331", None, None, 401),
        ("/api/status", "127.0.0.1:7331", None, Some(TOKEN), 200),
        ("/healthz", "evil.example", None, None, 403),
        (
            "/",
            "127.0.0.1:7331",
            Some("http://evil.example"),
            None,
            403,
        ),
        (
            "/api/status",
            "127.0.0.1:7331",
            Some("null"),
            Some(TOKEN),
            403,
        ),
        (
            "/api/status",
            "localhost:7331",
            Some("http://localhost:7331"),
            Some(TOKEN),
            200,
        ),
    ] {
        let mut req = Request::builder().uri(path).header("host", host);
        if let Some(o) = origin {
            req = req.header("origin", o)
        }
        if let Some(t) = token {
            req = req.header("authorization", format!("Bearer {t}"))
        }
        let response = app
            .clone()
            .oneshot(req.body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status().as_u16(), want, "{path} {host}");
        assert_eq!(response.headers()["cache-control"], "no-store");
        assert!(
            response.headers()["content-security-policy"]
                .to_str()
                .unwrap()
                .contains("frame-ancestors 'none'")
        );
        assert!(
            !response
                .headers()
                .contains_key("access-control-allow-origin")
        );
    }
    let req = Request::builder()
        .method("OPTIONS")
        .uri("/api/index")
        .header("host", "127.0.0.1:7331")
        .header("origin", "https://evil.example")
        .body(Body::empty())
        .unwrap();
    assert_eq!(app.oneshot(req).await.unwrap().status(), 403);
}
#[tokio::test]
async fn validation_and_limit() {
    let (_d, _store, _state, app) = setup();
    for path in [
        "/api/source?path=../secret",
        "/api/source?path=%2Fetc%2Fpasswd",
        "/api/source?path=C:%5Csecret",
        "/api/source?path=a%2F..%2Fb",
    ] {
        assert_eq!(call(&app, "GET", path, Value::Null).await.0, 400)
    }
    assert_eq!(
        call(&app, "POST", "/api/query", json!({"seed":"x","depth":6}))
            .await
            .0,
        400
    );
    assert_eq!(
        call(
            &app,
            "PUT",
            "/api/views/test",
            json!({"id":"other","title":"v","query":{"seed":"x"}})
        )
        .await
        .0,
        400
    );
    assert_eq!(
        call(&app, "POST", "/api/index", json!({"workspaceRoot":"/tmp"}))
            .await
            .0,
        400
    );
    assert_eq!(
        call(&app, "POST", "/api/index", json!({"expectedRevision":9}))
            .await
            .0,
        409
    );
    let req = Request::builder()
        .method("POST")
        .uri("/api/index")
        .header("host", "127.0.0.1:7331")
        .header("authorization", format!("Bearer {TOKEN}"))
        .body(Body::from(vec![b' '; 1024 * 1024 + 1]))
        .unwrap();
    assert_eq!(app.oneshot(req).await.unwrap().status(), 413);
}
#[tokio::test]
async fn source_is_snapshot_and_revision_checked() {
    let (_d, store, _state, app) = setup();
    let mut graph = Graph::default();
    graph.files.push(SourceFile {
        path: "a.js".into(),
        hash: "hash".into(),
        language: "javascript".into(),
        text: "cached secret-free source".into(),
    });
    store
        .publish(&graph, Some(0), &Arc::new(AtomicBool::new(false)))
        .unwrap();
    let (code, body) = call(&app, "GET", "/api/source?path=a.js&revision=1", Value::Null).await;
    assert_eq!(code, 200);
    assert_eq!(body["revision"], 1);
    assert_eq!(body["file"]["text"], "cached secret-free source");
    assert_eq!(
        call(&app, "GET", "/api/source?path=a.js&revision=0", Value::Null)
            .await
            .0,
        409
    );
    assert_eq!(
        call(&app, "GET", "/api/source?path=absent.js", Value::Null)
            .await
            .0,
        404
    );
}
#[tokio::test]
async fn jobs_publish_and_cancel() {
    let (_d, store, state, app) = setup();
    let (code, job) = call(&app, "POST", "/api/index", json!({})).await;
    assert_eq!(code, 202);
    let id = job["id"].as_str().unwrap();
    let completed = tokio::time::timeout(std::time::Duration::from_secs(10), async {
        loop {
            let (_, j) = call(&app, "GET", &format!("/api/jobs/{id}"), Value::Null).await;
            if !j["finishedAt"].is_null() {
                break j;
            }
            tokio::task::yield_now().await
        }
    })
    .await
    .unwrap();
    assert_eq!(completed["state"], "completed");
    assert_eq!(store.status().unwrap().revision, 1);
    let (_, cancelled) = call(&app, "POST", &format!("/api/jobs/{id}/cancel"), Value::Null).await;
    assert_eq!(cancelled["state"], "completed");
    assert_eq!(store.status().unwrap().revision, 1);
    state.cancel_active();
}
#[test]
fn token_security() {
    use std::os::unix::fs::{PermissionsExt, symlink};
    let d = tempfile::tempdir().unwrap();
    let path = d.path().join("token");
    let token = auth::load_or_create_token(&path).unwrap();
    assert!(auth::valid_token(&token));
    assert_eq!(auth::load_or_create_token(&path).unwrap(), token);
    assert_eq!(
        std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
        0o600
    );
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
    assert!(auth::load_or_create_token(&path).is_err());
    let link = d.path().join("link");
    symlink(&path, &link).unwrap();
    assert!(auth::load_or_create_token(&link).is_err());
}
#[test]
fn rejects_bad_startup() {
    let (_d, store, _state, _app) = setup();
    assert!(
        http::new(
            store.clone(),
            IndexOptions::new(".".into()),
            TOKEN.into(),
            "0.0.0.0:7331".parse().unwrap()
        )
        .is_err()
    );
    assert!(
        http::new(
            store,
            IndexOptions::new(".".into()),
            "bad".into(),
            "127.0.0.1:7331".parse().unwrap()
        )
        .is_err()
    );
}

#[tokio::test]
async fn active_job_cancellation_does_not_publish() {
    let (d, store, _state, app) = setup();
    let source = (0..15000)
        .map(|i| format!("function f{i}() {{ console.log({i}); }}\n"))
        .collect::<String>();
    std::fs::write(d.path().join("workspace/large.js"), source).unwrap();
    let (code, j) = call(&app, "POST", "/api/index", json!({})).await;
    assert_eq!(code, 202);
    let id = j["id"].as_str().unwrap();
    let (code, _) = call(&app, "POST", &format!("/api/jobs/{id}/cancel"), Value::Null).await;
    assert_eq!(code, 200);
    let terminal = tokio::time::timeout(std::time::Duration::from_secs(10), async {
        loop {
            let (_, j) = call(&app, "GET", &format!("/api/jobs/{id}"), Value::Null).await;
            if !j["finishedAt"].is_null() {
                break j;
            }
            tokio::task::yield_now().await
        }
    })
    .await
    .unwrap();
    assert_eq!(terminal["state"], "cancelled");
    assert_eq!(store.status().unwrap().revision, 0);
}

#[tokio::test]
async fn local_design_assets_preserve_same_origin_guards() {
    let (_d, _store, _state, app) = setup();
    for (path, mime) in [
        ("/shell.js", "text/javascript; charset=utf-8"),
        ("/fonts/jetbrains-mono-latin.woff2", "font/woff2"),
        ("/fonts/space-grotesk-latin.woff2", "font/woff2"),
        ("/fonts/JetBrainsMono-OFL.txt", "text/plain; charset=utf-8"),
        ("/fonts/SpaceGrotesk-OFL.txt", "text/plain; charset=utf-8"),
    ] {
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(path)
                    .header("host", "127.0.0.1:7331")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), 200);
        assert_eq!(response.headers()["content-type"], mime);
        assert_eq!(response.headers()["x-content-type-options"], "nosniff");
        let csp = response.headers()["content-security-policy"]
            .to_str()
            .unwrap();
        assert!(csp.contains("font-src 'self'"));
        assert!(!csp.contains("unsafe-inline"));
        let bytes = to_bytes(response.into_body(), 1024 * 1024).await.unwrap();
        if mime == "font/woff2" {
            assert_eq!(&bytes[..4], b"wOF2");
        } else if path.ends_with("OFL.txt") {
            assert!(String::from_utf8_lossy(&bytes).contains("SIL OPEN FONT LICENSE"));
        }
        let bad = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(path)
                    .header("host", "127.0.0.1:7331")
                    .header("origin", "https://evil.example")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(bad.status(), 403);
    }
    assert_eq!(
        call(&app, "GET", "/fonts/missing.woff2", Value::Null)
            .await
            .0,
        404
    );
}
