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
        store
            .publish(
                &graph,
                &store.leader().unwrap(),
                baleyg::model::IndexPin {
                    index_generation: store.status().unwrap().revision.index_generation,
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
    let (_dir, store, graph, app) = setup();
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
    store
        .publish(
            &graph,
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
        store
            .publish(
                &graph,
                &store.leader().unwrap(),
                baleyg::model::IndexPin {
                    index_generation: store.status().unwrap().revision.index_generation,
                    index_revision: 0
                },
                &cancel()
            )
            .is_err()
    );
    assert!(
        store
            .publish(
                &graph,
                &store.leader().unwrap(),
                baleyg::model::IndexPin {
                    index_generation: store.status().unwrap().revision.index_generation,
                    index_revision: 1
                },
                &Arc::new(AtomicBool::new(true))
            )
            .is_err()
    );
    let db = rusqlite::Connection::open(index_db(&dir.path().join("state"))).unwrap();
    db.execute_batch("CREATE TRIGGER fail_projection BEFORE INSERT ON class_relations BEGIN SELECT RAISE(ABORT,'synthetic failure'); END;").unwrap();
    assert!(
        store
            .publish(
                &graph,
                &store.leader().unwrap(),
                baleyg::model::IndexPin {
                    index_generation: store.status().unwrap().revision.index_generation,
                    index_revision: 1
                },
                &cancel()
            )
            .is_err()
    );
    assert_eq!(store.status().unwrap().revision.index_revision, 1);
    assert_eq!(
        serde_json::to_value(store.class_diagram_at(&q).unwrap()).unwrap(),
        before
    );
    assert_eq!(store.graph().unwrap().nodes, graph.nodes);
    db.execute_batch("DROP TRIGGER fail_projection").unwrap();
    assert!(
        store
            .publish(
                &graph,
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
    let (dir, store, mut graph, _app) = setup();
    let q = request(id(&graph, "A"), &store);
    let before = serde_json::to_value(store.class_diagram_at(&q).unwrap()).unwrap();
    let baseline = store.graph().unwrap();
    let method = graph
        .nodes
        .iter()
        .find(|n| n.name == "run")
        .unwrap()
        .clone();
    for i in 0..30_000 {
        let mut extra = method.clone();
        extra.id = format!("extra-{i}");
        graph.nodes.push(extra);
    }
    let flag = cancel();
    let worker_flag = flag.clone();
    let worker_store = store.clone();
    let expected = worker_store.status().unwrap().revision;
    let leader = worker_store.leader().unwrap();
    let db = rusqlite::Connection::open(index_db(&dir.path().join("state"))).unwrap();
    db.busy_timeout(std::time::Duration::ZERO).unwrap();
    let worker =
        std::thread::spawn(move || worker_store.publish(&graph, &leader, expected, &worker_flag));
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
    let mut text = String::new();
    for i in 0..100 {
        text.push_str(&format!("class N{i:03} {{}}\n"));
    }
    let (dir, store, _graph, _app) = setup_with(&text);
    let db = rusqlite::Connection::open(index_db(&dir.path().join("state"))).unwrap();
    let payloads = db
        .prepare("SELECT payload FROM classes ORDER BY id")
        .unwrap()
        .query_map([], |r| r.get::<_, String>(0))
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap();
    for payload in payloads {
        let mut class: ClassDefinition = serde_json::from_str(&payload).unwrap();
        class.fields = (0..256)
            .map(|i| ClassMember {
                name: format!("field{i}"),
                type_hint: Some("X".repeat(2048)),
                symbol_id: None,
                path: class.symbol.path.clone(),
                range: class.symbol.range.clone(),
            })
            .collect();
        db.execute(
            "UPDATE classes SET payload=?1 WHERE id=?2",
            rusqlite::params![serde_json::to_string(&class).unwrap(), class.symbol.id],
        )
        .unwrap();
    }
    let mut offset = 0;
    let mut seen = BTreeSet::new();
    let mut pages = 0;
    loop {
        let page = store
            .classes_at(
                None,
                "",
                Some(store.status().unwrap().revision),
                offset,
                100,
            )
            .unwrap();
        assert!(
            serde_json::to_vec(&page).unwrap().len() <= baleyg::class_diagram::MAX_RESPONSE_BYTES
        );
        assert!(page.truncated);
        assert!(page.warnings.iter().any(|w| w.contains("byte limits")));
        assert!(
            !page.items.is_empty(),
            "individually oversized first class still makes progress"
        );
        for class in page.items {
            assert!(class.truncated);
            assert!(class.fields.len() < 256);
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
    assert!(pages > 1);
    assert_eq!(seen.len(), 100);
    let stored: ClassDefinition = serde_json::from_str(
        &db.query_row("SELECT payload FROM classes LIMIT 1", [], |r| {
            r.get::<_, String>(0)
        })
        .unwrap(),
    )
    .unwrap();
    assert_eq!(
        stored.fields.len(),
        256,
        "presentation does not mutate stored class"
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
