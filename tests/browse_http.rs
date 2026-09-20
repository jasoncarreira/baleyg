//! Offline catalog and cached-source sequence API checks.
use axum::{
    Router,
    body::{Body, to_bytes},
    http::{Request, StatusCode},
};
use baleyg::{
    http,
    indexer::{IndexOptions, index_workspace},
    model::*,
    store::Store,
};
use serde_json::{Value, json};
use std::sync::{Arc, atomic::AtomicBool};
use tower::ServiceExt;
const TOKEN: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
fn setup() -> (tempfile::TempDir, Store, Graph, Router) {
    let dir = tempfile::tempdir().unwrap();
    let workspace = dir.path().join("workspace");
    std::fs::create_dir(&workspace).unwrap();
    std::fs::write(workspace.join("a.js"), "function check(x) { return x > 0; }\nfunction seed(x) { function nested() { side(); } if (x) external(x); return x; }\nclass Box { get value() { audit(); return this.x; } method() { save(); } }\n").unwrap();
    std::fs::write(workspace.join("z.js"), "// no methods\n").unwrap();
    let options = IndexOptions::new(workspace.clone());
    let graph = index_workspace(&options, &Arc::new(AtomicBool::new(false)), |_| {}).unwrap();
    let store = Store::open(&dir.path().join("state"), &workspace).unwrap();
    store
        .publish(&graph, Some(0), &Arc::new(AtomicBool::new(false)))
        .unwrap();
    let app = http::router(
        http::new(
            store.clone(),
            options,
            TOKEN.into(),
            "127.0.0.1:7331".parse().unwrap(),
        )
        .unwrap(),
    );
    (dir, store, graph, app)
}
async fn call(app: &Router, method: &str, path: &str, body: Value) -> (StatusCode, Value) {
    let request = Request::builder()
        .method(method)
        .uri(path)
        .header("host", "127.0.0.1:7331")
        .header("authorization", format!("Bearer {TOKEN}"))
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))
        .unwrap();
    let response = app.clone().oneshot(request).await.unwrap();
    assert_eq!(response.headers()["cache-control"], "no-store");
    let status = response.status();
    let bytes = to_bytes(response.into_body(), 4 * 1024 * 1024)
        .await
        .unwrap();
    (status, serde_json::from_slice(&bytes).unwrap())
}
#[tokio::test]
async fn catalog_pages_inline_methods_and_conservative_checks() {
    let (_dir, _store, graph, app) = setup();
    let (status, page) = call(&app, "GET", "/api/files?revision=1&limit=1", Value::Null).await;
    assert_eq!(status, 200);
    assert_eq!(page["revision"], 1);
    assert_eq!(page["items"][0]["path"], "a.js");
    assert_eq!(page["nextOffset"], 1);
    let expected = graph
        .nodes
        .iter()
        .filter(|n| n.path == "a.js" && matches!(n.kind, SymbolKind::Function | SymbolKind::Method))
        .count();
    assert_eq!(page["items"][0]["methodCount"], expected);
    let (_, page) = call(
        &app,
        "GET",
        "/api/files?revision=1&limit=1&offset=1",
        Value::Null,
    )
    .await;
    assert_eq!(page["items"][0]["path"], "z.js");
    assert_eq!(page["nextOffset"], Value::Null);
    let (status, methods) = call(
        &app,
        "GET",
        "/api/methods?revision=1&path=a.js",
        Value::Null,
    )
    .await;
    assert_eq!(status, 200);
    let items = methods["items"].as_array().unwrap();
    assert_eq!(items.len(), expected);
    assert!(
        items
            .iter()
            .all(|m| matches!(m["symbol"]["kind"].as_str(), Some("function" | "method")))
    );
    for name in ["check", "nested", "method", "value"] {
        assert!(
            items
                .iter()
                .any(|m| m["symbol"]["name"] == name && m["consequential"] == true),
            "missing {name}: {items:?}"
        );
    }
    assert_eq!(
        call(&app, "GET", "/api/methods?path=z.js", Value::Null)
            .await
            .1["items"],
        json!([])
    );
}
#[tokio::test]
async fn strict_errors_revisions_and_auth() {
    let (_dir, store, graph, app) = setup();
    for path in [
        "/api/files?limit=0",
        "/api/files?limit=201",
        "/api/files?extra=1",
        "/api/files?revision=no",
        "/api/methods?path=../a.js",
        "/api/methods?path=a.js&extra=1",
    ] {
        assert_eq!(call(&app, "GET", path, Value::Null).await.0, 422, "{path}");
    }
    assert_eq!(
        call(&app, "GET", "/api/methods?path=missing.js", Value::Null)
            .await
            .0,
        404
    );
    let seed = &graph.nodes.iter().find(|n| n.name == "seed").unwrap().id;
    for body in [
        json!({"seed":seed}),
        json!({"seed":seed,"expectedRevision":1,"extra":true}),
        json!({"seed":seed,"expectedRevision":1,"showAll":"yes"}),
        json!({"seed":"","expectedRevision":1}),
    ] {
        assert_eq!(call(&app, "POST", "/api/sequence", body).await.0, 422);
    }
    assert_eq!(
        call(
            &app,
            "POST",
            "/api/sequence",
            json!({"seed":"missing","expectedRevision":1})
        )
        .await
        .0,
        404
    );
    for symbol in graph
        .nodes
        .iter()
        .filter(|n| matches!(n.kind, SymbolKind::Class | SymbolKind::Module))
    {
        assert_eq!(
            call(
                &app,
                "POST",
                "/api/sequence",
                json!({"seed":symbol.id,"expectedRevision":1})
            )
            .await
            .0,
            422
        );
    }
    store
        .publish(&graph, Some(1), &Arc::new(AtomicBool::new(false)))
        .unwrap();
    for path in ["/api/files?revision=1", "/api/methods?revision=1&path=a.js"] {
        assert_eq!(call(&app, "GET", path, Value::Null).await.0, 409);
    }
    assert_eq!(
        call(
            &app,
            "POST",
            "/api/sequence",
            json!({"seed":seed,"expectedRevision":1})
        )
        .await
        .0,
        409
    );
    for path in ["/api/files", "/api/methods?path=a.js", "/api/sequence"] {
        let request = Request::builder()
            .uri(path)
            .header("host", "127.0.0.1:7331")
            .body(Body::empty())
            .unwrap();
        assert_eq!(app.clone().oneshot(request).await.unwrap().status(), 401);
    }
}
#[tokio::test]
async fn cached_snapshot_is_independent_of_workspace_and_raw_query() {
    let (dir, _store, graph, app) = setup();
    let seed = &graph.nodes.iter().find(|n| n.name == "seed").unwrap().id;
    let query = json!({"seed":seed});
    let raw_before = call(&app, "POST", "/api/query", query.clone()).await;
    let request = json!({"seed":seed,"expectedRevision":1});
    let sequence_before = call(&app, "POST", "/api/sequence", request.clone()).await;
    assert_eq!(sequence_before.0, 200, "{:?}", sequence_before.1);
    std::fs::remove_dir_all(dir.path().join("workspace")).unwrap();
    assert_eq!(
        call(&app, "POST", "/api/sequence", request).await,
        sequence_before
    );
    assert_eq!(call(&app, "POST", "/api/query", query).await, raw_before);
    assert_eq!(
        call(&app, "GET", "/api/files?revision=1", Value::Null)
            .await
            .1["items"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    assert_eq!(sequence_before.1["revision"], 1);
    assert_eq!(sequence_before.1["seed"]["id"], *seed);
}
