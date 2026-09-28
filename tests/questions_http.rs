//! HTTP protocol fixtures are offline; synthetic responses are not model quality evidence.
mod common;
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
#[path = "common/jev_wire.rs"]
mod jev_wire;
use jev_wire::decode_packet;
use std::sync::{Arc, atomic::AtomicBool};
use tower::ServiceExt;
const TOKEN: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
fn setup(padding: usize) -> (tempfile::TempDir, Store, Graph, Router, Value) {
    let dir = tempfile::tempdir().unwrap();
    let workspace = dir.path().join("workspace");
    std::fs::create_dir(&workspace).unwrap();
    std::fs::write(workspace.join("a.js"), format!("function leaf() {{}}\nfunction helper() {{ leaf(); }}\nfunction seed(flag) {{ if (flag) helper(); console.log(flag); }}\n//{}", "x".repeat(padding))).unwrap();
    let options = IndexOptions::new(workspace.clone());
    let cancel = Arc::new(AtomicBool::new(false));
    let graph = index_workspace(&options, &cancel, |_| {}).unwrap();
    // Even locally named calls remain terminal, with no inferred graph edge.
    let seed = graph
        .nodes
        .iter()
        .find(|n| n.name == "seed")
        .unwrap()
        .id
        .clone();
    let store = crate::common::open_store(&dir.path().join("state"), &workspace).unwrap();
    store
        .publish(
            &graph,
            &store.leader().unwrap(),
            baleyg::model::IndexPin {
                index_generation: store.status().unwrap().revision.index_generation,
                index_revision: 0,
            },
            &cancel,
        )
        .unwrap();
    let state = http::new(
        store.clone(),
        options,
        TOKEN.into(),
        "127.0.0.1:7331".parse().unwrap(),
    )
    .unwrap();
    let pin = store.status().unwrap().revision;
    (
        dir,
        store,
        graph,
        http::router(state),
        json!({"seed":seed,"question":"helper leaf", "expectedRevision":pin}),
    )
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
    assert_eq!(response.headers()["cache-control"], "no-store");
    assert!(
        response.headers()["content-security-policy"]
            .to_str()
            .unwrap()
            .contains("connect-src 'self'")
    );
    let status = response.status();
    let bytes = to_bytes(response.into_body(), 4 * 1024 * 1024)
        .await
        .unwrap();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}
fn path(preview: &Value, action: &str) -> String {
    format!(
        "/api/questions/{}/{action}",
        preview["packet"]["packetId"].as_str().unwrap()
    )
}
fn response(export: &Value) -> Value {
    let mut answers = serde_json::Map::new();
    for alias in export["questions"].as_object().unwrap().keys() {
        answers.insert(
            alias.clone(),
            json!({"type":"choice", "choice":"essential", "confidence":1.0,
            "probabilities":{"essential":1.0,"supporting":0.0,"incidental":0.0,"uncertain":0.0}}),
        );
    }
    json!({"model":"jev-1.13.0","answers":answers})
}
#[tokio::test]
async fn offline_roundtrip_is_stable_and_preserves_display_policy() {
    let (_d, store, _g, app, request) = setup(0);
    let (status, preview) = call(&app, "POST", "/api/questions/preview", request.clone()).await;
    assert_eq!(status, 200, "{preview}");
    assert_eq!(preview["view"]["selectionSource"], "localPreview");
    assert_eq!(preview["view"]["calls"].as_array().unwrap().len(), 1);
    assert!(
        preview["view"]["calls"]
            .as_array()
            .unwrap()
            .iter()
            .all(|c| c["caller"] == request["seed"])
    );
    assert_eq!(
        call(&app, "POST", "/api/questions/preview", request)
            .await
            .1,
        preview
    );
    let (status, export) = call(&app, "GET", &path(&preview, "jev-request"), Value::Null).await;
    assert_eq!(status, 200);
    assert_eq!(export["model"], "jev-1.13.0");
    assert_eq!(decode_packet(&export), preview["packet"]);
    let packet_id = preview["packet"]["packetId"].as_str().unwrap();
    for index in 0..preview["packet"]["context"]["calls"]
        .as_array()
        .unwrap()
        .len()
    {
        assert!(
            export["questions"]
                .as_object()
                .unwrap()
                .contains_key(&format!("c{index}_{packet_id}"))
        );
        assert!(
            export["questions"][format!("c{index}_{packet_id}")]["instructions"]
                .as_str()
                .unwrap()
                .contains(&format!("calls.rows[{index}]"))
        );
        let instruction = export["questions"][format!("c{index}_{packet_id}")]["instructions"]
            .as_str()
            .unwrap();
        let call = &preview["packet"]["context"]["calls"][index];
        if call["caller"] == preview["packet"]["request"]["seed"] {
            assert!(instruction.contains("Direct seed call; display eligible"));
        } else {
            assert!(instruction.contains("Deeper call; evidence only, display disabled"));
        }
    }
    assert_eq!(
        export["questions"].as_object().unwrap().len(),
        preview["packet"]["context"]["calls"]
            .as_array()
            .unwrap()
            .len()
    );
    assert_eq!(
        call(&app, "GET", &path(&preview, "jev-request"), Value::Null)
            .await
            .1,
        export
    );
    let (status, imported) = call(
        &app,
        "POST",
        &path(&preview, "jev-response"),
        response(&export),
    )
    .await;
    assert_eq!(status, 200, "{imported}");
    assert_eq!(imported["view"]["selectionSource"], "importedJev");
    assert_eq!(imported["view"]["policyHiddenCount"], 0);
    assert!(
        imported["view"]["calls"]
            .as_array()
            .unwrap()
            .iter()
            .all(|c| c.get("target").is_none())
    );
    assert!(
        imported["view"]["calls"]
            .as_array()
            .unwrap()
            .iter()
            .all(|c| c["caller"] == preview["packet"]["request"]["seed"])
    );
    let (status, manual) = call(
        &app,
        "POST",
        &path(&preview, "selection"),
        imported["selection"].clone(),
    )
    .await;
    assert_eq!(status, 200);
    assert_eq!(manual["view"]["selectionSource"], "manual");
    assert_eq!(manual["view"]["calls"], imported["view"]["calls"]);
    assert_eq!(store.status().unwrap().revision.index_revision, 1);
}
#[tokio::test]
async fn bad_choices_missing_packets_and_stale_revisions_are_client_errors() {
    let (_d, store, graph, app, request) = setup(0);
    let (_, preview) = call(&app, "POST", "/api/questions/preview", request.clone()).await;
    let (status, export) = call(&app, "GET", &path(&preview, "jev-request"), Value::Null).await;
    assert_eq!(status, 200);
    let mut other_request = request.clone();
    other_request["question"] = json!("a different question about the same calls");
    let (status, other) = call(&app, "POST", "/api/questions/preview", other_request).await;
    assert_eq!(status, 200);
    assert_ne!(other["packet"]["packetId"], preview["packet"]["packetId"]);
    assert_eq!(
        other["packet"]["context"]["calls"],
        preview["packet"]["context"]["calls"]
    );
    assert_eq!(
        call(
            &app,
            "POST",
            &path(&other, "jev-response"),
            response(&export)
        )
        .await
        .0,
        422
    );
    for bad in [
        json!({"packetId":preview["packet"]["packetId"],"decisions":[]}),
        json!({"packetId":"wrong","decisions":preview["selection"]["decisions"]}),
        json!({"packetId":preview["packet"]["packetId"],"decisions":[{"candidateId":"unknown","relevance":"essential"}]}),
    ] {
        assert_eq!(
            call(&app, "POST", &path(&preview, "selection"), bad)
                .await
                .0,
            422
        );
    }
    let mut duplicate = preview["selection"].clone();
    let first = duplicate["decisions"][0].clone();
    duplicate["decisions"].as_array_mut().unwrap().push(first);
    assert_eq!(
        call(&app, "POST", &path(&preview, "selection"), duplicate)
            .await
            .0,
        422
    );
    assert_eq!(
        call(
            &app,
            "POST",
            &path(&preview, "jev-response"),
            json!({"model":"jev-1.13.0","answers":{}})
        )
        .await
        .0,
        422
    );
    for (method, action, body) in [
        ("GET", "jev-request", Value::Null),
        ("POST", "jev-response", response(&export)),
        ("POST", "selection", preview["selection"].clone()),
    ] {
        assert_eq!(
            call(
                &app,
                method,
                &format!("/api/questions/absent/{action}"),
                body.clone()
            )
            .await
            .0,
            404
        );
    }
    let mut absent = request.clone();
    absent["seed"] = json!("absent");
    assert_eq!(
        call(&app, "POST", "/api/questions/preview", absent).await.0,
        404
    );
    store
        .publish(
            &graph,
            &store.leader().unwrap(),
            baleyg::model::IndexPin {
                index_generation: store.status().unwrap().revision.index_generation,
                index_revision: 1,
            },
            &Arc::new(AtomicBool::new(false)),
        )
        .unwrap();
    assert_eq!(
        call(&app, "POST", "/api/questions/preview", request)
            .await
            .0,
        409
    );
    for (method, action, body) in [
        ("GET", "jev-request", Value::Null),
        ("POST", "jev-response", response(&export)),
        ("POST", "selection", preview["selection"].clone()),
    ] {
        assert_eq!(
            call(&app, method, &path(&preview, action), body).await.0,
            409
        );
    }
}
#[tokio::test]
async fn cache_is_bounded_and_failed_previews_do_not_evict() {
    let (_d, _store, _graph, app, mut request) = setup(0);
    let mut previews = Vec::new();
    for i in 0..8 {
        request["question"] = json!(format!("helper question {i}"));
        let (status, p) = call(&app, "POST", "/api/questions/preview", request.clone()).await;
        assert_eq!(status, 200);
        previews.push(p);
    }
    let mut invalid = request.clone();
    invalid["seed"] = json!("absent");
    assert_eq!(
        call(&app, "POST", "/api/questions/preview", invalid)
            .await
            .0,
        404
    );
    assert_eq!(
        call(&app, "GET", &path(&previews[0], "jev-request"), Value::Null)
            .await
            .0,
        200
    );
    request["question"] = json!("ninth question");
    assert_eq!(
        call(&app, "POST", "/api/questions/preview", request)
            .await
            .0,
        200
    );
    assert_eq!(
        call(&app, "GET", &path(&previews[0], "jev-request"), Value::Null)
            .await
            .0,
        404
    );
    for p in &previews[1..] {
        assert_eq!(
            call(&app, "GET", &path(p, "jev-request"), Value::Null)
                .await
                .0,
            200
        );
    }
}
#[tokio::test]
async fn strict_inputs_guards_and_body_limits_apply_to_question_routes() {
    let (_d, _s, _g, app, request) = setup(0);
    for field in [
        "context",
        "sourceFiles",
        "packet",
        "providerKey",
        "unexpected",
    ] {
        let mut invalid = request.clone();
        invalid[field] = json!({});
        assert!(
            call(&app, "POST", "/api/questions/preview", invalid)
                .await
                .0
                .is_client_error()
        );
    }
    let (_, p) = call(&app, "POST", "/api/questions/preview", request).await;
    let mut selection = p["selection"].clone();
    selection["context"] = json!({});
    assert_eq!(
        call(&app, "POST", &path(&p, "selection"), selection)
            .await
            .0,
        422
    );
    for (method, url) in [
        ("POST", "/api/questions/preview".into()),
        ("GET", path(&p, "jev-request")),
        ("POST", path(&p, "jev-response")),
        ("POST", path(&p, "selection")),
    ] {
        for (host, origin, token, want) in [
            ("127.0.0.1:7331", None, None, 401),
            ("evil.example", None, Some(TOKEN), 403),
            (
                "127.0.0.1:7331",
                Some("https://evil.example"),
                Some(TOKEN),
                403,
            ),
        ] {
            let mut req = Request::builder()
                .method(method)
                .uri(&url)
                .header("host", host);
            if let Some(origin) = origin {
                req = req.header("origin", origin);
            }
            if let Some(token) = token {
                req = req.header("authorization", format!("Bearer {token}"));
            }
            let result = app
                .clone()
                .oneshot(req.body(Body::empty()).unwrap())
                .await
                .unwrap();
            assert_eq!(result.status().as_u16(), want);
        }
        let req = Request::builder()
            .method(method)
            .uri(&url)
            .header("host", "127.0.0.1:7331")
            .header("authorization", format!("Bearer {TOKEN}"))
            .body(Body::from(vec![b' '; 1024 * 1024 + 1]))
            .unwrap();
        assert_eq!(app.clone().oneshot(req).await.unwrap().status(), 413);
    }
}
#[tokio::test]
async fn oversized_export_explains_how_to_narrow_without_truncation() {
    let (_d, _s, _g, app, request) = setup(180_000);
    let (status, p) = call(&app, "POST", "/api/questions/preview", request).await;
    assert_eq!(status, 200, "{p}");
    let (status, error) = call(&app, "GET", &path(&p, "jev-request"), Value::Null).await;
    assert_eq!(status, 422);
    let message = error["error"]["message"].as_str().unwrap();
    assert!(
        message.contains("176000")
            && message.contains("Narrow")
            && message.contains("cannot be truncated")
    );
}

#[tokio::test]
async fn packet_operation_pair_matrix() {
    use baleyg::store::topology::UseGuard;
    let (temp, _store, graph, app, request) = setup(0);
    let (code, preview) = call(&app, "POST", "/api/questions/preview", request.clone()).await;
    assert_eq!(code, 200, "{preview}");
    let old: IndexPin = serde_json::from_value(preview["packet"]["revision"].clone()).unwrap();
    let index_root = temp.path().join("state/cache/indexes");
    let dir = std::fs::read_dir(&index_root)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .find(|p| p.is_dir())
        .unwrap();
    let lock = index_root.join(format!(
        "{}.lock",
        dir.file_name().unwrap().to_string_lossy()
    ));
    let exclusive = UseGuard::acquire_existing(&lock, true, true).unwrap();
    std::fs::remove_file(dir.join("index.db")).unwrap();
    std::fs::remove_file(dir.join("leader.lock")).unwrap();
    std::fs::remove_dir(dir).unwrap();
    exclusive.remove_last().unwrap();
    let recreated =
        crate::common::open_store(&temp.path().join("state"), &temp.path().join("workspace"))
            .unwrap();
    recreated
        .publish(
            &graph,
            &recreated.leader().unwrap(),
            recreated.status().unwrap().revision,
            &Arc::new(AtomicBool::new(false)),
        )
        .unwrap();
    let fresh = recreated.status().unwrap().revision;
    assert_eq!(old.index_revision, fresh.index_revision);
    assert_ne!(old.index_generation, fresh.index_generation);
    assert_eq!(
        call(&app, "POST", "/api/questions/preview", request)
            .await
            .0,
        409
    );
    assert_eq!(
        call(&app, "GET", &path(&preview, "jev-request"), Value::Null)
            .await
            .0,
        409
    );
}

#[tokio::test]
async fn legacy_reindex_invalidates_cached_question_exports_and_rebuilds_terminal_packet() {
    let (temp, _store, _graph, app, request) = setup(0);
    let (status, old) = call(&app, "POST", "/api/questions/preview", request.clone()).await;
    assert_eq!(status, StatusCode::OK, "{old}");
    let old_pin = old["packet"]["revision"].clone();
    let (status, old_export) = call(&app, "GET", &path(&old, "jev-request"), Value::Null).await;
    assert_eq!(status, StatusCode::OK, "{old_export}");
    let index_root = temp.path().join("state/cache/indexes");
    let db_path = std::fs::read_dir(index_root)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .find(|path| path.is_dir())
        .unwrap()
        .join("index.db");
    {
        let db = rusqlite::Connection::open(db_path).unwrap();
        db.execute(
            "UPDATE index_metadata SET schema_version=4,extractor_version='native-v1'",
            [],
        )
        .unwrap();
        db.pragma_update(None, "user_version", 4).unwrap();
        db.execute("UPDATE calls SET payload=json_set(payload,'$.target','lexical-guess','$.resolution','internal')", []).unwrap();
    }
    for (method, action, body) in [
        ("GET", "jev-request", Value::Null),
        ("POST", "selection", old["selection"].clone()),
        ("POST", "jev-response", response(&old_export)),
    ] {
        let (status, result) = call(&app, method, &path(&old, action), body).await;
        assert_eq!(
            status,
            StatusCode::SERVICE_UNAVAILABLE,
            "{action}: {result}"
        );
        assert_eq!(result["error"]["code"], "index_not_ready");
        assert!(!result.to_string().contains("lexical-guess"));
    }
    let (status, job) = call(
        &app,
        "POST",
        "/api/index",
        json!({"expectedRevision":old_pin}),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{job}");
    let id = job["id"].as_str().unwrap();
    let done = tokio::time::timeout(std::time::Duration::from_secs(10), async {
        loop {
            let (_, current) = call(&app, "GET", &format!("/api/jobs/{id}"), Value::Null).await;
            if !current["finishedAt"].is_null() {
                break current;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert_eq!(done["state"], "completed", "{done}");
    let (status, ready) = call(&app, "GET", "/api/status", Value::Null).await;
    assert_eq!(status, StatusCode::OK, "{ready}");
    assert_eq!(ready["evidenceFormat"], "terminal-native-graph-v1");
    assert_ne!(
        ready["revision"]["indexGeneration"],
        old_pin["indexGeneration"]
    );
    for (method, action, body) in [
        ("GET", "jev-request", Value::Null),
        ("POST", "selection", old["selection"].clone()),
        ("POST", "jev-response", response(&old_export)),
    ] {
        let (status, result) = call(&app, method, &path(&old, action), body).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{action}: {result}");
    }
    let (status, stale) = call(&app, "POST", "/api/questions/preview", request.clone()).await;
    assert_eq!(status, StatusCode::CONFLICT, "{stale}");
    let mut fresh_request = request;
    fresh_request["expectedRevision"] = ready["revision"].clone();
    let (status, fresh) = call(&app, "POST", "/api/questions/preview", fresh_request).await;
    assert_eq!(status, StatusCode::OK, "{fresh}");
    assert_eq!(fresh["packet"]["revision"], ready["revision"]);
    assert!(!fresh.to_string().contains("lexical-guess"));
    assert!(
        fresh["packet"]["context"]["calls"]
            .as_array()
            .unwrap()
            .iter()
            .all(|call| call.get("target").is_none())
    );
    let (status, export) = call(&app, "GET", &path(&fresh, "jev-request"), Value::Null).await;
    assert_eq!(status, StatusCode::OK, "{export}");
    assert!(!export.to_string().contains("lexical-guess"));
    let (status, imported) = call(
        &app,
        "POST",
        &path(&fresh, "jev-response"),
        response(&export),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{imported}");
    assert_eq!(imported["view"]["selectionSource"], "importedJev");
    assert!(
        imported["view"]["calls"]
            .as_array()
            .unwrap()
            .iter()
            .all(|call| call.get("target").is_none())
    );
    let (status, selected) = call(
        &app,
        "POST",
        &path(&fresh, "selection"),
        imported["selection"].clone(),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{selected}");
    assert_eq!(selected["view"]["selectionSource"], "manual");
}
