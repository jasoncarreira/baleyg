mod common;
use axum::{
    Router,
    body::{Body, to_bytes},
    http::Request,
};
use baleyg::{
    http,
    indexer::{IndexOptions, index_workspace},
    model::{CancelFlag, Graph},
    store::Store,
};
use serde_json::{Value, json};
use std::sync::{Arc, atomic::AtomicBool};
use tower::ServiceExt;
const TOKEN: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
fn fixture() -> (tempfile::TempDir, Store, Graph, Router, String) {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("workspace");
    std::fs::create_dir(&root).unwrap();
    std::fs::write(root.join("a.js"), "function seed() { save(); }").unwrap();
    let options = IndexOptions::new(root.clone());
    let cancel: CancelFlag = Arc::new(AtomicBool::new(false));
    let graph = index_workspace(&options, &cancel, |_| {}).unwrap();
    let seed = graph
        .nodes
        .iter()
        .find(|node| node.name == "seed")
        .unwrap()
        .id
        .clone();
    let store = crate::common::open_store(&temp.path().join("state"), &root).unwrap();
    let app = http::router(
        http::new(
            store.clone(),
            options,
            TOKEN.into(),
            "127.0.0.1:7331".parse().unwrap(),
        )
        .unwrap(),
    );
    (temp, store, graph, app, seed)
}
async fn call(app: &Router, method: &str, path: &str, body: Value) -> (u16, Value) {
    let req = Request::builder()
        .method(method)
        .uri(path)
        .header("host", "127.0.0.1:7331")
        .header("authorization", format!("Bearer {TOKEN}"))
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))
        .unwrap();
    let response = app.clone().oneshot(req).await.unwrap();
    let code = response.status().as_u16();
    let bytes = to_bytes(response.into_body(), 2 * 1024 * 1024)
        .await
        .unwrap();
    (code, serde_json::from_slice(&bytes).unwrap_or(Value::Null))
}
#[tokio::test]
async fn durable_crud_matrix() {
    let (temp, store, graph, app, seed) = fixture();
    let durable = temp.path().join("state/data/workspaces");
    assert_eq!(
        call(&app, "GET", "/api/views", Value::Null).await.1,
        json!([])
    );
    assert_eq!(
        call(&app, "GET", "/api/annotations", Value::Null).await.1,
        json!([])
    );
    assert_eq!(
        call(&app, "DELETE", "/api/views/absent", Value::Null)
            .await
            .0,
        204
    );
    assert!(
        !durable.exists(),
        "absent reads/deletes do not create durable records"
    );
    let view = json!({"id":"view","title":"Keep","query":{"seed":seed},"pins":{},"hidden":[]});
    let canonical = serde_json::to_value(
        serde_json::from_value::<baleyg::model::SavedView>(view.clone()).unwrap(),
    )
    .unwrap();
    let (status, saved) = call(&app, "PUT", "/api/views/view", view.clone()).await;
    assert_eq!(status, 200, "{saved}");
    assert_eq!(saved["view"], canonical);
    assert_eq!(
        call(&app, "GET", "/api/views/view", Value::Null).await.1["view"],
        canonical
    );
    let note = json!({"id":"note","nodeId":seed,"body":"Preserved"});
    let (status, saved) = call(&app, "PUT", "/api/annotations/note", note.clone()).await;
    assert_eq!(status, 200, "{saved}");
    assert_eq!(saved["annotation"], note);
    assert_eq!(
        call(&app, "GET", "/api/annotations", Value::Null).await.1[0]["annotation"],
        note
    );
    assert_eq!(
        call(&app, "PUT", "/api/views/wrong", view.clone()).await.0,
        400
    );
    assert_eq!(
        call(&app, "DELETE", "/api/views/view", Value::Null).await.0,
        204
    );
    assert_eq!(
        call(&app, "DELETE", "/api/annotations/note", Value::Null)
            .await
            .0,
        204
    );
    assert_eq!(
        call(&app, "GET", "/api/views", Value::Null).await.1,
        json!([])
    );
    let record = std::fs::read_dir(&durable)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .find(|p| p.is_dir())
        .unwrap();
    assert!(
        record.join("workspace.db").exists(),
        "empty record remains durable"
    );
    store
        .publish(
            &graph,
            &store.leader().unwrap(),
            store.status().unwrap().revision,
            &Arc::new(AtomicBool::new(false)),
        )
        .unwrap();
}
#[tokio::test]
async fn workspace_root_changed() {
    let (temp, store, _graph, app, _seed) = fixture();
    let original = temp.path().join("workspace");
    let moved = temp.path().join("moved");
    std::fs::rename(&original, &moved).unwrap();
    std::fs::create_dir(&original).unwrap();
    for (method, path, body) in [
        ("GET", "/api/status", Value::Null),
        ("GET", "/api/tree", Value::Null),
        ("GET", "/api/files", Value::Null),
        ("GET", "/api/views", Value::Null),
    ] {
        let (code, result) = call(&app, method, path, body).await;
        assert_eq!(code, 409, "{path}: {result}");
        assert_eq!(result["error"]["code"], "root_changed");
    }
    assert!(store.status().is_err());
}
