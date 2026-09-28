//! Synthetic class projection tests. No workspace programs or providers run.
mod common;
use axum::{
    Router,
    body::{Body, to_bytes},
    http::{Request, StatusCode},
};
use baleyg::{
    class_diagram::ClassDiagramRequest,
    http,
    indexer::{IndexOptions, index_workspace},
    model::*,
    store::Store,
};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::{Arc, atomic::AtomicBool},
};
use tower::ServiceExt;
const TOKEN: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
const JAVA: &str = r#"package demo;
class A extends B {
    B field;
    Missing unknown;
    B run(B input) { return input; }
    class Inner { B nested(B input) { return input; } }
}
class B { A back; C second; }
class C {}
class Alone {}
"#;
fn pin_query(pin: IndexPin) -> String {
    format!(
        "indexGeneration={}&indexRevision={}",
        pin.index_generation, pin.index_revision
    )
}
fn index_db(state: &std::path::Path) -> std::path::PathBuf {
    std::fs::read_dir(state.join("cache/indexes"))
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .find(|path| path.is_dir())
        .unwrap()
        .join("index.db")
}
fn cancel() -> CancelFlag {
    Arc::new(AtomicBool::new(false))
}
fn setup_with(source: &str) -> (tempfile::TempDir, Store, Graph, Router) {
    let dir = tempfile::tempdir().unwrap();
    let workspace = dir.path().join("workspace");
    std::fs::create_dir(&workspace).unwrap();
    std::fs::write(workspace.join("Types.java"), source).unwrap();
    std::fs::write(workspace.join("unsupported.js"), "class Unsupported {}").unwrap();
    let options = IndexOptions::new(workspace.clone());
    let graph = index_workspace(&options, &cancel(), |_| {}).unwrap();
    let store = crate::common::open_store(&dir.path().join("state"), &workspace).unwrap();
    assert_eq!(
        publish_bundle(
            &store,
            &graph,
            &workspace,
            &store.leader().unwrap(),
            baleyg::model::IndexPin {
                index_generation: store.index_baseline().unwrap().index_generation,
                index_revision: 0
            },
            &cancel()
        )
        .unwrap()
        .index_revision,
        1
    );
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
fn setup() -> (tempfile::TempDir, Store, Graph, Router) {
    setup_with(JAVA)
}
fn id(graph: &Graph, name: &str) -> String {
    graph
        .nodes
        .iter()
        .find(|n| n.name == name)
        .unwrap()
        .id
        .clone()
}
fn request(seed: String, store: &Store) -> ClassDiagramRequest {
    ClassDiagramRequest {
        seed,
        expected_revision: store.status().unwrap().revision,
        expanded: vec![],
        include_unmatched: false,
        include_hierarchy: false,
    }
}
async fn call(app: &Router, method: &str, path: &str, body: Value) -> (StatusCode, Value) {
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
    assert_eq!(response.headers()["cache-control"], "no-store");
    let status = response.status();
    let bytes = to_bytes(response.into_body(), 8 * 1024 * 1024)
        .await
        .unwrap();
    (status, serde_json::from_slice(&bytes).unwrap())
}

#[tokio::test]
async fn search_pagination_literal_wildcards_and_supported_languages() {
    let (_dir, store, _graph, app) = setup();
    let pin = store.status().unwrap().revision;
    let (status, page) = call(
        &app,
        "GET",
        &format!("/api/classes?{}&path=Types.java&limit=2", pin_query(pin)),
        Value::Null,
    )
    .await;
    assert_eq!(status, 200);
    assert_eq!(page["revision"], json!(pin));
    assert_eq!(page["nextOffset"], 2);
    assert_eq!(page["requireIndex"], false);
    assert_eq!(page["items"].as_array().unwrap().len(), 2);
    let (_, filtered) = call(
        &app,
        "GET",
        &format!("/api/classes?q=Alone&{}", pin_query(pin)),
        Value::Null,
    )
    .await;
    assert_eq!(filtered["items"].as_array().unwrap().len(), 1);
    assert_eq!(filtered["items"][0]["symbol"]["name"], "Alone");
    let (status, empty_path) = call(
        &app,
        "GET",
        &format!("/api/classes?path=&q=Alone&{}", pin_query(pin)),
        Value::Null,
    )
    .await;
    assert_eq!(status, 200);
    assert_eq!(empty_path, filtered);
    for q in ["%25", "%5F", "%27%20OR%201%3D1--", "%5C"] {
        let (status, page) = call(&app, "GET", &format!("/api/classes?q={q}"), Value::Null).await;
        assert_eq!(status, 200);
        assert_eq!(page["items"], json!([]));
    }
    let (_, unsupported) = call(&app, "GET", "/api/classes?path=unsupported.js", Value::Null).await;
    assert_eq!(unsupported["items"], json!([]));
    assert!(!unsupported["warnings"].as_array().unwrap().is_empty());
}
#[tokio::test]
async fn one_hop_incoming_methods_terminal_hints_and_cached_only() {
    let (_dir, store, graph, _app) = setup();
    let seed = id(&graph, "A");
    let mut request = request(seed.clone(), &store);
    request.include_hierarchy = true;
    request.include_unmatched = true;
    let diagram = store.class_diagram_at(&request).unwrap();
    assert_eq!(diagram.nodes.len(), 1);
    assert_eq!(diagram.nodes[0].id, seed);
    assert!(
        diagram.edges.is_empty(),
        "syntax names must not create class edges"
    );
    assert!(diagram.nodes.iter().all(|node| node.kind == "class"));
}
#[tokio::test]
async fn authentication_strict_requests_revision_and_disconnected_expansion() {
    let (dir, store, graph, app) = setup();
    let pin = store.status().unwrap().revision;
    for path in ["/api/classes", "/api/class-diagram"] {
        let req = Request::builder()
            .uri(path)
            .header("host", "127.0.0.1:7331")
            .body(Body::empty())
            .unwrap();
        assert_eq!(app.clone().oneshot(req).await.unwrap().status(), 401);
    }
    for path in [
        "/api/classes?extra=1",
        "/api/classes?limit=0",
        "/api/classes?limit=101",
        "/api/classes?offset=1000001",
        "/api/classes?path=../Types.java",
        "/api/classes?path=/Types.java",
        "/api/classes?revision=x",
    ] {
        assert_eq!(call(&app, "GET", path, Value::Null).await.0, 400, "{path}");
    }
    let seed = id(&graph, "A");
    for body in [
        json!({"seed":seed}),
        json!({"seed":seed,"expectedRevision":pin,"unknown":true}),
        json!({"seed":"","expectedRevision":pin}),
        json!({"seed":"missing","expectedRevision":pin}),
        json!({"seed":seed,"expectedRevision":pin,"includeUnmatched":"yes"}),
        json!({"seed":seed,"expectedRevision":pin,"expanded":vec![seed.clone();13]}),
        json!({"seed":id(&graph,"Unsupported"),"expectedRevision":pin}),
    ] {
        let (status, value) = call(&app, "POST", "/api/class-diagram", body).await;
        assert_eq!(status, 400, "{value}");
        assert!(value["error"]["message"].is_string());
    }
    let (status, independent) = call(
        &app,
        "POST",
        "/api/class-diagram",
        json!({"seed":seed,"expectedRevision":pin,"expanded":[id(&graph,"Alone")]}),
    )
    .await;
    assert_eq!(status, 200);
    assert_eq!(independent["nodes"].as_array().unwrap().len(), 2);
    assert_eq!(independent["edges"], json!([]));
    publish_bundle(
        &store,
        &graph,
        &dir.path().join("workspace"),
        &store.leader().unwrap(),
        baleyg::model::IndexPin {
            index_generation: store.status().unwrap().revision.index_generation,
            index_revision: 1,
        },
        &cancel(),
    )
    .unwrap();
    assert_eq!(
        call(
            &app,
            "GET",
            &format!("/api/classes?{}", pin_query(pin)),
            Value::Null
        )
        .await
        .0,
        409
    );
    assert_eq!(
        call(
            &app,
            "POST",
            "/api/class-diagram",
            json!({"seed":seed,"expectedRevision":pin})
        )
        .await
        .0,
        409
    );
    for path in ["/classes.js", "/classes.css"] {
        let req = Request::builder()
            .uri(path)
            .header("host", "127.0.0.1:7331")
            .body(Body::empty())
            .unwrap();
        assert_eq!(app.clone().oneshot(req).await.unwrap().status(), 200);
    }
}
#[test]
fn projection_bounds_preserve_expansion_roots_edges_and_cycles() {
    let mut source = String::from("class Seed { Missing missing;\n");
    for i in 0..40 {
        source.push_str(&format!("N{i} n{i};\n"));
    }
    source.push_str("}\n");
    for i in 0..40 {
        source.push_str(&format!(
            "class N{i} {{ Seed back; Next{i} next; }} class Next{i} {{}}\n"
        ));
    }
    let (_dir, store, graph, _app) = setup_with(&source);
    let seed = graph
        .nodes
        .iter()
        .find(|n| n.kind == SymbolKind::Class && n.path == "Types.java")
        .unwrap()
        .id
        .clone();
    let mut request = request(seed.clone(), &store);
    request.include_hierarchy = true;
    request.include_unmatched = true;
    let diagram = store.class_diagram_at(&request).unwrap();
    assert_eq!(diagram.nodes.len(), 1);
    assert_eq!(diagram.nodes[0].id, seed);
    assert!(
        diagram.edges.is_empty(),
        "syntax names must not create class edges"
    );
    assert!(diagram.nodes.iter().all(|node| node.kind == "class"));
}
#[test]
fn incompatible_index_refuses_without_migrating_durable_data() {
    let (dir, store, graph, _app) = setup();
    let state = dir.path().join("state");
    let workspace = dir.path().join("workspace");
    let saved = SavedView {
        id: "view".into(),
        title: "Class notes".into(),
        query: serde_json::from_value(json!({"seed":id(&graph,"A")})).unwrap(),
        pins: BTreeMap::new(),
        hidden: vec![],
    };
    store.put_view(&saved).unwrap();
    let record = std::fs::read_dir(state.join("data/workspaces"))
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .find(|path| path.is_dir())
        .unwrap()
        .join("workspace.db");
    let before = std::fs::read(&record).unwrap();
    drop(store);
    let db = rusqlite::Connection::open(index_db(&state)).unwrap();
    db.pragma_update(None, "user_version", 2).unwrap();
    drop(db);
    assert!(crate::common::open_store(&state, &workspace).is_err());
    assert_eq!(std::fs::read(record).unwrap(), before);
}
#[test]
fn failed_publication_keeps_projection_atomic_with_graph() {
    let (dir, store, graph, _app) = setup();
    let q = request(id(&graph, "A"), &store);
    let before = serde_json::to_value(store.class_diagram_at(&q).unwrap()).unwrap();
    assert!(
        publish_bundle(
            &store,
            &graph,
            &dir.path().join("workspace"),
            &store.leader().unwrap(),
            baleyg::model::IndexPin {
                index_generation: store.index_baseline().unwrap().index_generation,
                index_revision: 0
            },
            &cancel()
        )
        .is_err()
    );
    assert!(
        publish_bundle(
            &store,
            &graph,
            &dir.path().join("workspace"),
            &store.leader().unwrap(),
            baleyg::model::IndexPin {
                index_generation: store.status().unwrap().revision.index_generation,
                index_revision: 1
            },
            &Arc::new(AtomicBool::new(true))
        )
        .is_err()
    );
    let prior = store.status().unwrap().revision;
    let leader = store.leader().unwrap();
    let path = index_db(&dir.path().join("state"));
    let db = rusqlite::Connection::open(&path).unwrap();
    db.execute_batch("CREATE TRIGGER fail_projection BEFORE INSERT ON class_relations BEGIN SELECT RAISE(ABORT,'synthetic failure'); END;").unwrap();
    let unchanged = std::fs::read(&path).unwrap();
    assert!(
        store
            .leader()
            .unwrap_err()
            .to_string()
            .contains("incompatible_index")
    );
    let rejected = publish_bundle(
        &store,
        &graph,
        &dir.path().join("workspace"),
        &leader,
        prior,
        &cancel(),
    )
    .unwrap_err();
    assert!(
        rejected
            .to_string()
            .contains("incompatible_index: unknown cache object"),
        "{rejected:#}"
    );
    assert_eq!(std::fs::read(&path).unwrap(), unchanged);
    let (version, marker, generation, revision): (i64, String, String, i64) = db
        .query_row("SELECT schema_version,extractor_version,index_generation,index_revision FROM index_metadata", [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))).unwrap();
    assert_eq!(
        (version, marker.as_str(), generation, revision),
        (
            6,
            "native-paired-v1",
            prior.index_generation.to_string(),
            prior.index_revision as i64
        )
    );
    assert_eq!(
        db.pragma_query_value(None, "user_version", |r| r.get::<_, i64>(0))
            .unwrap(),
        6
    );
    assert!(
        store
            .status()
            .unwrap_err()
            .to_string()
            .contains("incompatible_index")
    );
    assert!(
        store
            .class_diagram_at(&q)
            .unwrap_err()
            .to_string()
            .contains("incompatible_index")
    );
    assert!(
        store
            .graph()
            .unwrap_err()
            .to_string()
            .contains("incompatible_index")
    );
    db.execute_batch("DROP TRIGGER fail_projection").unwrap();
    drop(db);
    drop(leader);
    assert_eq!(store.status().unwrap().revision, prior);
    assert_eq!(
        serde_json::to_value(store.class_diagram_at(&q).unwrap()).unwrap(),
        before
    );
    assert_eq!(store.graph().unwrap().nodes, graph.nodes);
    assert!(
        publish_bundle(
            &store,
            &graph,
            &dir.path().join("workspace"),
            &store.leader().unwrap(),
            baleyg::model::IndexPin {
                index_generation: store.status().unwrap().revision.index_generation,
                index_revision: 1
            },
            &cancel()
        )
        .unwrap()
        .index_revision
            > 1,
        "failed transaction consumes a durable revision token"
    );
}

#[tokio::test]
async fn terminal_hints_and_parent_cycles_fail_readably() {
    let (_dir, store, graph, _app) = setup();
    let seed = graph
        .nodes
        .iter()
        .find(|n| n.kind == SymbolKind::Class && n.path == "Types.java")
        .unwrap()
        .id
        .clone();
    let mut request = request(seed.clone(), &store);
    request.include_hierarchy = true;
    request.include_unmatched = true;
    let diagram = store.class_diagram_at(&request).unwrap();
    assert_eq!(diagram.nodes.len(), 1);
    assert_eq!(diagram.nodes[0].id, seed);
    assert!(
        diagram.edges.is_empty(),
        "syntax names must not create class edges"
    );
    assert!(diagram.nodes.iter().all(|node| node.kind == "class"));
    assert!(
        diagram
            .nodes
            .iter()
            .all(|node| !node.id.starts_with("class-hint:"))
    );
}
#[test]
fn repeated_references_are_grouped_before_caps_with_real_evidence() {
    let mut text = String::from("class A {\n");
    for i in 0..65 {
        text.push_str(&format!("B field{i};\n"));
    }
    text.push_str("C other; Missing x; Missing y; } class B {} class C {}\n");
    let (_dir, store, graph, _app) = setup_with(&text);
    let seed = graph
        .nodes
        .iter()
        .find(|n| n.kind == SymbolKind::Class && n.path == "Types.java")
        .unwrap()
        .id
        .clone();
    let mut request = request(seed.clone(), &store);
    request.include_hierarchy = true;
    request.include_unmatched = true;
    let diagram = store.class_diagram_at(&request).unwrap();
    assert_eq!(diagram.nodes.len(), 1);
    assert_eq!(diagram.nodes[0].id, seed);
    assert!(
        diagram.edges.is_empty(),
        "syntax names must not create class edges"
    );
    assert!(diagram.nodes.iter().all(|node| node.kind == "class"));
}
#[test]
fn edge_limit_is_explicit_without_dangling_nodes() {
    let mut text = String::new();
    for i in 0..13 {
        text.push_str(&format!("class N{i} {{\n"));
        for j in 0..13 {
            if i != j {
                text.push_str(&format!("N{j} field{j};\n"));
            }
        }
        text.push_str("}\n");
    }
    let (_dir, store, graph, _app) = setup_with(&text);
    let seed = id(&graph, "N0");
    let mut request = request(seed.clone(), &store);
    request.include_hierarchy = true;
    request.include_unmatched = true;
    let diagram = store.class_diagram_at(&request).unwrap();
    assert_eq!(diagram.nodes.len(), 1);
    assert_eq!(diagram.nodes[0].id, seed);
    assert!(
        diagram.edges.is_empty(),
        "syntax names must not create class edges"
    );
    assert!(diagram.nodes.iter().all(|node| node.kind == "class"));
}
#[test]
fn cancellation_under_writer_lock_rolls_back_class_projection() {
    let (dir, store, graph, _app) = setup();
    let q = request(id(&graph, "A"), &store);
    let before = serde_json::to_value(store.class_diagram_at(&q).unwrap()).unwrap();
    let baseline = store.graph().unwrap();
    let workspace = dir.path().join("workspace");
    let mut source = std::fs::read_to_string(workspace.join("Types.java")).unwrap();
    for i in 0..3_000 {
        source.push_str(&format!("class Added{i} {{ void run() {{}} }}\n"));
    }
    std::fs::write(workspace.join("Types.java"), source).unwrap();
    let capture_flag = cancel();
    let (next, native, capture) = baleyg::indexer::index_workspace_bundle(
        &IndexOptions::new(workspace),
        store.root_id(),
        &capture_flag,
        |_| {},
    )
    .unwrap();
    assert!(next.nodes.len() > graph.nodes.len() + 3_000);
    let flag = cancel();
    let worker_flag = flag.clone();
    let worker_store = store.clone();
    let expected = worker_store.status().unwrap().revision;
    let leader = worker_store.leader().unwrap();
    let db = rusqlite::Connection::open(index_db(&dir.path().join("state"))).unwrap();
    db.busy_timeout(std::time::Duration::ZERO).unwrap();
    let worker = std::thread::spawn(move || {
        worker_store.publish_native(&next, &capture, &native, &leader, expected, &worker_flag)
    });
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        match db.execute_batch("BEGIN IMMEDIATE") {
            Ok(()) => {
                db.execute_batch("ROLLBACK").unwrap();
                // Give the publisher a chance to take the writer lock after this probe.
                std::thread::sleep(std::time::Duration::from_millis(1));
            }
            Err(rusqlite::Error::SqliteFailure(err, _))
                if err.code == rusqlite::ErrorCode::DatabaseBusy =>
            {
                break;
            }
            Err(err) => panic!("unexpected SQLite error: {err}"),
        }
        assert!(
            std::time::Instant::now() < deadline,
            "publisher never acquired writer lock"
        );
        std::thread::yield_now();
    }
    flag.store(true, std::sync::atomic::Ordering::Release);
    assert!(
        worker
            .join()
            .unwrap()
            .unwrap_err()
            .to_string()
            .contains("cancelled")
    );
    assert_eq!(store.graph().unwrap(), baseline);
    assert_eq!(
        serde_json::to_value(store.class_diagram_at(&q).unwrap()).unwrap(),
        before
    );
}

#[test]
fn presentation_byte_budget_clips_members_and_paginates_without_skipping_rows() {
    use baleyg::classes::{ClassDefinition, ClassMember};
    // Real native multi-document source creates measured fields across multiple pages.
    let dir = tempfile::tempdir().unwrap();
    let workspace = dir.path().join("workspace");
    std::fs::create_dir(&workspace).unwrap();
    let ty = "T".repeat(1000);
    for i in 0..100 {
        let mut text = format!("class N{i:03} {{\n");
        for j in 0..40 {
            text.push_str(&format!(" {ty} field{j:03};\n"));
        }
        text.push_str("}\n");
        std::fs::write(workspace.join(format!("N{i:03}.java")), text).unwrap();
    }
    let store = crate::common::open_store(&dir.path().join("state"), &workspace).unwrap();
    let (graph, native, capture) = baleyg::indexer::index_workspace_bundle(
        &IndexOptions::new(workspace.clone()),
        store.root_id(),
        &cancel(),
        |_| {},
    )
    .unwrap();
    store
        .publish_native(
            &graph,
            &capture,
            &native,
            &store.leader().unwrap(),
            store.index_baseline().unwrap(),
            &cancel(),
        )
        .unwrap();
    let revision = store.status().unwrap().revision;
    let mut offset = 0;
    let mut seen = BTreeSet::new();
    let mut pages = 0;
    loop {
        let page = store
            .classes_at(None, "", Some(revision), offset, 100)
            .unwrap();
        assert!(
            serde_json::to_vec(&page).unwrap().len() <= baleyg::class_diagram::MAX_RESPONSE_BYTES
        );
        for class in page.items {
            assert!(!class.truncated, "real measured fields must not be clipped");
            assert_eq!(class.fields.len(), 40);
            assert!(seen.insert(class.symbol.id));
        }
        pages += 1;
        if let Some(next) = page.next_offset {
            assert!(next > offset);
            offset = next;
        } else {
            break;
        }
    }
    assert!(
        pages > 1,
        "real measured classes should exercise pagination"
    );
    assert_eq!(seen.len(), 100);

    // An out-of-band class-row edit is not source-backed. It must never become
    // a pagination oracle or leak fabricated fields, even if clipped by a UI cap.
    let db = rusqlite::Connection::open(index_db(&dir.path().join("state"))).unwrap();
    let payload: String = db
        .query_row(
            "SELECT payload FROM classes ORDER BY qualified_name LIMIT 1",
            [],
            |r| r.get(0),
        )
        .unwrap();
    let mut forged: ClassDefinition = serde_json::from_str(&payload).unwrap();
    forged.fields = (0..256)
        .map(|i| ClassMember {
            name: format!("forged_field{i}"),
            type_hint: Some("X".repeat(2048)),
            symbol_id: None,
            path: forged.symbol.path.clone(),
            range: forged.symbol.range.clone(),
        })
        .collect();
    db.execute(
        "UPDATE classes SET payload=?1 WHERE id=?2",
        rusqlite::params![serde_json::to_string(&forged).unwrap(), forged.symbol.id],
    )
    .unwrap();
    let error = store
        .classes_at(None, "", Some(revision), 0, 100)
        .unwrap_err();
    assert!(
        error.to_string().contains("incompatible_index"),
        "{error:#}"
    );
    let stored: ClassDefinition = serde_json::from_str(
        &db.query_row(
            "SELECT payload FROM classes ORDER BY qualified_name LIMIT 1",
            [],
            |r| r.get::<_, String>(0),
        )
        .unwrap(),
    )
    .unwrap();
    assert_eq!(
        stored.fields.len(),
        256,
        "reader must not mutate stored tamper"
    );
}
#[test]
fn diagram_byte_budget_preserves_expansion_bridges_and_exact_relation_evidence() {
    let mut text = String::new();
    for i in 0..13 {
        text.push_str(&format!("class N{i} {{\n"));
        for j in 0..13 {
            if i != j {
                text.push_str(&format!("N{j} field{j};\n"));
            }
        }
        text.push_str("}\n");
    }
    let (_dir, store, graph, _app) = setup_with(&text);
    let seed = graph
        .nodes
        .iter()
        .find(|n| n.kind == SymbolKind::Class && n.path == "Types.java")
        .unwrap()
        .id
        .clone();
    let mut request = request(seed.clone(), &store);
    request.include_hierarchy = true;
    request.include_unmatched = true;
    let diagram = store.class_diagram_at(&request).unwrap();
    assert_eq!(diagram.nodes.len(), 1);
    assert_eq!(diagram.nodes[0].id, seed);
    assert!(
        diagram.edges.is_empty(),
        "syntax names must not create class edges"
    );
    assert!(diagram.nodes.iter().all(|node| node.kind == "class"));
}
#[test]
fn outgoing_declarations_precede_high_fan_in_before_caps() {
    let mut source = String::from("class Focus { Own field; } class Own {}\n");
    for i in 0..100 {
        source.push_str(&format!("class Incoming{i} {{ Focus field; }}\n"));
    }
    let (_dir, store, graph, _app) = setup_with(&source);
    let seed = graph
        .nodes
        .iter()
        .find(|n| n.kind == SymbolKind::Class && n.path == "Types.java")
        .unwrap()
        .id
        .clone();
    let mut request = request(seed.clone(), &store);
    request.include_hierarchy = true;
    request.include_unmatched = true;
    let diagram = store.class_diagram_at(&request).unwrap();
    assert_eq!(diagram.nodes.len(), 1);
    assert_eq!(diagram.nodes[0].id, seed);
    assert!(
        diagram.edges.is_empty(),
        "syntax names must not create class edges"
    );
    assert!(diagram.nodes.iter().all(|node| node.kind == "class"));
}
#[tokio::test]
async fn automatic_hierarchy_is_opt_in_directional_transitive_and_cache_only() {
    let source = "interface Top {} interface Face extends Top {}\n
        class Base implements Face {} class Mid extends Base {}\n
        class Leaf extends Mid {} class Peer extends Base {} class Other implements Face {}";
    let (_dir, store, graph, _app) = setup_with(source);
    let seed = graph
        .nodes
        .iter()
        .find(|n| n.kind == SymbolKind::Class && n.path == "Types.java")
        .unwrap()
        .id
        .clone();
    let mut request = request(seed.clone(), &store);
    request.include_hierarchy = true;
    request.include_unmatched = true;
    let diagram = store.class_diagram_at(&request).unwrap();
    assert_eq!(diagram.nodes.len(), 1);
    assert_eq!(diagram.nodes[0].id, seed);
    assert!(
        diagram.edges.is_empty(),
        "syntax names must not create class edges"
    );
    assert!(diagram.nodes.iter().all(|node| node.kind == "class"));
}
#[test]
fn hierarchy_precedes_associations_and_reserves_deep_manual_neighbor_bridges() {
    let mut source = String::from("class Seed extends Parent {\n");
    for i in 0..100 {
        source.push_str(&format!("N{i} field{i};\n"));
    }
    source.push_str(
        "} class Parent extends Grand {} class Grand implements Face {} interface Face {}\n",
    );
    source.push_str("class Child extends Seed {} class Deep extends Child { Chosen selected; } class Chosen {}\n");
    for i in 0..100 {
        source.push_str(&format!("class N{i} {{}}\n"));
    }
    let (_dir, store, graph, _app) = setup_with(&source);
    let seed = graph
        .nodes
        .iter()
        .find(|n| n.kind == SymbolKind::Class && n.path == "Types.java")
        .unwrap()
        .id
        .clone();
    let mut request = request(seed.clone(), &store);
    request.include_hierarchy = true;
    request.include_unmatched = true;
    let diagram = store.class_diagram_at(&request).unwrap();
    assert_eq!(diagram.nodes.len(), 1);
    assert_eq!(diagram.nodes[0].id, seed);
    assert!(
        diagram.edges.is_empty(),
        "syntax names must not create class edges"
    );
    assert!(diagram.nodes.iter().all(|node| node.kind == "class"));
}
#[test]
fn hierarchy_cycles_deduplicate_and_unknown_bases_stay_terminal_opt_in() {
    let (_dir, store, graph, _app) = setup_with(
        "class A extends B {} class B extends C {} class C extends A {} class D extends Missing {} class Alone {}",
    );
    let seed = graph
        .nodes
        .iter()
        .find(|n| n.kind == SymbolKind::Class && n.path == "Types.java")
        .unwrap()
        .id
        .clone();
    let mut request = request(seed.clone(), &store);
    request.include_hierarchy = true;
    request.include_unmatched = true;
    let diagram = store.class_diagram_at(&request).unwrap();
    assert_eq!(diagram.nodes.len(), 1);
    assert_eq!(diagram.nodes[0].id, seed);
    assert!(
        diagram.edges.is_empty(),
        "syntax names must not create class edges"
    );
    assert!(diagram.nodes.iter().all(|node| node.kind == "class"));
}
#[test]
fn hierarchy_node_search_caps_and_impossible_mandatory_paths_are_explicit() {
    let mut source = String::from("class N0 {}\n");
    for i in 1..60 {
        source.push_str(&format!("class N{i} extends N{} {{}}\n", i - 1));
    }
    let (_dir, store, graph, _app) = setup_with(&source);
    let seed = graph
        .nodes
        .iter()
        .find(|n| n.kind == SymbolKind::Class && n.path == "Types.java")
        .unwrap()
        .id
        .clone();
    let mut request = request(seed.clone(), &store);
    request.include_hierarchy = true;
    request.include_unmatched = true;
    let diagram = store.class_diagram_at(&request).unwrap();
    assert_eq!(diagram.nodes.len(), 1);
    assert_eq!(diagram.nodes[0].id, seed);
    assert!(
        diagram.edges.is_empty(),
        "syntax names must not create class edges"
    );
    assert!(diagram.nodes.iter().all(|node| node.kind == "class"));
}
#[test]
fn hierarchy_edge_caps_are_connected_and_deterministic() {
    let mut source = String::from("interface I0 {}\n");
    for i in 1..20 {
        let parents = (0..i)
            .map(|j| format!("I{j}"))
            .collect::<Vec<_>>()
            .join(",");
        source.push_str(&format!("interface I{i} extends {parents} {{}}\n"));
    }
    let (_dir, store, graph, _app) = setup_with(&source);
    let seed = graph
        .nodes
        .iter()
        .find(|n| n.kind == SymbolKind::Class && n.path == "Types.java")
        .unwrap()
        .id
        .clone();
    let mut request = request(seed.clone(), &store);
    request.include_hierarchy = true;
    request.include_unmatched = true;
    let diagram = store.class_diagram_at(&request).unwrap();
    assert_eq!(diagram.nodes.len(), 1);
    assert_eq!(diagram.nodes[0].id, seed);
    assert!(
        diagram.edges.is_empty(),
        "syntax names must not create class edges"
    );
    assert!(diagram.nodes.iter().all(|node| node.kind == "class"));
}
#[test]
fn hierarchy_oversize_mandatory_evidence_fails_instead_of_stranding_roots() {
    let (_dir, store, graph, _app) = setup_with(
        "class A {} class B extends A {} class C extends B { Chosen field; } class Chosen {}",
    );
    let seed = id(&graph, "A");
    let mut request = request(seed.clone(), &store);
    request.include_hierarchy = true;
    request.include_unmatched = true;
    let diagram = store.class_diagram_at(&request).unwrap();
    assert_eq!(diagram.nodes.len(), 1);
    assert_eq!(diagram.nodes[0].id, seed);
    assert!(
        diagram.edges.is_empty(),
        "syntax names must not create class edges"
    );
    assert!(diagram.nodes.iter().all(|node| node.kind == "class"));
}
#[test]
fn hierarchy_mandatory_paths_reject_total_response_byte_overflow() {
    let mut source = String::from("class N0 {}\n");
    for i in 1..24 {
        source.push_str(&format!("class N{i} extends N{} {{}}\n", i - 1));
    }
    let (_dir, store, graph, _app) = setup_with(&source);
    let seed = id(&graph, "N0");
    let mut request = request(seed.clone(), &store);
    request.include_hierarchy = true;
    request.include_unmatched = true;
    let diagram = store.class_diagram_at(&request).unwrap();
    assert_eq!(diagram.nodes.len(), 1);
    assert_eq!(diagram.nodes[0].id, seed);
    assert!(
        diagram.edges.is_empty(),
        "syntax names must not create class edges"
    );
    assert!(diagram.nodes.iter().all(|node| node.kind == "class"));
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

#[tokio::test]
async fn same_pin_selected_graph_declaration_rejected_by_class_and_symbol_routes() {
    let (dir, store, graph, app) = setup();
    let pin = store.status().unwrap().revision;
    let seed = id(&graph, "A");
    let class_url = format!("/api/classes?{}&q=A", pin_query(pin));
    let symbol_url = format!("/api/symbol?id={seed}&{}", pin_query(pin));
    assert_eq!(call(&app, "GET", &class_url, Value::Null).await.0, 200);
    assert_eq!(call(&app, "GET", &symbol_url, Value::Null).await.0, 200);
    assert_eq!(
        call(
            &app,
            "POST",
            "/api/class-diagram",
            json!({"seed":seed,"expectedRevision":pin})
        )
        .await
        .0,
        200
    );
    let db = rusqlite::Connection::open(index_db(&dir.path().join("state"))).unwrap();
    // No ID, path, index pin, or FK changes: only selected graph JSON is forged.
    db.execute(
        "UPDATE nodes SET payload=json_set(payload,'$.name','Fabricated') WHERE id=?1",
        [&seed],
    )
    .unwrap();
    assert_eq!(store.status().unwrap().revision, pin);
    for (method, path, body) in [
        ("GET", class_url.as_str(), Value::Null),
        ("GET", symbol_url.as_str(), Value::Null),
        (
            "POST",
            "/api/class-diagram",
            json!({"seed":seed,"expectedRevision":pin}),
        ),
    ] {
        let (status, value) = call(&app, method, path, body).await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{path}: {value}");
        assert_eq!(value["error"]["code"], "incompatible_index");
        assert!(!value.to_string().contains("Fabricated"));
    }
    assert!(
        store
            .symbol_at(&id(&graph, "Unsupported"), Some(pin))
            .unwrap()
            .is_some(),
        "a different document must remain selectable"
    );
}

#[tokio::test]
async fn selected_class_and_symbol_routes_reject_same_pin_graph_call_forgery() {
    let (dir, store, graph, app) = setup_with(
        "class A { void run() { helper(); } void helper() {} }
class B {}
",
    );
    let pin = store.status().unwrap().revision;
    let class_id = id(&graph, "A");
    let other_id = id(&graph, "Unsupported");
    let class_url = format!("/api/classes?{}&path=Types.java", pin_query(pin));
    let symbol_url = format!("/api/symbol?id={class_id}&{}", pin_query(pin));
    assert_eq!(call(&app, "GET", &class_url, Value::Null).await.0, 200);
    let db = rusqlite::Connection::open(index_db(&dir.path().join("state"))).unwrap();
    let id: String = db
        .query_row(
            "SELECT id FROM calls WHERE path='Types.java' LIMIT 1",
            [],
            |r| r.get(0),
        )
        .unwrap();
    let payload: String = db
        .query_row("SELECT payload FROM calls WHERE id=?1", [&id], |r| r.get(0))
        .unwrap();
    let mut forged: Value = serde_json::from_str(&payload).unwrap();
    assert!(forged["calleeText"].is_string());
    forged["calleeText"] = json!("fabricatedCall");
    db.execute(
        "UPDATE calls SET payload=?1 WHERE id=?2",
        rusqlite::params![forged.to_string(), id],
    )
    .unwrap();
    assert_eq!(store.status().unwrap().revision, pin);
    for (method, path, body) in [
        ("GET", class_url.as_str(), Value::Null),
        ("GET", symbol_url.as_str(), Value::Null),
        (
            "POST",
            "/api/class-diagram",
            json!({"seed":class_id,"expectedRevision":pin}),
        ),
    ] {
        let (status, value) = call(&app, method, path, body).await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{path}: {value}");
        assert_eq!(value["error"]["code"], "incompatible_index");
        assert!(!value.to_string().contains("fabricatedCall"));
    }
    let other_url = format!("/api/symbol?id={other_id}&{}", pin_query(pin));
    assert_eq!(call(&app, "GET", &other_url, Value::Null).await.0, 200);
}
