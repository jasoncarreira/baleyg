//! Rust file -> methods -> sequence on cached source, without executing the workspace.
use axum::{
    Router,
    body::{Body, to_bytes},
    http::Request,
};
use baleyg::{
    http,
    indexer::{IndexOptions, index_workspace},
    store::Store,
};
use serde_json::{Value, json};
use std::sync::{Arc, atomic::AtomicBool};
use tower::ServiceExt;
const TOKEN: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
async fn request(app: &Router, method: &str, path: &str, body: Value) -> (u16, Value) {
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method(method)
                .uri(path)
                .header("host", "127.0.0.1:7331")
                .header("authorization", format!("Bearer {TOKEN}"))
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status().as_u16();
    let bytes = to_bytes(response.into_body(), 4 * 1024 * 1024)
        .await
        .unwrap();
    (status, serde_json::from_slice(&bytes).unwrap())
}
#[tokio::test]
async fn rust_methods_sequence_and_source_survive_live_file_removal() {
    let temp = tempfile::tempdir().unwrap();
    let workspace = temp.path().join("workspace");
    std::fs::create_dir(&workspace).unwrap();
    std::fs::write(
        workspace.join("Cargo.toml"),
        "[package]\nname='untrusted_fixture'\nversion='0.1.0'\nbuild='build.rs'\n",
    )
    .unwrap();
    let sentinel = temp.path().join("MUST_NOT_RUN");
    std::fs::write(
        workspace.join("build.rs"),
        format!(
            "fn main() {{ std::fs::write({:?}, \"executed\").unwrap(); }}",
            sentinel.to_str().unwrap()
        ),
    )
    .unwrap();
    let text = "pub struct Counter; impl Counter { pub fn increment(&self, x: i32) -> i32 { if x > 0 { save(x); } x + 1 } } fn save(x: i32) { audit(x); }\n";
    std::fs::write(workspace.join("lib.rs"), text).unwrap();
    std::fs::write(
        workspace.join("helper.js"),
        "function helper() { notify(); }",
    )
    .unwrap();
    let options = IndexOptions::new(workspace.clone());
    let cancel = Arc::new(AtomicBool::new(false));
    let graph = index_workspace(&options, &cancel, |_| {}).unwrap();
    assert!(!sentinel.exists());
    assert!(
        graph
            .files
            .iter()
            .any(|f| f.path == "lib.rs" && f.language == "rust")
    );
    assert!(
        graph
            .files
            .iter()
            .any(|f| f.path == "helper.js" && f.language == "javascript")
    );
    let store = Store::open(&temp.path().join("state"), &workspace).unwrap();
    store.publish(&graph, Some(0), &cancel).unwrap();
    let app = http::router(
        http::new(
            store.clone(),
            options,
            TOKEN.into(),
            "127.0.0.1:7331".parse().unwrap(),
        )
        .unwrap(),
    );
    let (code, tree) = request(&app, "GET", "/api/tree", Value::Null).await;
    assert_eq!(code, 200);
    let entry = tree["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["name"] == "lib.rs")
        .unwrap();
    assert_eq!(entry["indexedPath"], "lib.rs");
    assert!(entry["unindexedReason"].is_null());
    let (code, methods) = request(
        &app,
        "GET",
        "/api/methods?path=lib.rs&revision=1",
        Value::Null,
    )
    .await;
    assert_eq!(code, 200);
    let method = methods["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["symbol"]["name"] == "increment")
        .unwrap();
    assert_eq!(method["symbol"]["kind"], "method");
    let seed = method["symbol"]["id"].as_str().unwrap();
    std::fs::remove_file(workspace.join("lib.rs")).unwrap();
    let (code, sequence) = request(
        &app,
        "POST",
        "/api/sequence",
        json!({"seed":seed,"expectedRevision":1}),
    )
    .await;
    assert_eq!(code, 200);
    assert_eq!(sequence["seed"]["id"], seed);
    assert!(!sequence["steps"].as_array().unwrap().is_empty());
    let measured = graph
        .calls
        .iter()
        .find(|c| c.caller == seed && c.callee_text == "save")
        .unwrap();
    assert!(sequence.to_string().contains(&measured.id));
    let (code, source) = request(
        &app,
        "GET",
        "/api/source?path=lib.rs&revision=1",
        Value::Null,
    )
    .await;
    assert_eq!(code, 200);
    assert_eq!(source["file"]["text"], text);
    store.publish(&graph, Some(1), &cancel).unwrap();
    assert_eq!(
        request(
            &app,
            "POST",
            "/api/sequence",
            json!({"seed":seed,"expectedRevision":1})
        )
        .await
        .0,
        409
    );
    assert!(!sentinel.exists());
}
