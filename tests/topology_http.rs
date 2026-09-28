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
    std::fs::write(
        root.join("a.js"),
        "function seed() { other(); } function other() {}",
    )
    .unwrap();
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
fn pinned(route: &str, pin: &baleyg::model::IndexPin) -> String {
    let separator = if route.contains('?') { '&' } else { '?' };
    format!(
        "{route}{separator}indexGeneration={}&indexRevision={}",
        pin.index_generation, pin.index_revision
    )
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

    let baseline = store.index_baseline().unwrap();
    let pin = publish_bundle(
        &store,
        &graph,
        &temp.path().join("workspace"),
        &store.leader().unwrap(),
        baseline,
        &Arc::new(AtomicBool::new(false)),
    )
    .unwrap();
    let view = json!({"id":"view","title":"Keep","query":{"seed":seed},"pins":{},"hidden":[]});
    assert_eq!(
        call(&app, "PUT", "/api/views/view", view.clone()).await.0,
        400
    );
    let (status, saved) = call(&app, "PUT", &pinned("/api/views/view", &pin), view.clone()).await;
    assert_eq!(status, 200, "{saved}");
    assert_eq!(saved["view"]["id"], "view");
    assert_eq!(saved["view"]["title"], "Keep");
    assert_eq!(saved["view"]["query"], view["query"]);
    assert_eq!(saved["view"]["anchor"]["syntaxId"], seed);
    assert_eq!(saved["indexGeneration"], pin.index_generation.to_string());
    assert_eq!(saved["indexRevision"], pin.index_revision);
    assert_eq!(saved["attachment"]["availability"], "ready");
    assert_eq!(saved["attachment"]["result"]["status"], "attached");
    assert_eq!(
        call(&app, "GET", "/api/views/view", Value::Null).await.1["view"],
        saved["view"]
    );

    let note = json!({"id":"note","nodeId":seed,"body":"Preserved"});
    assert_eq!(
        call(&app, "PUT", "/api/annotations/note", note.clone())
            .await
            .0,
        400
    );
    let (status, saved_note) = call(
        &app,
        "PUT",
        &pinned("/api/annotations/note", &pin),
        note.clone(),
    )
    .await;
    assert_eq!(status, 200, "{saved_note}");
    assert_eq!(saved_note["annotation"]["id"], "note");
    assert_eq!(saved_note["annotation"]["nodeId"], seed);
    assert_eq!(saved_note["annotation"]["body"], "Preserved");
    assert_eq!(saved_note["annotation"]["anchor"]["syntaxId"], seed);
    assert_eq!(saved_note["attachment"]["availability"], "ready");
    assert_eq!(
        call(&app, "GET", "/api/annotations", Value::Null).await.1[0]["annotation"],
        saved_note["annotation"]
    );
    assert_eq!(
        call(&app, "PUT", &pinned("/api/views/wrong", &pin), view.clone())
            .await
            .0,
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
}

#[tokio::test]
async fn query_optional_pin_matrix() {
    let (temp, store, graph, app, seed) = fixture();
    let root = temp.path().join("workspace");
    let pin = publish_bundle(
        &store,
        &graph,
        &root,
        &store.leader().unwrap(),
        store.index_baseline().unwrap(),
        &Arc::new(AtomicBool::new(false)),
    )
    .unwrap();
    let body = json!({"seed":seed});

    let (status, unpinned) = call(&app, "POST", "/api/query", body.clone()).await;
    assert_eq!(status, 200, "{unpinned}");
    assert_eq!(unpinned["revision"], json!(pin));
    let (status, current) = call(&app, "POST", &pinned("/api/query", &pin), body.clone()).await;
    assert_eq!(status, 200, "{current}");
    assert_eq!(current["revision"], json!(pin));

    for route in [
        format!("/api/query?indexGeneration={}", pin.index_generation),
        format!("/api/query?indexRevision={}", pin.index_revision),
        "/api/query?1".to_owned(),
        format!(
            "/api/query?indexGeneration={}&indexRevision={}&extra=true",
            pin.index_generation, pin.index_revision
        ),
    ] {
        assert_eq!(
            call(&app, "POST", &route, body.clone()).await.0,
            400,
            "{route}"
        );
    }
    let wrong_generation = format!(
        "/api/query?indexGeneration={}&indexRevision={}",
        uuid::Uuid::new_v4(),
        pin.index_revision
    );
    assert_eq!(
        call(&app, "POST", &wrong_generation, body.clone()).await.0,
        409
    );

    let next = publish_bundle(
        &store,
        &graph,
        &root,
        &store.leader().unwrap(),
        pin,
        &Arc::new(AtomicBool::new(false)),
    )
    .unwrap();
    let (status, stale) = call(&app, "POST", &pinned("/api/query", &pin), body.clone()).await;
    assert_eq!(status, 409, "{stale}");
    let (status, current) = call(&app, "POST", &pinned("/api/query", &next), body.clone()).await;
    assert_eq!(status, 200, "{current}");
    assert_eq!(current["revision"], json!(next));
    let (status, ordinary) = call(&app, "POST", "/api/query", body).await;
    assert_eq!(status, 200, "{ordinary}");
    assert_eq!(ordinary["revision"], json!(next));
}

#[tokio::test]
async fn saved_read_pin_and_ownership_matrix() {
    let (temp, store, graph, app, seed) = fixture();
    let root = temp.path().join("workspace");
    let other = graph
        .nodes
        .iter()
        .find(|node| node.name == "other")
        .unwrap()
        .id
        .clone();
    let pin = publish_bundle(
        &store,
        &graph,
        &root,
        &store.leader().unwrap(),
        store.index_baseline().unwrap(),
        &Arc::new(AtomicBool::new(false)),
    )
    .unwrap();
    let view = json!({"id":"owned","title":"Original","query":{"seed":seed}});
    let (status, created) =
        call(&app, "PUT", &pinned("/api/views/owned", &pin), view.clone()).await;
    assert_eq!(status, 200, "{created}");
    let view_anchor = created["view"]["anchor"].clone();
    assert_eq!(created["attachment"]["result"]["targetId"], seed);

    let note = json!({"id":"owned-note","nodeId":seed,"body":"body","title":"Heading"});
    let (status, created_note) = call(
        &app,
        "PUT",
        &pinned("/api/annotations/owned-note", &pin),
        note.clone(),
    )
    .await;
    assert_eq!(status, 200, "{created_note}");
    let note_anchor = created_note["annotation"]["anchor"].clone();

    for route in [
        format!("/api/views?indexGeneration={}", pin.index_generation),
        format!("/api/views?indexRevision={}", pin.index_revision),
        "/api/views?1".to_owned(),
        format!("/api/annotations?indexRevision={}", pin.index_revision),
    ] {
        assert_eq!(
            call(&app, "GET", &route, Value::Null).await.0,
            400,
            "{route}"
        );
    }
    let (status, listed) = call(&app, "GET", &pinned("/api/views", &pin), Value::Null).await;
    assert_eq!(status, 200, "{listed}");
    assert_eq!(listed[0]["view"]["anchor"], view_anchor);
    assert_eq!(
        listed[0]["indexGeneration"],
        pin.index_generation.to_string()
    );
    assert_eq!(listed[0]["indexRevision"], pin.index_revision);
    let (status, opened) = call(&app, "GET", &pinned("/api/views/owned", &pin), Value::Null).await;
    assert_eq!(status, 200, "{opened}");
    assert_eq!(opened["view"]["anchor"], view_anchor);

    let replacement = json!({"id":"owned","title":"No","query":{"seed":other}});
    assert_eq!(
        call(&app, "PUT", &pinned("/api/views/owned", &pin), replacement)
            .await
            .0,
        400
    );
    let note_replacement = json!({"id":"owned-note","nodeId":other,"body":"No"});
    assert_eq!(
        call(
            &app,
            "PUT",
            &pinned("/api/annotations/owned-note", &pin),
            note_replacement
        )
        .await
        .0,
        400
    );

    let offered_anchor = json!({
        "id":"owned",
        "title":"No",
        "query":{"seed":seed},
        "anchor":view_anchor
    });
    assert_eq!(
        call(
            &app,
            "PUT",
            &pinned("/api/views/owned", &pin),
            offered_anchor
        )
        .await
        .0,
        400
    );
    let offered_metadata = json!({
        "id":"owned-note",
        "nodeId":seed,
        "body":"No",
        "attachment":{"availability":"ready"}
    });
    assert_eq!(
        call(
            &app,
            "PUT",
            &pinned("/api/annotations/owned-note", &pin),
            offered_metadata
        )
        .await
        .0,
        400
    );

    let edit = json!({"id":"owned","title":"Edited","query":{"seed":seed}});
    let (status, edited) = call(&app, "PUT", &pinned("/api/views/owned", &pin), edit).await;
    assert_eq!(status, 200, "{edited}");
    assert_eq!(edited["view"]["anchor"], created["view"]["anchor"]);
    let note_edit = json!({"id":"owned-note","nodeId":seed,"body":"edited"});
    let (status, edited_note) = call(
        &app,
        "PUT",
        &pinned("/api/annotations/owned-note", &pin),
        note_edit,
    )
    .await;
    assert_eq!(status, 200, "{edited_note}");
    assert_eq!(edited_note["annotation"]["anchor"], note_anchor);
    assert_eq!(
        edited_note["annotation"]["title"], "Heading",
        "omitted optional title preserves the stored title"
    );

    let legacy: baleyg::model::SavedView = serde_json::from_value(json!({
        "id":"legacy","title":"Legacy","query":{"seed":seed}
    }))
    .unwrap();
    store.put_view(&legacy).unwrap();
    let (status, legacy_state) =
        call(&app, "GET", &pinned("/api/views/legacy", &pin), Value::Null).await;
    assert_eq!(status, 200, "{legacy_state}");
    assert!(legacy_state["view"].get("anchor").is_none());
    assert_eq!(legacy_state["attachment"]["availability"], "anchorless");
    assert!(
        legacy_state["orphanedIds"]
            .as_array()
            .unwrap()
            .iter()
            .any(|id| id == &json!(seed))
    );
    let legacy_edit = json!({"id":"legacy","title":"Still legacy","query":{"seed":seed}});
    let (status, legacy_edited) =
        call(&app, "PUT", &pinned("/api/views/legacy", &pin), legacy_edit).await;
    assert_eq!(status, 200, "{legacy_edited}");
    assert!(legacy_edited["view"].get("anchor").is_none());
    assert_eq!(legacy_edited["attachment"]["availability"], "anchorless");

    let next = publish_bundle(
        &store,
        &graph,
        &root,
        &store.leader().unwrap(),
        pin,
        &Arc::new(AtomicBool::new(false)),
    )
    .unwrap();
    for route in [pinned("/api/views", &pin), pinned("/api/annotations", &pin)] {
        let (status, error) = call(&app, "GET", &route, Value::Null).await;
        assert_eq!(status, 409, "{route}: {error}");
    }
    assert_eq!(
        call(&app, "PUT", &pinned("/api/views/owned", &pin), view)
            .await
            .0,
        409
    );
    let (status, current) = call(&app, "GET", &pinned("/api/views", &next), Value::Null).await;
    assert_eq!(status, 200, "{current}");
    assert!(current.as_array().unwrap().len() >= 2);
}

#[tokio::test]
async fn schema5_is_not_native_anchor_evidence() {
    let (_temp, store, _graph, app, seed) = fixture();
    let legacy_pin = store.index_baseline().unwrap();
    let legacy: baleyg::model::SavedView = serde_json::from_value(json!({
        "id":"legacy","title":"Legacy","query":{"seed":seed}
    }))
    .unwrap();
    store.put_view(&legacy).unwrap();

    let (status, response) = call(&app, "GET", "/api/views/legacy", Value::Null).await;
    assert_eq!(status, 200, "{response}");
    assert_eq!(response["view"]["query"]["seed"], seed);
    assert!(response["view"].get("anchor").is_none());
    assert!(response["indexGeneration"].is_null());
    assert!(response["indexRevision"].is_null());
    assert_eq!(response["attachment"]["availability"], "indexUnavailable");
    assert!(response["attachment"]["result"].is_null());

    let (status, error) = call(
        &app,
        "GET",
        &pinned("/api/views/legacy", &legacy_pin),
        Value::Null,
    )
    .await;
    assert_eq!(status, 409, "{error}");
    let body = json!({"id":"new","title":"No capture","query":{"seed":seed}});
    let (status, error) = call(&app, "PUT", &pinned("/api/views/new", &legacy_pin), body).await;
    assert_eq!(status, 409, "{error}");
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

fn publish_bundle(
    store: &baleyg::store::Store,
    graph: &baleyg::model::Graph,
    workspace: &std::path::Path,
    leader: &baleyg::store::topology::LeaderGuard,
    expected: baleyg::model::IndexPin,
    cancel: &baleyg::model::CancelFlag,
) -> anyhow::Result<baleyg::model::IndexPin> {
    let (indexed, native, capture) = baleyg::indexer::index_workspace_bundle(
        &baleyg::indexer::IndexOptions::new(workspace.to_owned()),
        store.root_id(),
        cancel,
        |_| {},
    )?;
    assert_eq!(
        &indexed, graph,
        "published graph must match captured source"
    );
    store.publish_native(&indexed, &capture, &native, leader, expected, cancel)
}
