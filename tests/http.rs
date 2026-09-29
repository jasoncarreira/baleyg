mod common;
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
    let store = crate::common::open_store(&dir.path().join("state"), &workspace).unwrap();
    let state = http::new(
        store.clone(),
        IndexOptions::new(workspace),
        TOKEN.into(),
        "127.0.0.1:7331".parse().unwrap(),
    )
    .unwrap();
    state.retain_serving_session(store.leader_session().unwrap());
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
        ("/api/status", "127.0.0.1:7331", None, Some(TOKEN), 503),
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
            503,
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
        400
    );
    let stale = IndexPin {
        index_generation: _store.index_baseline().unwrap().index_generation,
        index_revision: 9,
    };
    assert_eq!(
        call(
            &app,
            "POST",
            "/api/index",
            json!({"expectedRevision":stale})
        )
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
    let (_d, store, state, app) = setup();
    let workspace = _d.path().join("workspace");
    std::fs::write(workspace.join("a.js"), "cached secret-free source").unwrap();
    let cancel = Arc::new(AtomicBool::new(false));
    let (graph, native, capture) = baleyg::indexer::index_workspace_bundle(
        &IndexOptions::new(workspace),
        store.root_id(),
        &cancel,
        |_| {},
    )
    .unwrap();
    assert_eq!(graph.files[0].text, "cached secret-free source");
    store
        .publish_native(
            &graph,
            &capture,
            &native,
            state
                .retained_serving_session()
                .unwrap()
                .leader_guard()
                .unwrap(),
            store.index_baseline().unwrap(),
            &cancel,
        )
        .unwrap();
    let pin = store.status().unwrap().revision;
    let (code, body) = call(
        &app,
        "GET",
        &format!(
            "/api/source?path=a.js&indexGeneration={}&indexRevision={}",
            pin.index_generation, pin.index_revision
        ),
        Value::Null,
    )
    .await;
    assert_eq!(code, 200);
    assert_eq!(body["revision"], json!(pin));
    assert_eq!(body["file"]["text"], "cached secret-free source");
    assert_eq!(
        call(
            &app,
            "GET",
            &format!(
                "/api/source?path=a.js&indexGeneration={}&indexRevision=0",
                pin.index_generation
            ),
            Value::Null
        )
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
async fn dependency_status_whole_response_fence() {
    let (dir, store, state, app) = setup();
    std::fs::write(dir.path().join("workspace/a.js"), "function go() {}\n").unwrap();
    let options = IndexOptions::new(dir.path().join("workspace"));
    let cancel = Arc::new(AtomicBool::new(false));
    let (graph, native, capture) =
        baleyg::indexer::index_workspace_bundle(&options, store.root_id(), &cancel, |_| {})
            .unwrap();
    let pin = store
        .publish_native(
            &graph,
            &capture,
            &native,
            state
                .retained_serving_session()
                .unwrap()
                .leader_guard()
                .unwrap(),
            store.index_baseline().unwrap(),
            &cancel,
        )
        .unwrap();
    let (code, result) = call(&app, "GET", "/api/dependencies", Value::Null).await;
    assert_eq!(code, StatusCode::OK, "{result}");
    assert_eq!(result["workspaceRevision"], json!(pin));
    assert_eq!(result["catalogId"], Value::Null);
    assert_eq!(result["packages"], json!([]));
    assert_eq!(result["symbolCount"], 0);
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
    assert_eq!(store.status().unwrap().revision.index_revision, 1);
    let (_, cancelled) = call(&app, "POST", &format!("/api/jobs/{id}/cancel"), Value::Null).await;
    assert_eq!(cancelled["state"], "completed");
    assert_eq!(store.status().unwrap().revision.index_revision, 1);
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
    assert_eq!(store.index_baseline().unwrap().index_revision, 0);
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

#[tokio::test]
async fn legacy_index_refuses_derived_routes_and_cached_packet_before_pin_comparison() {
    let (dir, store, state, app) = setup();
    let workspace = dir.path().join("workspace");
    std::fs::write(
        workspace.join("a.js"),
        "function go() { console.log(1); }\nconst value = 1;",
    )
    .unwrap();
    std::fs::write(
        workspace.join("Types.java"),
        "class Base {}\nclass Types extends Base { void run() { System.out.println(1); } }",
    )
    .unwrap();
    let options = IndexOptions::new(workspace.clone());
    let cancel = Arc::new(AtomicBool::new(false));
    let (graph, native, capture) =
        baleyg::indexer::index_workspace_bundle(&options, store.root_id(), &cancel, |_| {})
            .unwrap();
    let pin = store
        .publish_native(
            &graph,
            &capture,
            &native,
            state
                .retained_serving_session()
                .unwrap()
                .leader_guard()
                .unwrap(),
            store.index_baseline().unwrap(),
            &cancel,
        )
        .unwrap();
    let seed = graph
        .nodes
        .iter()
        .find(|n| n.name == "go")
        .unwrap()
        .id
        .clone();
    let class_seed = graph
        .nodes
        .iter()
        .find(|node| node.name == "Types")
        .unwrap()
        .id
        .clone();
    let variable_seed = graph
        .nodes
        .iter()
        .find(|node| node.name == "value")
        .unwrap()
        .id
        .clone();
    let (code, preview) = call(
        &app,
        "POST",
        "/api/questions/preview",
        json!({"seed":seed,"question":"what happens?","expectedRevision":pin}),
    )
    .await;
    assert_eq!(code, StatusCode::OK, "{preview}");
    let packet = preview["packet"]["packetId"].as_str().unwrap();
    let old_selection = preview["selection"].clone();
    // Retain the daemon's verified leader session across deliberate legacy metadata tampering.
    let leader = state.retained_serving_session().unwrap();
    let index = std::fs::read_dir(dir.path().join("state/cache/indexes"))
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .find(|path| path.is_dir())
        .unwrap()
        .join("index.db");
    {
        let db = rusqlite::Connection::open(&index).unwrap();
        db.pragma_update(None, "foreign_keys", false).unwrap();
        let native_tables = db
            .prepare("SELECT name FROM sqlite_master WHERE type='table' AND name LIKE 'native_%'")
            .unwrap()
            .query_map([], |row| row.get::<_, String>(0))
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap();
        for table in native_tables {
            db.execute_batch(&format!("DROP TABLE {table}")).unwrap();
        }
        for index in ["nodes_path", "calls_path", "regions_path"] {
            db.execute_batch(&format!("DROP INDEX {index}")).unwrap();
        }
        db.execute_batch(
            "DROP TABLE capture_inputs;
             ALTER TABLE files DROP COLUMN capture_stat;
             ALTER TABLE index_metadata DROP COLUMN reconcile_options;
             ALTER TABLE index_metadata DROP COLUMN reconciled_incarnation;",
        )
        .unwrap();
        // An old index holds lexical class adjacency and lexical call targets.
        let base: String = db
            .query_row("SELECT id FROM classes WHERE name='Base'", [], |row| {
                row.get(0)
            })
            .unwrap();
        let lexical_edges = db
            .execute(
                "UPDATE class_relations SET target=?1, payload=json_set(payload,'$.target',?1,'$.candidateIds',json_array(?1),'$.matchKind','actual')",
                [&base],
            )
            .unwrap();
        assert!(lexical_edges > 0, "fixture needs a lexical class edge");
        let lexical_calls = db
            .execute(
                "UPDATE calls SET target=?1, payload=json_set(payload,'$.target',?1,'$.resolution','internal')",
                [&base],
            )
            .unwrap();
        assert!(lexical_calls > 0, "fixture needs a lexical call target");
        db.execute(
            "UPDATE index_metadata SET schema_version=4,extractor_version='native-v1'",
            [],
        )
        .unwrap();
        db.pragma_update(None, "user_version", 4).unwrap();
    }
    let same_old_pair = json!(pin).to_string();
    let unauthenticated = Request::builder()
        .method("POST")
        .uri("/api/index")
        .header("host", "127.0.0.1:7331")
        .body(Body::from("{}"))
        .unwrap();
    assert_eq!(
        app.clone().oneshot(unauthenticated).await.unwrap().status(),
        StatusCode::UNAUTHORIZED
    );
    let mut denied_routes = Vec::new();
    for (method, path, body) in [
        ("GET", "/api/status".to_owned(), Value::Null),
        ("GET", "/api/symbols?q=go".to_owned(), Value::Null),
        ("GET", "/api/source?path=a.js".to_owned(), Value::Null),
        ("GET", "/api/classes".to_owned(), Value::Null),
        ("GET", "/api/tree".to_owned(), Value::Null),
        ("GET", "/api/files".to_owned(), Value::Null),
        ("GET", "/api/dependencies".to_owned(), Value::Null),
        (
            "GET",
            "/api/dependencies/symbols?catalogId=stale&q=go".to_owned(),
            Value::Null,
        ),
        (
            "GET",
            "/api/dependencies/source?catalogId=stale&sourceRef=stale".to_owned(),
            Value::Null,
        ),
        ("GET", "/api/methods?path=a.js".to_owned(), Value::Null),
        ("GET", format!("/api/symbol?id={seed}"), Value::Null),
        (
            "GET",
            format!(
                "/api/source?path=a.js&indexGeneration={}&indexRevision={}",
                pin.index_generation, pin.index_revision
            ),
            Value::Null,
        ),
        (
            "GET",
            format!(
                "/api/classes?indexGeneration={}&indexRevision={}",
                pin.index_generation, pin.index_revision
            ),
            Value::Null,
        ),
        (
            "GET",
            format!(
                "/api/files?indexGeneration={}&indexRevision={}",
                pin.index_generation, pin.index_revision
            ),
            Value::Null,
        ),
        (
            "POST",
            "/api/navigation".to_owned(),
            json!({"expectedRevision":pin,"path":"a.js","line":1}),
        ),
        (
            "POST",
            "/api/sequence".to_owned(),
            json!({"seed":seed,"expectedRevision":pin}),
        ),
        (
            "POST",
            "/api/class-diagram".to_owned(),
            json!({"seed":class_seed,"expectedRevision":pin}),
        ),
        (
            "POST",
            "/api/query".to_owned(),
            json!({"seed":seed,"includeCallbacks":true}),
        ),
        (
            "POST",
            "/api/questions/preview".to_owned(),
            json!({"seed":seed,"question":"what?","expectedRevision":pin}),
        ),
        (
            "GET",
            format!("/api/questions/{packet}/jev-request"),
            Value::Null,
        ),
        (
            "POST",
            format!("/api/questions/{packet}/selection"),
            json!({"packetId":packet,"decisions":[]}),
        ),
        (
            "POST",
            format!("/api/questions/{packet}/jev-response"),
            json!({}),
        ),
        (
            "POST",
            format!("/api/questions/{packet}/jev-run"),
            json!({}),
        ),
        (
            "POST",
            format!("/api/questions/{packet}/acp-answer"),
            json!({}),
        ),
    ] {
        let (status, response) = call(&app, method, &path, body).await;
        if status != StatusCode::SERVICE_UNAVAILABLE
            || response["error"]["code"] != "index_not_ready"
        {
            denied_routes.push(format!("{method} {path}: {status} {response}"));
        }
        assert!(
            !response.to_string().contains(&same_old_pair),
            "old pin escaped {path}"
        );
    }
    assert!(
        denied_routes.is_empty(),
        "unexpected old-cache responses: {denied_routes:#?}"
    );
    assert_eq!(
        call(&app, "GET", "/api/jobs/current", Value::Null).await.0,
        StatusCode::OK
    );
    assert_eq!(
        call(&app, "GET", "/api/acp/status", Value::Null).await.0,
        StatusCode::OK
    );
    assert_eq!(
        call(&app, "GET", "/api/jev/status", Value::Null).await.0,
        StatusCode::OK
    );
    // A failed explicit old rebaseline cannot publish or clear a prior question packet.
    // The old-readiness gate still prevents any cached packet from being served.
    let old_bytes = std::fs::read(&index).unwrap();
    let cancelled = Arc::new(AtomicBool::new(true));
    assert!(
        store
            .publish_native(
                &graph,
                &capture,
                &native,
                leader.leader_guard().unwrap(),
                pin,
                &cancelled,
            )
            .is_err()
    );
    assert_eq!(std::fs::read(&index).unwrap(), old_bytes);
    assert_eq!(
        call(
            &app,
            "GET",
            &format!("/api/questions/{packet}/jev-request"),
            Value::Null
        )
        .await
        .0,
        StatusCode::SERVICE_UNAVAILABLE
    );
    assert_eq!(
        call(&app, "GET", "/api/status", Value::Null).await.0,
        StatusCode::SERVICE_UNAVAILABLE
    );
    drop(leader);
    let (code, job) = call(&app, "POST", "/api/index", json!({"expectedRevision":pin})).await;
    assert_eq!(code, StatusCode::ACCEPTED);
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
    assert_eq!(status, StatusCode::OK);
    assert_eq!(ready["evidenceFormat"], "terminal-native-graph-v1");
    assert_ne!(
        ready["revision"]["indexGeneration"],
        json!(pin)["indexGeneration"]
    );
    assert_eq!(
        call(
            &app,
            "GET",
            &format!("/api/questions/{packet}/jev-request"),
            Value::Null
        )
        .await
        .0,
        StatusCode::NOT_FOUND
    );
    let (status, graph_result) = call(
        &app,
        "POST",
        "/api/query",
        json!({"seed":seed,"includeCallbacks":true}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{graph_result}");
    assert_eq!(graph_result["nodes"].as_array().unwrap().len(), 1);
    assert!(
        graph_result["calls"]
            .as_array()
            .unwrap()
            .iter()
            .all(|call| call.get("target").is_none())
    );
    let fresh: IndexPin = serde_json::from_value(ready["revision"].clone()).unwrap();
    assert!(fresh.index_revision > 0);
    let pinned = |route: &str, revision: &IndexPin| {
        format!(
            "{route}{}indexGeneration={}&indexRevision={}",
            if route.contains('?') { '&' } else { '?' },
            revision.index_generation,
            revision.index_revision
        )
    };
    let (status, source) = call(
        &app,
        "GET",
        &pinned("/api/source?path=a.js", &fresh),
        Value::Null,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{source}");
    assert_eq!(source["revision"], json!(fresh));
    assert_eq!(
        source["file"]["text"],
        "function go() { console.log(1); }\nconst value = 1;"
    );
    let (status, symbol) = call(
        &app,
        "GET",
        &pinned(&format!("/api/symbol?id={seed}"), &fresh),
        Value::Null,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{symbol}");
    assert_eq!(symbol["revision"], json!(fresh));
    assert_eq!(symbol["symbol"]["name"], "go");
    let (status, search) = call(&app, "GET", "/api/symbols?q=value", Value::Null).await;
    assert_eq!(status, StatusCode::OK, "{search}");
    assert!(
        search["items"]
            .as_array()
            .unwrap()
            .iter()
            .any(|item| item["id"] == variable_seed && item["kind"] == "variable"),
        "{search}"
    );
    let (status, variable) = call(
        &app,
        "GET",
        &pinned(&format!("/api/symbol?id={variable_seed}"), &fresh),
        Value::Null,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{variable}");
    assert_eq!(variable["symbol"]["kind"], "variable");
    let (status, methods) = call(&app, "GET", "/api/methods?path=a.js", Value::Null).await;
    assert_eq!(status, StatusCode::OK, "{methods}");
    assert!(
        methods["items"]
            .as_array()
            .unwrap()
            .iter()
            .all(|item| item["symbol"]["id"] != variable_seed)
    );
    let (status, files) = call(&app, "GET", "/api/files", Value::Null).await;
    assert_eq!(status, StatusCode::OK, "{files}");
    let js_file = files["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["path"] == "a.js")
        .unwrap();
    assert_eq!(js_file["methodCount"], 1);
    let (status, refused) = call(
        &app,
        "POST",
        "/api/sequence",
        json!({"seed":variable_seed,"expectedRevision":fresh}),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{refused}");
    let (status, variable_view) =
        call(&app, "POST", "/api/query", json!({"seed":variable_seed})).await;
    assert_eq!(status, StatusCode::OK, "{variable_view}");
    assert_eq!(variable_view["nodes"][0]["kind"], "variable");
    assert_eq!(variable_view["calls"], json!([]));
    let (status, variable_packet) = call(
        &app,
        "POST",
        "/api/questions/preview",
        json!({"seed":variable_seed,"question":"what is value?","expectedRevision":fresh}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{variable_packet}");
    assert_eq!(variable_packet["packet"]["context"]["calls"], json!([]));
    assert_eq!(
        variable_packet["packet"]["context"]["nodes"][0]["kind"],
        "variable"
    );
    for route in [
        "/api/symbols?q=go",
        "/api/files",
        "/api/methods?path=a.js",
        "/api/classes",
        "/api/tree",
        "/api/dependencies",
    ] {
        let (status, body) = call(&app, "GET", route, Value::Null).await;
        assert_eq!(status, StatusCode::OK, "{route}: {body}");
        assert!(
            !body.to_string().contains("lexical-guess"),
            "{route}: {body}"
        );
    }
    for (route, body) in [
        (
            "/api/navigation",
            json!({"expectedRevision":fresh,"path":"a.js","line":1}),
        ),
        (
            "/api/sequence",
            json!({"seed":seed,"expectedRevision":fresh}),
        ),
        (
            "/api/class-diagram",
            json!({"seed":class_seed,"expectedRevision":fresh}),
        ),
    ] {
        let (status, view) = call(&app, "POST", route, body).await;
        assert_eq!(status, StatusCode::OK, "{route}: {view}");
        assert_eq!(view["revision"], json!(fresh), "{route}: {view}");
        assert!(!view.to_string().contains("lexical-guess"));
        assert!(!view.to_string().contains(r#""resolution":"internal""#));
    }
    let (status, preview) = call(
        &app,
        "POST",
        "/api/questions/preview",
        json!({"seed":seed,"question":"what happens?","expectedRevision":fresh}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{preview}");
    assert_eq!(preview["packet"]["revision"], json!(fresh));
    assert!(!preview.to_string().contains("lexical-guess"));
    let fresh_packet = preview["packet"]["packetId"].as_str().unwrap();
    let (status, export) = call(
        &app,
        "GET",
        &format!("/api/questions/{fresh_packet}/jev-request"),
        Value::Null,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{export}");
    assert!(!export.to_string().contains("lexical-guess"));
    let mut answers = serde_json::Map::new();
    for alias in export["questions"].as_object().unwrap().keys() {
        answers.insert(
            alias.clone(),
            json!({"type":"choice","choice":"essential","confidence":1.0,
            "probabilities":{"essential":1.0,"supporting":0.0,"incidental":0.0,"uncertain":0.0}}),
        );
    }
    let (status, imported) = call(
        &app,
        "POST",
        &format!("/api/questions/{fresh_packet}/jev-response"),
        json!({"model":"jev-1.13.0","answers":answers}),
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
        &format!("/api/questions/{fresh_packet}/selection"),
        imported["selection"].clone(),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{selected}");
    assert_eq!(selected["view"]["selectionSource"], "manual");
    for (action, code) in [("acp-answer", "acp_disabled"), ("jev-run", "jev_disabled")] {
        let (status, body) = call(
            &app,
            "POST",
            &format!("/api/questions/{fresh_packet}/{action}"),
            json!({}),
        )
        .await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{action}: {body}");
        assert_eq!(body["error"]["code"], code);
        assert!(!body.to_string().contains("lexical-guess"));
    }
    for route in ["/api/jobs/current", "/api/acp/status", "/api/jev/status"] {
        assert_eq!(
            call(&app, "GET", route, Value::Null).await.0,
            StatusCode::OK,
            "{route}"
        );
    }
    // A once-valid schema-4 pair must never join data from the rotated generation.
    for route in [
        "/api/files",
        "/api/classes",
        "/api/methods?path=a.js",
        "/api/source?path=a.js",
        &format!("/api/symbol?id={seed}"),
    ] {
        let (status, body) = call(&app, "GET", &pinned(route, &pin), Value::Null).await;
        assert_eq!(status, StatusCode::CONFLICT, "{route}: {body}");
        assert_eq!(body["error"]["code"], "revision_conflict");
    }
    for (route, body) in [
        ("/api/sequence", json!({"seed":seed,"expectedRevision":pin})),
        (
            "/api/navigation",
            json!({"path":"a.js","line":1,"expectedRevision":pin}),
        ),
        (
            "/api/class-diagram",
            json!({"seed":class_seed,"expectedRevision":pin}),
        ),
        (
            "/api/questions/preview",
            json!({"seed":seed,"question":"what?","expectedRevision":pin}),
        ),
    ] {
        let (status, response) = call(&app, "POST", route, body).await;
        assert_eq!(status, StatusCode::CONFLICT, "{route}: {response}");
    }
    for (action, method, body) in [
        ("jev-request", "GET", Value::Null),
        ("selection", "POST", old_selection),
        (
            "jev-response",
            "POST",
            json!({"model":"jev-1.13.0","answers":{}}),
        ),
    ] {
        let route = format!("/api/questions/{packet}/{action}");
        let (status, response) = call(&app, method, &route, body).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{route}: {response}");
    }
    for (action, code) in [("jev-run", "jev_disabled"), ("acp-answer", "acp_disabled")] {
        let route = format!("/api/questions/{packet}/{action}");
        let (status, body) = call(&app, "POST", &route, json!({})).await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{route}: {body}");
        assert_eq!(body["error"]["code"], code);
    }
    for route in [
        "/api/dependencies/symbols?catalogId=stale&q=go",
        "/api/dependencies/source?catalogId=stale&sourceRef=stale",
    ] {
        let (status, body) = call(&app, "GET", route, Value::Null).await;
        assert_eq!(status, StatusCode::CONFLICT, "{route}: {body}");
        assert_eq!(body["error"]["code"], "stale_catalog");
    }
}

#[tokio::test]
async fn live_control_corruption_returns_typed_503_and_hard_latches_clones() {
    for case in ["stats-source", "real-symbol", "input-classes"] {
        let (dir, store, state, app) = setup();
        let workspace = dir.path().join("workspace");
        std::fs::write(workspace.join("a.js"), "function go() { measured(); }\n").unwrap();
        let cancel = Arc::new(AtomicBool::new(false));
        let (graph, native, capture) = baleyg::indexer::index_workspace_bundle(
            &IndexOptions::new(workspace),
            store.root_id(),
            &cancel,
            |_| {},
        )
        .unwrap();
        let symbol_id = graph.nodes[0].id.clone();
        let pin = store
            .publish_native(
                &graph,
                &capture,
                &native,
                state
                    .retained_serving_session()
                    .unwrap()
                    .leader_guard()
                    .unwrap(),
                store.index_baseline().unwrap(),
                &cancel,
            )
            .unwrap();
        let index = std::fs::read_dir(dir.path().join("state/cache/indexes"))
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .find(|path| path.is_dir())
            .unwrap()
            .join("index.db");
        let db = rusqlite::Connection::open(index).unwrap();
        let route = match case {
            "stats-source" => {
                db.execute("UPDATE index_metadata SET stats='not-json'", [])
                    .unwrap();
                format!(
                    "/api/source?path=a.js&indexGeneration={}&indexRevision={}",
                    pin.index_generation, pin.index_revision
                )
            }
            "real-symbol" => {
                db.execute(
                    "UPDATE index_metadata SET index_revision=CAST(1.5 AS REAL)",
                    [],
                )
                .unwrap();
                format!(
                    "/api/symbol?id={symbol_id}&indexGeneration={}&indexRevision={}",
                    pin.index_generation, pin.index_revision
                )
            }
            "input-classes" => {
                db.execute(
                    "UPDATE capture_inputs SET payload='not-json' WHERE input_key='root:.'",
                    [],
                )
                .unwrap();
                format!(
                    "/api/classes?indexGeneration={}&indexRevision={}",
                    pin.index_generation, pin.index_revision
                )
            }
            _ => unreachable!(),
        };
        drop(db);

        let (status, body) = call(&app, "GET", &route, Value::Null).await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{case}: {body}");
        assert_eq!(
            body["error"]["code"], "incompatible_index",
            "{case}: {body}"
        );
        let (status, body) = call(&app, "GET", "/api/status", Value::Null).await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{case}: {body}");
        assert_eq!(
            body["error"]["code"], "incompatible_index",
            "{case}: {body}"
        );
    }
}

#[tokio::test]
async fn index_request_without_startup_session_does_not_reacquire_leadership() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = dir.path().join("workspace");
    std::fs::create_dir(&workspace).unwrap();
    let store = crate::common::open_store(&dir.path().join("state"), &workspace).unwrap();
    let state = http::new(
        store.clone(),
        IndexOptions::new(workspace),
        TOKEN.into(),
        "127.0.0.1:7331".parse().unwrap(),
    )
    .unwrap();
    let app = http::router(state);
    let (status, body) = call(&app, "POST", "/api/index", json!({})).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(body["error"]["code"], "index_not_ready");
    let session = store.leader_session().unwrap();
    assert!(
        session.is_leader(),
        "HTTP request must not have retained or reacquired the lock"
    );
}
