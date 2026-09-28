//! Synthetic cached-only navigation. No providers or repository programs are executed.
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
use std::sync::{Arc, atomic::AtomicBool};
use tower::ServiceExt;
const TOKEN: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
const JAVA: &str = "package demo;\nclass A {\n B first, second;\n Missing unknown;\n int count;\n B run(B input) { return input; }\n C run(C input) { return input; }\n class Inner { C own; }\n}\n";
fn cancel() -> CancelFlag {
    Arc::new(AtomicBool::new(false))
}
fn setup(files: &[(&str, &str)]) -> (tempfile::TempDir, Store, Graph, Router) {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("workspace");
    std::fs::create_dir(&root).unwrap();
    for (path, text) in files {
        std::fs::write(root.join(path), text).unwrap();
    }
    let options = IndexOptions::new(root.clone());
    let graph = index_workspace(&options, &cancel(), |_| {}).unwrap();
    let store = crate::common::open_store(&dir.path().join("state"), &root).unwrap();
    publish_bundle(
        &store,
        &graph,
        &root,
        &store.leader().unwrap(),
        baleyg::model::IndexPin {
            index_generation: store.index_baseline().unwrap().index_generation,
            index_revision: 0,
        },
        &cancel(),
    )
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
fn fixture() -> (tempfile::TempDir, Store, Graph, Router) {
    setup(&[
        ("A.java", JAVA),
        ("B.java", "package demo; class B {}"),
        ("C.java", "package demo; class C {}"),
    ])
}
fn index_db(dir: &tempfile::TempDir) -> std::path::PathBuf {
    std::fs::read_dir(dir.path().join("state/cache/indexes"))
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .find(|path| path.is_dir())
        .unwrap()
        .join("index.db")
}
fn pin(dir: &tempfile::TempDir) -> Value {
    let db = rusqlite::Connection::open(index_db(dir)).unwrap();
    db.query_row("SELECT index_generation,index_revision FROM index_metadata", [], |row| {
        Ok(json!({"indexGeneration":row.get::<_,String>(0)?,"indexRevision":row.get::<_,i64>(1)?}))
    }).unwrap()
}
fn source(path: &str, line: usize, dir: &tempfile::TempDir) -> Value {
    json!({"expectedRevision":pin(dir),"path":path,"line":line})
}
fn id<'a>(g: &'a Graph, name: &str) -> &'a str {
    &g.nodes.iter().find(|s| s.name == name).unwrap().id
}
fn member(dir: &tempfile::TempDir, class: &str, name: &str, ordinal: usize) -> Value {
    let db = rusqlite::Connection::open(index_db(dir)).unwrap();
    let payload: String = db
        .query_row("SELECT payload FROM classes WHERE id=?1", [class], |r| {
            r.get(0)
        })
        .unwrap();
    let c: Value = serde_json::from_str(&payload).unwrap();
    let m = c["fields"]
        .as_array()
        .unwrap()
        .iter()
        .chain(c["methods"].as_array().unwrap())
        .filter(|m| m["name"] == name)
        .nth(ordinal)
        .unwrap();
    json!({"expectedRevision":pin(dir),"classId":class,"memberName":name,"startByte":m["range"]["startByte"],"endByte":m["range"]["endByte"]})
}
async fn call(app: &Router, body: Value) -> (StatusCode, Value) {
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/navigation")
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
    let bytes = to_bytes(response.into_body(), 512 * 1024).await.unwrap();
    (status, serde_json::from_slice(&bytes).unwrap())
}
fn targets(v: &Value, reason: &str) -> Vec<String> {
    v["targets"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|t| t["reason"] == reason)
        .map(|t| t["symbol"]["name"].as_str().unwrap().into())
        .collect()
}
#[tokio::test]
async fn exact_members_overloads_shared_fields_and_crossfile_types() {
    let (dir, _store, graph, app) = fixture();
    for field in ["first", "second"] {
        let (status, view) = call(&app, member(&dir, id(&graph, "A"), field, 0)).await;
        assert_eq!(status, 200, "{view}");
        assert!(
            targets(&view, "type").is_empty(),
            "declared type is terminal text"
        );
    }
    for (ordinal, expected) in ["B", "C"].into_iter().enumerate() {
        let selector = member(&dir, id(&graph, "A"), "run", ordinal);
        let (_, view) = call(&app, selector.clone()).await;
        assert!(
            targets(&view, "type").is_empty(),
            "{expected} is not a proven type target"
        );
        assert_eq!(targets(&view, "declaration"), ["run"]);
        let method = view["targets"]
            .as_array()
            .unwrap()
            .iter()
            .find(|t| t["action"] == "sequence")
            .unwrap();
        assert_eq!(
            method["symbol"]["range"]["startByte"],
            selector["startByte"]
        );
        assert_eq!(method["symbol"]["range"]["endByte"], selector["endByte"]);
    }
    for field in ["unknown", "count"] {
        let (_, view) = call(&app, member(&dir, id(&graph, "A"), field, 0)).await;
        assert!(view["targets"].as_array().unwrap().is_empty());
        assert!(targets(&view, "type").is_empty());
    }
}
#[tokio::test]
async fn cached_only_source_lines_utf8_and_measured_ranges() {
    let (dir, store, graph, app) = setup(&[(
        "unicode.py",
        "class Café:\n    def méthode(self):\n        # π 😀\n        return 1\n\n",
    )]);
    let (_, view) = call(&app, source("unicode.py", 3, &dir)).await;
    assert_eq!(targets(&view, "enclosing"), ["Café", "méthode"]);
    for target in view["targets"].as_array().unwrap() {
        let measured = graph
            .nodes
            .iter()
            .find(|s| s.id == target["symbol"]["id"])
            .unwrap();
        assert_eq!(target["symbol"], serde_json::to_value(measured).unwrap());
    }
    let (_, method) = call(&app, source("unicode.py", 2, &dir)).await;
    assert_eq!(targets(&method, "declaration"), ["méthode"]);
    std::fs::remove_dir_all(dir.path().join("workspace")).unwrap();
    assert_eq!(call(&app, source("unicode.py", 3, &dir)).await.0, 409);
    assert!(store.graph().is_err());
}
#[tokio::test]
async fn strict_selectors_validation_and_revision() {
    let (dir, store, graph, app) = fixture();
    let good = member(&dir, id(&graph, "A"), "first", 0);
    let mut wrong_range = good.clone();
    wrong_range["startByte"] = json!(0);
    let mut wrong_name = good.clone();
    wrong_name["memberName"] = json!("unknown");
    let mut wrong_owner = good.clone();
    wrong_owner["classId"] = json!(id(&graph, "B"));
    for body in [
        json!({}),
        json!({"path":"A.java","line":3}),
        json!({"expectedRevision":1,"path":"A.java","line":3,"classId":null}),
        json!({"expectedRevision":1,"path":"A.java","line":3,"typeHint":"B"}),
        json!({"expectedRevision":1,"path":"A.java","line":3,"classId":"A","memberName":"first","startByte":0,"endByte":1}),
        source("../A.java", 1, &dir),
        source("/A.java", 1, &dir),
        source("A.java", 0, &dir),
        source("missing", 1, &dir),
        source("A.java", 999, &dir),
        wrong_range,
        wrong_name,
        wrong_owner,
    ] {
        let (status, value) = call(&app, body.clone()).await;
        assert_eq!(status, 400, "{body}: {value}");
    }
    let old_source = source("A.java", 3, &dir);
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
    assert_eq!(call(&app, good).await.0, 409);
    assert_eq!(call(&app, old_source).await.0, 409);
}
#[tokio::test]
async fn auth_host_origin_are_enforced() {
    let (dir, _store, _graph, app) = fixture();
    for (host, token, origin, expected) in [
        ("127.0.0.1:7331", "", "", 401),
        ("evil.example", TOKEN, "", 403),
        ("127.0.0.1:7331", TOKEN, "https://evil.example", 403),
    ] {
        let mut req = Request::builder()
            .method("POST")
            .uri("/api/navigation")
            .header("host", host)
            .header("content-type", "application/json");
        if !token.is_empty() {
            req = req.header("authorization", format!("Bearer {token}"));
        }
        if !origin.is_empty() {
            req = req.header("origin", origin);
        }
        let response = app
            .clone()
            .oneshot(
                req.body(Body::from(source("A.java", 3, &dir).to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), expected);
    }
}
#[tokio::test]
async fn ambiguity_preserves_all_cached_candidates_and_unresolved_calls_are_not_guessed() {
    let (dir, _store, graph, app) = setup(&[
        (
            "a.py",
            "class A:\n    value: B\nclass B: pass\nclass B: pass\n",
        ),
        (
            "calls.py",
            "def target():\n    pass\ndef caller(obj):\n    target()\n    obj.target()\n",
        ),
    ]);
    let measured = graph
        .nodes
        .iter()
        .find(|n| n.kind == SymbolKind::Class)
        .unwrap();
    let (status, declaration) = call(
        &app,
        source(&measured.path, measured.range.start_line, &dir),
    )
    .await;
    assert_eq!(status, 200);
    assert!(
        declaration["targets"]
            .as_array()
            .unwrap()
            .iter()
            .all(
                |t| matches!(t["reason"].as_str(), Some("declaration" | "enclosing"))
                    && t["matchKind"] == "measured"
            )
    );
    if let Some(callsite) = graph.calls.first() {
        let (status, line) = call(
            &app,
            source(&callsite.path, callsite.range.start_line, &dir),
        )
        .await;
        assert_eq!(status, 200);
        assert!(
            line["targets"]
                .as_array()
                .unwrap()
                .iter()
                .all(|t| t["reason"] != "call" && t["reason"] != "type")
        );
    }
}
#[tokio::test]
async fn old_projection_guidance_and_large_payloads_are_bounded() {
    let (dir, _store, graph, app) = fixture();
    let selector = member(&dir, id(&graph, "A"), "first", 0);
    let db = rusqlite::Connection::open(index_db(&dir)).unwrap();
    // A large unrelated member array must not be deserialized to validate one tiny field.
    let huge=json!({"name":"padding","typeHint":"x".repeat(2*1024*1024),"symbolId":null,"path":"A.java","range":{"startByte":0,"endByte":1,"startLine":1,"endLine":1,"startColumn":1,"endColumn":2}}).to_string();
    db.execute(
        "UPDATE classes SET payload=json_insert(payload,'$.methods[#]',json(?1)) WHERE id=?2",
        rusqlite::params![huge, id(&graph, "A")],
    )
    .unwrap();
    assert!(targets(&call(&app, selector.clone()).await.1, "type").is_empty());
    db.execute(
        "UPDATE class_catalog SET warnings=?1",
        [json!(["x".repeat(2 * 1024 * 1024)]).to_string()],
    )
    .unwrap();
    // Also cap a selected record, returning a limits notice rather than an invalid-selector error.
    let oversized = member(&dir, id(&graph, "A"), "padding", 0);
    let (status, selected_limit) = call(&app, oversized).await;
    assert_eq!(status, 200);
    assert_eq!(selected_limit["truncated"], true);
    assert!(selected_limit["targets"].as_array().unwrap().is_empty());
    db.execute("UPDATE class_relations SET payload=json_set(payload,'$.candidateIds',json(?1)) WHERE owner=?2",rusqlite::params![json!(["x".repeat(100000)]).to_string(),id(&graph,"A")]).unwrap();
    let (_, clipped) = call(&app, selector.clone()).await;
    assert!(clipped["targets"].as_array().unwrap().is_empty());
    db.execute_batch(
        "DELETE FROM class_relations; DELETE FROM classes; DELETE FROM class_catalog;",
    )
    .unwrap();
    let (_, old) = call(&app, source("A.java", 6, &dir)).await;
    assert_eq!(old["requireIndex"], true);
    assert!(
        targets(&old, "declaration")
            .iter()
            .any(|name| name == "run")
    );
    assert!(
        old["warnings"].as_array().unwrap().iter().any(|s| s
            .as_str()
            .unwrap()
            .to_lowercase()
            .contains("index"))
    );
}
#[tokio::test]
async fn source_candidate_count_and_huge_cached_text_have_fixed_output_limits() {
    let mut text = String::new();
    for n in 0..200 {
        text.push_str(&format!("class C{n} {{}} "));
    }
    text.push('\n');
    text.push_str(&" ".repeat(700_000));
    let (dir, _store, _graph, app) = setup(&[("Many.java", &text)]);
    let (_, view) = call(&app, source("Many.java", 1, &dir)).await;
    assert_eq!(view["truncated"], true);
    assert!(view["targets"].as_array().unwrap().len() <= 64);
    assert!(serde_json::to_vec(&view).unwrap().len() <= 512 * 1024);
    assert_eq!(call(&app, source("Many.java", 2, &dir)).await.0, 200);
    assert_eq!(call(&app, source("Many.java", 3, &dir)).await.0, 400);
}

#[tokio::test]
async fn java_field_envelope_is_exact_across_comments_lines_shared_declarations_and_utf8() {
    let (dir, _store, graph, app) = setup(&[(
        "Fields.java",
        "// π 😀 before class bytes\nclass A {\n B first, second; C neighbor;\n B /* shared type */\n multiline;\n java.util.List<B> generic;\n B initialized = make(\"; C trick\");\n}\nclass B {} class C {}\n",
    )]);
    for field in [
        "first",
        "second",
        "multiline",
        "generic",
        "initialized",
        "neighbor",
    ] {
        let (_, view) = call(&app, member(&dir, id(&graph, "A"), field, 0)).await;
        assert!(targets(&view, "type").is_empty(), "{field}: {view}");
    }
    for line in [3, 4, 5] {
        let (_, view) = call(&app, source("Fields.java", line, &dir)).await;
        assert!(targets(&view, "type").is_empty());
    }
}
#[tokio::test]
async fn cached_java_source_budget_and_unproven_member_shape_do_not_guess() {
    let mut text = String::from("class A { B value; } class B {}\n");
    text.push_str(&" ".repeat(300_000));
    let (dir, store, graph, app) = setup(&[("Large.java", &text)]);
    let selector = member(&dir, id(&graph, "A"), "value", 0);
    let (status, within_budget) = call(&app, selector.clone()).await;
    assert_eq!(status, 200, "{within_budget}");
    assert!(targets(&within_budget, "type").is_empty());
    let db = rusqlite::Connection::open(index_db(&dir)).unwrap();
    let original_text = text.clone();
    text.push_str("\nforged()\n");
    text.push_str(&" ".repeat(2 * 1024 * 1024));
    db.execute(
        "UPDATE files SET payload=json_set(payload,'$.text',?1)",
        [text],
    )
    .unwrap();
    // Member navigation must authenticate the selected graph JSON/native pair
    // even though it does not use the source body for type inference.
    let (status, limited) = call(&app, selector.clone()).await;
    assert_eq!(status, 503, "{limited}");
    assert_eq!(limited["error"]["code"], "incompatible_index");
    assert!(limited.get("targets").is_none());
    // The source selector would count forged lines in files.payload.text. Gate
    // that read in the same pinned transaction, before any false line is admitted.
    let (status, forged_line) = call(&app, source("Large.java", 2, &dir)).await;
    assert_eq!(status, 503, "{forged_line}");
    assert_eq!(forged_line["error"]["code"], "incompatible_index");
    assert!(store.source_at("Large.java", None).is_err());
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/source?path=Large.java")
                .header("host", "127.0.0.1:7331")
                .header("authorization", format!("Bearer {TOKEN}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), 503);
    let body: Value =
        serde_json::from_slice(&to_bytes(response.into_body(), 512 * 1024).await.unwrap()).unwrap();
    assert_eq!(body["error"]["code"], "incompatible_index");
    db.execute(
        "UPDATE files SET payload=json_set(payload,'$.text','class A { C other; } class B {}')",
        [],
    )
    .unwrap();
    let (status, unproven) = call(&app, selector.clone()).await;
    assert_eq!(status, 503, "{unproven}");
    assert_eq!(unproven["error"]["code"], "incompatible_index");
    assert!(unproven.get("targets").is_none());
    assert!(store.source_at("Large.java", None).is_err());
    // Restore graph JSON, then tamper the actual selected native BLOB. A pinned
    // typed read must reject it, while unrelated member navigation stays inert.
    db.execute(
        "UPDATE files SET payload=json_set(payload,'$.text',?1)",
        [&original_text],
    )
    .unwrap();
    let mut bytes: Vec<u8> = db
        .query_row(
            "SELECT source_bytes FROM native_documents WHERE path='Large.java'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    bytes[0] ^= 1;
    db.execute(
        "UPDATE native_documents SET source_bytes=?1 WHERE path='Large.java'",
        [bytes],
    )
    .unwrap();
    let native_key = baleyg::native_evidence::DocumentKey {
        source_set_id: format!("source-set:v1:{}", store.root_id()),
        language: "java".into(),
        path: "Large.java".into(),
    };
    assert!(
        store
            .native_source_at(store.status().unwrap().revision, &native_key)
            .is_err()
    );
    assert!(store.source_at("Large.java", None).is_err());
    let (status, corrupted) = call(&app, selector).await;
    assert_eq!(status, 503, "{corrupted}");
    assert_eq!(corrupted["error"]["code"], "incompatible_index");
    assert!(corrupted.get("targets").is_none());
    let response = app
        .oneshot(
            Request::builder()
                .uri("/api/source?path=Large.java")
                .header("host", "127.0.0.1:7331")
                .header("authorization", format!("Bearer {TOKEN}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), 503);
    let body: Value =
        serde_json::from_slice(&to_bytes(response.into_body(), 512 * 1024).await.unwrap()).unwrap();
    assert_eq!(body["error"]["code"], "incompatible_index");
}

#[tokio::test]
async fn unsupported_classes_never_become_class_targets_but_methods_remain_measured() {
    let (dir, _store, _graph, app) =
        setup(&[("app.js", "class Unsupported { run() { return 1; } }\n")]);
    let (_, view) = call(&app, source("app.js", 1, &dir)).await;
    assert!(
        view["targets"]
            .as_array()
            .unwrap()
            .iter()
            .all(|t| t["action"] == "sequence")
    );
    assert_eq!(targets(&view, "declaration"), ["run"]);
    assert_eq!(view["requireIndex"], false);
}

#[tokio::test]
async fn java_same_class_calls_preserve_overloads_measured_nodes_and_cached_revision() {
    let text = "// π 😀 byte offsets\nclass Café {\n void flagArtifactType() {}\n void flagArtifactType(String type) {}\n void run() {\n  flagArtifactType(); // bare\n  this.flagArtifactType(\"π\"); // explicit\n }\n}\nclass Other { void flagArtifactType() {} }\n";
    let (dir, _store, graph, app) = setup(&[("Café.java", text)]);
    let measured = graph
        .nodes
        .iter()
        .find(|n| n.kind == SymbolKind::Class)
        .unwrap();
    let (status, declaration) = call(
        &app,
        source(&measured.path, measured.range.start_line, &dir),
    )
    .await;
    assert_eq!(status, 200);
    assert!(
        declaration["targets"]
            .as_array()
            .unwrap()
            .iter()
            .all(
                |t| matches!(t["reason"].as_str(), Some("declaration" | "enclosing"))
                    && t["matchKind"] == "measured"
            )
    );
    if let Some(callsite) = graph.calls.first() {
        let (status, line) = call(
            &app,
            source(&callsite.path, callsite.range.start_line, &dir),
        )
        .await;
        assert_eq!(status, 200);
        assert!(
            line["targets"]
                .as_array()
                .unwrap()
                .iter()
                .all(|t| t["reason"] != "call" && t["reason"] != "type")
        );
    }
}
#[tokio::test]
async fn java_same_class_calls_do_not_cross_receivers_or_lexical_scopes() {
    let text = r#"import static Utility.imported;
class Outer {
 void hit() {}
 void outerOnly() {}
 int value = hit(); // field initializer
 static { hit(); } // static initializer
 { hit(); } // instance initializer
 void run(Outer obj) {
  obj.hit(); // object
  Outer.hit(); // static receiver
  super.hit(); // super receiver
  (this).hit(); // parenthesized this
  imported(); // imported only
  Runnable ref = this::hit; // reference
  Runnable lambda = () -> { this.hit(); }; // lambda
  class Local {
   void hit() {}
   void run() { hit(); } // local
  }
  Object anon = new Object() {
   void hit() {}
   void run() { this.hit(); } // anonymous
   { hit(); } // anonymous initializer
  };
 }
 class Inner {
  void hit() {}
  void run() {
   hit(); // inner own
   outerOnly(); // no outer fallback
   Outer.this.hit(); // qualified this
   Outer.super.hit(); // qualified super
  }
 }
}
class Utility { static void imported() {} }
enum E {
 ITEM {
  void hit() {}
  void run() { hit(); } // enum anonymous
 };
 void hit() {}
}
"#;
    let (dir, _store, graph, app) = setup(&[("Scopes.java", text)]);
    let measured = graph
        .nodes
        .iter()
        .find(|n| n.kind == SymbolKind::Class)
        .unwrap();
    let (status, declaration) = call(
        &app,
        source(&measured.path, measured.range.start_line, &dir),
    )
    .await;
    assert_eq!(status, 200);
    assert!(
        declaration["targets"]
            .as_array()
            .unwrap()
            .iter()
            .all(
                |t| matches!(t["reason"].as_str(), Some("declaration" | "enclosing"))
                    && t["matchKind"] == "measured"
            )
    );
    if let Some(callsite) = graph.calls.first() {
        let (status, line) = call(
            &app,
            source(&callsite.path, callsite.range.start_line, &dir),
        )
        .await;
        assert_eq!(status, 200);
        assert!(
            line["targets"]
                .as_array()
                .unwrap()
                .iter()
                .all(|t| t["reason"] != "call" && t["reason"] != "type")
        );
    }
}
#[tokio::test]
async fn java_same_class_candidates_exclude_constructors_and_keep_internal_targets() {
    let text = "class A {\n A() {}\n void A() {}\n void A(int n) {}\n void run() {\n  A(); // collision\n  this.A(); // resolved\n }\n}\n";
    let (dir, _store, graph, app) = setup(&[("A.java", text)]);
    let measured = graph
        .nodes
        .iter()
        .find(|n| n.kind == SymbolKind::Class)
        .unwrap();
    let (status, declaration) = call(
        &app,
        source(&measured.path, measured.range.start_line, &dir),
    )
    .await;
    assert_eq!(status, 200);
    assert!(
        declaration["targets"]
            .as_array()
            .unwrap()
            .iter()
            .all(
                |t| matches!(t["reason"].as_str(), Some("declaration" | "enclosing"))
                    && t["matchKind"] == "measured"
            )
    );
    if let Some(callsite) = graph.calls.first() {
        let (status, line) = call(
            &app,
            source(&callsite.path, callsite.range.start_line, &dir),
        )
        .await;
        assert_eq!(status, 200);
        assert!(
            line["targets"]
                .as_array()
                .unwrap()
                .iter()
                .all(|t| t["reason"] != "call" && t["reason"] != "type")
        );
    }
}
#[tokio::test]
async fn java_same_class_proof_requires_exact_calls_callers_owners_and_target_syntax() {
    let text = "class A {\n A hit() { return this; }\n void run() {\n  hit().hit(); // chain\n }\n}\nclass B { void run() {} }\n";
    let (dir, _store, graph, app) = setup(&[("Proof.java", text)]);
    let measured = graph
        .nodes
        .iter()
        .find(|n| n.kind == SymbolKind::Class)
        .unwrap();
    let (status, declaration) = call(
        &app,
        source(&measured.path, measured.range.start_line, &dir),
    )
    .await;
    assert_eq!(status, 200);
    assert!(
        declaration["targets"]
            .as_array()
            .unwrap()
            .iter()
            .all(
                |t| matches!(t["reason"].as_str(), Some("declaration" | "enclosing"))
                    && t["matchKind"] == "measured"
            )
    );
    if let Some(callsite) = graph.calls.first() {
        let (status, line) = call(
            &app,
            source(&callsite.path, callsite.range.start_line, &dir),
        )
        .await;
        assert_eq!(status, 200);
        assert!(
            line["targets"]
                .as_array()
                .unwrap()
                .iter()
                .all(|t| t["reason"] != "call" && t["reason"] != "type")
        );
    }
}
#[tokio::test]
async fn java_same_class_source_and_response_budgets_fail_closed() {
    let mut text = "class A {\n void hit() {}\n void run() {\n  hit(); // call\n }\n}\n".to_owned();
    text.push_str(&" ".repeat(300_000));
    let (dir, _store, graph, app) = setup(&[("Limits.java", &text)]);
    let measured = graph
        .nodes
        .iter()
        .find(|n| n.kind == SymbolKind::Class)
        .unwrap();
    let (status, declaration) = call(
        &app,
        source(&measured.path, measured.range.start_line, &dir),
    )
    .await;
    assert_eq!(status, 200);
    assert!(
        declaration["targets"]
            .as_array()
            .unwrap()
            .iter()
            .all(
                |t| matches!(t["reason"].as_str(), Some("declaration" | "enclosing"))
                    && t["matchKind"] == "measured"
            )
    );
    if let Some(callsite) = graph.calls.first() {
        let (status, line) = call(
            &app,
            source(&callsite.path, callsite.range.start_line, &dir),
        )
        .await;
        assert_eq!(status, 200);
        assert!(
            line["targets"]
                .as_array()
                .unwrap()
                .iter()
                .all(|t| t["reason"] != "call" && t["reason"] != "type")
        );
    }
}
#[tokio::test]
async fn java_same_class_multiline_unicode_and_nested_arguments_remain_line_based() {
    let text = r#"// 😀 π before byte ranges
class Café {
 Object méthode(Object arg) { return arg; }
 void run() {
  this.méthode( // invocation starts
   méthode(null) // nested argument
  ); // invocation ends
  Object anon = new Object(méthode(null)) { // constructor argument
   void run() { méthode(null); } // anonymous no outer fallback
  };
 }
}
"#;
    let (dir, _store, graph, app) = setup(&[("Café.java", text)]);
    let measured = graph
        .nodes
        .iter()
        .find(|n| n.kind == SymbolKind::Class)
        .unwrap();
    let (status, declaration) = call(
        &app,
        source(&measured.path, measured.range.start_line, &dir),
    )
    .await;
    assert_eq!(status, 200);
    assert!(
        declaration["targets"]
            .as_array()
            .unwrap()
            .iter()
            .all(
                |t| matches!(t["reason"].as_str(), Some("declaration" | "enclosing"))
                    && t["matchKind"] == "measured"
            )
    );
    if let Some(callsite) = graph.calls.first() {
        let (status, line) = call(
            &app,
            source(&callsite.path, callsite.range.start_line, &dir),
        )
        .await;
        assert_eq!(status, 200);
        assert!(
            line["targets"]
                .as_array()
                .unwrap()
                .iter()
                .all(|t| t["reason"] != "call" && t["reason"] != "type")
        );
    }
}
#[tokio::test]
async fn java_same_class_row_and_structural_work_limits_are_explicit() {
    let mut text = "class A {\n void hit() {}\n void run() {\n ".to_owned();
    text.push_str(&"hit(); ".repeat(200));
    text.push_str("// many calls\n }\n}\n");
    let (dir, _store, graph, app) = setup(&[("ManyCalls.java", &text)]);
    let measured = graph
        .nodes
        .iter()
        .find(|n| n.kind == SymbolKind::Class)
        .unwrap();
    let (status, declaration) = call(
        &app,
        source(&measured.path, measured.range.start_line, &dir),
    )
    .await;
    assert_eq!(status, 200);
    assert!(
        declaration["targets"]
            .as_array()
            .unwrap()
            .iter()
            .all(
                |t| matches!(t["reason"].as_str(), Some("declaration" | "enclosing"))
                    && t["matchKind"] == "measured"
            )
    );
    if let Some(callsite) = graph.calls.first() {
        let (status, line) = call(
            &app,
            source(&callsite.path, callsite.range.start_line, &dir),
        )
        .await;
        assert_eq!(status, 200);
        assert!(
            line["targets"]
                .as_array()
                .unwrap()
                .iter()
                .all(|t| t["reason"] != "call" && t["reason"] != "type")
        );
    }
}
#[tokio::test]
async fn java_same_class_does_not_infer_inheritance_or_constructor_callers() {
    let text = r#"class Base { void hit() {} }
class Derived extends Base {
 Derived() { hit(); } // constructor caller
 void run() {
  hit(); // inherited only
 }
}
class Own extends Base {
 void hit(int n) {}
 void run() {
  hit(); // own declaration only, no arity selection
 }
}
"#;
    let (dir, _store, graph, app) = setup(&[("Inheritance.java", text)]);
    let measured = graph
        .nodes
        .iter()
        .find(|n| n.kind == SymbolKind::Class)
        .unwrap();
    let (status, declaration) = call(
        &app,
        source(&measured.path, measured.range.start_line, &dir),
    )
    .await;
    assert_eq!(status, 200);
    assert!(
        declaration["targets"]
            .as_array()
            .unwrap()
            .iter()
            .all(
                |t| matches!(t["reason"].as_str(), Some("declaration" | "enclosing"))
                    && t["matchKind"] == "measured"
            )
    );
    if let Some(callsite) = graph.calls.first() {
        let (status, line) = call(
            &app,
            source(&callsite.path, callsite.range.start_line, &dir),
        )
        .await;
        assert_eq!(status, 200);
        assert!(
            line["targets"]
                .as_array()
                .unwrap()
                .iter()
                .all(|t| t["reason"] != "call" && t["reason"] != "type")
        );
    }
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
