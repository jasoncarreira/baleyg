//! Synthetic cached-only navigation. No providers or repository programs are executed.
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
    let store = Store::open(&dir.path().join("state"), &root).unwrap();
    store.publish(&graph, Some(0), &cancel()).unwrap();
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
fn source(path: &str, line: usize) -> Value {
    json!({"expectedRevision":1,"path":path,"line":line})
}
fn id<'a>(g: &'a Graph, name: &str) -> &'a str {
    &g.nodes.iter().find(|s| s.name == name).unwrap().id
}
fn member(dir: &tempfile::TempDir, class: &str, name: &str, ordinal: usize) -> Value {
    let db = rusqlite::Connection::open(dir.path().join("state/cache.db")).unwrap();
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
    json!({"expectedRevision":1,"classId":class,"memberName":name,"startByte":m["range"]["startByte"],"endByte":m["range"]["endByte"]})
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
        assert_eq!(targets(&view, "type"), ["B"], "{view}");
        assert_eq!(view["targets"][0]["symbol"]["path"], "B.java");
        assert_eq!(view["targets"][0]["matchKind"], "syntaxCandidate");
        assert_eq!(view["targets"][0]["action"], "class");
    }
    for (ordinal, expected) in ["B", "C"].into_iter().enumerate() {
        let selector = member(&dir, id(&graph, "A"), "run", ordinal);
        let (_, view) = call(&app, selector.clone()).await;
        assert_eq!(targets(&view, "type"), [expected]);
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
        assert!(
            view["warnings"]
                .as_array()
                .unwrap()
                .iter()
                .any(|w| w.as_str().unwrap().contains("No indexed target"))
        );
    }
}
#[tokio::test]
async fn cached_only_source_lines_utf8_and_measured_ranges() {
    let (dir, store, graph, app) = setup(&[(
        "unicode.py",
        "class Café:\n    def méthode(self):\n        # π 😀\n        return 1\n\n",
    )]);
    let (_, view) = call(&app, source("unicode.py", 3)).await;
    assert_eq!(targets(&view, "enclosing"), ["Café", "méthode"]);
    for target in view["targets"].as_array().unwrap() {
        let measured = graph
            .nodes
            .iter()
            .find(|s| s.id == target["symbol"]["id"])
            .unwrap();
        assert_eq!(target["symbol"], serde_json::to_value(measured).unwrap());
    }
    let (_, method) = call(&app, source("unicode.py", 2)).await;
    assert_eq!(targets(&method, "declaration"), ["méthode"]);
    std::fs::remove_dir_all(dir.path().join("workspace")).unwrap();
    assert_eq!(call(&app, source("unicode.py", 3)).await.1, view);
    assert_eq!(store.graph().unwrap(), graph);
    assert_eq!(call(&app, source("unicode.py", 6)).await.0, 200); // trailing empty cached line
    assert_eq!(call(&app, source("unicode.py", 7)).await.0, 400);
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
        source("../A.java", 1),
        source("/A.java", 1),
        source("A.java", 0),
        source("missing", 1),
        source("A.java", 999),
        wrong_range,
        wrong_name,
        wrong_owner,
    ] {
        let (status, value) = call(&app, body.clone()).await;
        assert_eq!(status, 400, "{body}: {value}");
    }
    store.publish(&graph, Some(1), &cancel()).unwrap();
    assert_eq!(call(&app, good).await.0, 409);
    assert_eq!(call(&app, source("A.java", 3)).await.0, 409);
}
#[tokio::test]
async fn auth_host_origin_are_enforced() {
    let (_dir, _store, _graph, app) = fixture();
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
                req.body(Body::from(source("A.java", 3).to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), expected);
    }
}
#[tokio::test]
async fn ambiguity_preserves_all_cached_candidates_and_unresolved_calls_are_not_guessed() {
    let (dir, store, mut graph, app) = setup(&[
        (
            "a.py",
            "class A:\n    value: B\nclass B: pass\nclass B: pass\n",
        ),
        (
            "calls.py",
            "def target():\n    pass\ndef caller(obj):\n    target()\n    obj.target()\n",
        ),
    ]);
    let (_, view) = call(&app, member(&dir, id(&graph, "A"), "value", 0)).await;
    assert_eq!(targets(&view, "type"), ["B", "B"]);
    assert!(
        view["targets"]
            .as_array()
            .unwrap()
            .iter()
            .all(|t| t["matchKind"] == "ambiguous")
    );
    // Synthetic measured resolution; the syntax-only Python index intentionally does not resolve calls.
    let target = id(&graph, "target").to_owned();
    graph.calls[0].target = Some(target);
    graph.calls[0].resolution = Resolution::Internal;
    store.publish(&graph, Some(1), &cancel()).unwrap();
    let mut resolved_selector = source("calls.py", 4);
    resolved_selector["expectedRevision"] = json!(2);
    let (_, resolved) = call(&app, resolved_selector).await;
    assert_eq!(targets(&resolved, "call"), ["target"]);
    let mut unresolved_selector = source("calls.py", 5);
    unresolved_selector["expectedRevision"] = json!(2);
    let (_, unresolved) = call(&app, unresolved_selector).await;
    assert!(targets(&unresolved, "call").is_empty());
}
#[tokio::test]
async fn old_projection_guidance_and_large_payloads_are_bounded() {
    let (dir, _store, graph, app) = fixture();
    let selector = member(&dir, id(&graph, "A"), "first", 0);
    let db = rusqlite::Connection::open(dir.path().join("state/cache.db")).unwrap();
    // A large unrelated member array must not be deserialized to validate one tiny field.
    let huge=json!({"name":"padding","typeHint":"x".repeat(2*1024*1024),"symbolId":null,"path":"A.java","range":{"startByte":0,"endByte":1,"startLine":1,"endLine":1,"startColumn":1,"endColumn":2}}).to_string();
    db.execute(
        "UPDATE classes SET payload=json_insert(payload,'$.methods[#]',json(?1)) WHERE id=?2",
        rusqlite::params![huge, id(&graph, "A")],
    )
    .unwrap();
    assert_eq!(
        targets(&call(&app, selector.clone()).await.1, "type"),
        ["B"]
    );
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
    assert_eq!(clipped["truncated"], true);
    assert!(clipped["targets"].as_array().unwrap().is_empty());
    db.execute_batch(
        "DELETE FROM class_relations; DELETE FROM classes; DELETE FROM class_catalog;",
    )
    .unwrap();
    let (_, old) = call(&app, source("A.java", 6)).await;
    assert_eq!(old["requireIndex"], true);
    assert_eq!(targets(&old, "declaration"), ["run"]);
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
    let (_dir, _store, _graph, app) = setup(&[("Many.java", &text)]);
    let (_, view) = call(&app, source("Many.java", 1)).await;
    assert_eq!(view["truncated"], true);
    assert!(view["targets"].as_array().unwrap().len() <= 64);
    assert!(serde_json::to_vec(&view).unwrap().len() <= 512 * 1024);
    assert_eq!(call(&app, source("Many.java", 2)).await.0, 200);
    assert_eq!(call(&app, source("Many.java", 3)).await.0, 400);
}

#[tokio::test]
async fn java_field_envelope_is_exact_across_comments_lines_shared_declarations_and_utf8() {
    let (dir, _store, graph, app) = setup(&[(
        "Fields.java",
        "// π 😀 before class bytes\nclass A {\n B first, second; C neighbor;\n B /* shared type */\n multiline;\n java.util.List<B> generic;\n B initialized = make(\"; C trick\");\n}\nclass B {} class C {}\n",
    )]);
    for field in ["first", "second", "multiline", "generic", "initialized"] {
        let (_, view) = call(&app, member(&dir, id(&graph, "A"), field, 0)).await;
        assert_eq!(targets(&view, "type"), ["B"], "{field}: {view}");
    }
    let (_, neighbor) = call(&app, member(&dir, id(&graph, "A"), "neighbor", 0)).await;
    assert_eq!(targets(&neighbor, "type"), ["C"]);
    let (_, shared_line) = call(&app, source("Fields.java", 3)).await;
    let mut names = targets(&shared_line, "type");
    names.sort();
    assert_eq!(names, ["B", "C"]);
    let (_, type_line) = call(&app, source("Fields.java", 4)).await;
    assert_eq!(targets(&type_line, "type"), ["B"]);
    let (_, declarator_line) = call(&app, source("Fields.java", 5)).await;
    assert!(
        targets(&declarator_line, "type").is_empty(),
        "line-based evidence is not arbitrary word guessing"
    );
}
#[tokio::test]
async fn cached_java_source_budget_and_unproven_member_shape_do_not_guess() {
    let mut text = String::from("class A { B value; } class B {}\n");
    text.push_str(&" ".repeat(300_000));
    let (dir, _store, graph, app) = setup(&[("Large.java", &text)]);
    let selector = member(&dir, id(&graph, "A"), "value", 0);
    let (_, within_budget) = call(&app, selector.clone()).await;
    assert_eq!(
        targets(&within_budget, "type"),
        ["B"],
        "files larger than 256KiB still navigate"
    );
    let db = rusqlite::Connection::open(dir.path().join("state/cache.db")).unwrap();
    text.push_str(&" ".repeat(2 * 1024 * 1024));
    db.execute(
        "UPDATE files SET payload=json_set(payload,'$.text',?1)",
        [text],
    )
    .unwrap();
    let (_, limited) = call(&app, selector.clone()).await;
    assert_eq!(limited["truncated"], true);
    assert!(targets(&limited, "type").is_empty());
    assert!(
        limited["warnings"]
            .as_array()
            .unwrap()
            .iter()
            .any(|w| w.as_str().unwrap().contains("text budget"))
    );
    // Even a recorded selector cannot bind to a different AST declarator in cached text.
    let db = rusqlite::Connection::open(dir.path().join("state/cache.db")).unwrap();
    db.execute(
        "UPDATE files SET payload=json_set(payload,'$.text','class A { C other; } class B {}')",
        [],
    )
    .unwrap();
    let (_, unproven) = call(&app, selector).await;
    assert!(targets(&unproven, "type").is_empty());
    assert!(
        unproven["warnings"]
            .as_array()
            .unwrap()
            .iter()
            .any(|w| w.as_str().unwrap().contains("could not be verified"))
    );
}

#[tokio::test]
async fn unsupported_classes_never_become_class_targets_but_methods_remain_measured() {
    let (_dir, _store, _graph, app) =
        setup(&[("app.js", "class Unsupported { run() { return 1; } }\n")]);
    let (_, view) = call(&app, source("app.js", 1)).await;
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

fn line_of(text: &str, marker: &str) -> usize {
    text.lines().position(|line| line.contains(marker)).unwrap() + 1
}
fn same_class_targets(view: &Value) -> Vec<&Value> {
    view["targets"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|t| t["reason"] == "call" && t["matchKind"] == "sameClassCandidate")
        .collect()
}
#[tokio::test]
async fn java_same_class_calls_preserve_overloads_measured_nodes_and_cached_revision() {
    let text = "// π 😀 byte offsets\nclass Café {\n void flagArtifactType() {}\n void flagArtifactType(String type) {}\n void run() {\n  flagArtifactType(); // bare\n  this.flagArtifactType(\"π\"); // explicit\n }\n}\nclass Other { void flagArtifactType() {} }\n";
    let (dir, store, graph, app) = setup(&[("Café.java", text)]);
    let owner = id(&graph, "Café");
    for marker in ["// bare", "// explicit"] {
        let (_, view) = call(&app, source("Café.java", line_of(text, marker))).await;
        let candidates = same_class_targets(&view);
        assert_eq!(candidates.len(), 2, "{view}");
        assert!(
            view["warnings"]
                .as_array()
                .unwrap()
                .iter()
                .any(|w| w.as_str().unwrap().contains("not compiler resolution"))
        );
        for target in candidates {
            assert_eq!(target["action"], "sequence");
            assert_eq!(target["symbol"]["parent"], owner);
            let measured = graph
                .nodes
                .iter()
                .find(|n| n.id == target["symbol"]["id"])
                .unwrap();
            assert_eq!(target["symbol"], serde_json::to_value(measured).unwrap());
        }
    }
    let selector = source("Café.java", line_of(text, "// bare"));
    let before = call(&app, selector.clone()).await.1;
    std::fs::remove_dir_all(dir.path().join("workspace")).unwrap();
    assert_eq!(call(&app, selector.clone()).await.1, before);
    assert_eq!(
        store.graph().unwrap(),
        graph,
        "navigation must not resolve or write graph calls"
    );
    // Missing capped projection entries must not suppress measured method nodes.
    let db = rusqlite::Connection::open(dir.path().join("state/cache.db")).unwrap();
    db.execute(
        "UPDATE classes SET payload=json_set(payload,'$.methods',json('[]')) WHERE id=?1",
        [owner],
    )
    .unwrap();
    db.execute("UPDATE class_catalog SET truncated=1", [])
        .unwrap();
    let (_, truncated) = call(&app, selector.clone()).await;
    assert_eq!(truncated["truncated"], true);
    assert_eq!(same_class_targets(&truncated).len(), 2);
    store.publish(&graph, Some(1), &cancel()).unwrap();
    assert_eq!(call(&app, selector).await.0, 409);
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
    let (_dir, _store, graph, app) = setup(&[("Scopes.java", text)]);
    assert_eq!(graph.stats.parse_error_files, 0);
    for marker in [
        "// field initializer",
        "// static initializer",
        "// instance initializer",
        "// object",
        "// static receiver",
        "// super receiver",
        "// parenthesized this",
        "// imported only",
        "// reference",
        "// lambda",
        "// local",
        "// anonymous",
        "// anonymous initializer",
        "// no outer fallback",
        "// qualified this",
        "// qualified super",
        "// enum anonymous",
    ] {
        let (_, view) = call(&app, source("Scopes.java", line_of(text, marker))).await;
        assert!(targets(&view, "call").is_empty(), "{marker}: {view}");
    }
    let (_, view) = call(&app, source("Scopes.java", line_of(text, "// inner own"))).await;
    let candidates = same_class_targets(&view);
    assert_eq!(candidates.len(), 1, "{view}");
    assert_eq!(candidates[0]["symbol"]["parent"], id(&graph, "Inner"));
}

#[tokio::test]
async fn java_same_class_candidates_exclude_constructors_and_keep_internal_targets() {
    let text = "class A {\n A() {}\n void A() {}\n void A(int n) {}\n void run() {\n  A(); // collision\n  this.A(); // resolved\n }\n}\n";
    let (dir, store, mut graph, app) = setup(&[("A.java", text)]);
    let (_, view) = call(&app, source("A.java", line_of(text, "// collision"))).await;
    let candidates = same_class_targets(&view);
    assert_eq!(candidates.len(), 2, "{view}");
    assert!(
        candidates
            .iter()
            .all(|t| t["symbol"]["range"]["startLine"] != 2)
    );
    let method = graph
        .nodes
        .iter()
        .find(|n| n.name == "A" && n.range.start_line == 3)
        .unwrap()
        .id
        .clone();
    let resolved = graph
        .calls
        .iter_mut()
        .find(|c| c.callee_text == "this.A")
        .unwrap();
    resolved.target = Some(method.clone());
    resolved.resolution = Resolution::Internal;
    store.publish(&graph, Some(1), &cancel()).unwrap();
    let published = store.graph().unwrap(); // publishing refreshes summary counts
    let mut selector = source("A.java", line_of(text, "// resolved"));
    selector["expectedRevision"] = json!(2);
    let (_, view) = call(&app, selector).await;
    assert_eq!(targets(&view, "call"), ["A"]);
    assert!(same_class_targets(&view).is_empty());
    let target = view["targets"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["reason"] == "call")
        .unwrap();
    assert_eq!(target["symbol"]["id"], method);
    assert_eq!(target["matchKind"], "measured");
    assert_eq!(store.graph().unwrap(), published);
    drop(dir);
}

#[tokio::test]
async fn java_same_class_proof_requires_exact_calls_callers_owners_and_target_syntax() {
    let text = "class A {\n A hit() { return this; }\n void run() {\n  hit().hit(); // chain\n }\n}\nclass B { void run() {} }\n";
    let (dir, _store, graph, app) = setup(&[("Proof.java", text)]);
    let selector = source("Proof.java", line_of(text, "// chain"));
    assert_eq!(
        same_class_targets(&call(&app, selector.clone()).await.1).len(),
        1
    );
    let db = rusqlite::Connection::open(dir.path().join("state/cache.db")).unwrap();
    let inner = graph.calls.iter().find(|c| c.callee_text == "hit").unwrap();
    let outer = graph
        .calls
        .iter()
        .find(|c| c.callee_text == "hit().hit")
        .unwrap();
    assert_eq!(inner.range.start_byte, outer.range.start_byte);
    // Exact range matters even when a chained invocation shares startByte and callee names.
    let original: String = db
        .query_row("SELECT payload FROM calls WHERE id=?1", [&inner.id], |r| {
            r.get(0)
        })
        .unwrap();
    let caller = graph.nodes.iter().find(|n| n.id == inner.caller).unwrap();
    let owner = graph
        .nodes
        .iter()
        .find(|n| n.id == caller.parent.as_ref().unwrap().as_str())
        .unwrap();
    let target = graph.nodes.iter().find(|n| n.name == "hit").unwrap();
    for (table, node_id, field, value) in [
        (
            "calls",
            inner.id.as_str(),
            "$.range.endByte",
            json!(outer.range.end_byte),
        ),
        ("calls", inner.id.as_str(), "$.range.startColumn", json!(99)),
        (
            "calls",
            inner.id.as_str(),
            "$.calleeText",
            json!("this.hit"),
        ),
        ("calls", inner.id.as_str(), "$.caller", json!(target.id)),
        (
            "nodes",
            caller.id.as_str(),
            "$.range.endByte",
            json!(owner.range.end_byte),
        ),
        ("nodes", caller.id.as_str(), "$.name", json!("notRun")),
        ("nodes", owner.id.as_str(), "$.name", json!("NotA")),
        ("nodes", owner.id.as_str(), "$.range.startByte", json!(1)),
        (
            "nodes",
            target.id.as_str(),
            "$.range.endByte",
            json!(caller.range.end_byte),
        ),
    ] {
        let saved: String = db
            .query_row(
                &format!("SELECT payload FROM {table} WHERE id=?1"),
                [node_id],
                |r| r.get(0),
            )
            .unwrap();
        db.execute(
            &format!("UPDATE {table} SET payload=json_set(payload,?2,json(?3)) WHERE id=?1"),
            rusqlite::params![node_id, field, value.to_string()],
        )
        .unwrap();
        let (_, view) = call(&app, selector.clone()).await;
        assert!(
            same_class_targets(&view).is_empty(),
            "{table} {field}: {view}"
        );
        db.execute(
            &format!("UPDATE {table} SET payload=?2 WHERE id=?1"),
            rusqlite::params![node_id, saved],
        )
        .unwrap();
    }
    assert_eq!(
        db.query_row("SELECT payload FROM calls WHERE id=?1", [&inner.id], |r| {
            r.get::<_, String>(0)
        })
        .unwrap(),
        original
    );
    // Cached source, not live text, must prove the recorded name and range.
    db.execute(
        "UPDATE files SET payload=json_set(payload,'$.text',?1)",
        [text.replace("hit().hit()", "new A().hit()")],
    )
    .unwrap();
    assert!(same_class_targets(&call(&app, selector).await.1).is_empty());
}

#[tokio::test]
async fn java_same_class_source_and_response_budgets_fail_closed() {
    let mut text = "class A {\n void hit() {}\n void run() {\n  hit(); // call\n }\n}\n".to_owned();
    text.push_str(&" ".repeat(300_000));
    let (dir, _store, graph, app) = setup(&[("Limits.java", &text)]);
    let selector = source("Limits.java", line_of(&text, "// call"));
    assert_eq!(
        same_class_targets(&call(&app, selector.clone()).await.1).len(),
        1
    );
    let db = rusqlite::Connection::open(dir.path().join("state/cache.db")).unwrap();
    db.execute(
        "UPDATE files SET payload=json_set(payload,'$.text',?1)",
        [format!("{text}{}", " ".repeat(2 * 1024 * 1024))],
    )
    .unwrap();
    let (_, view) = call(&app, selector.clone()).await;
    assert_eq!(view["truncated"], true);
    assert!(same_class_targets(&view).is_empty());
    assert!(
        view["warnings"]
            .as_array()
            .unwrap()
            .iter()
            .any(|w| w.as_str().unwrap().contains("text budget"))
    );
    db.execute(
        "UPDATE files SET payload=json_set(payload,'$.text',?1)",
        [format!("{text}class Broken {{")],
    )
    .unwrap();
    let (_, malformed) = call(&app, selector.clone()).await;
    assert!(same_class_targets(&malformed).is_empty());
    assert!(
        malformed["warnings"]
            .as_array()
            .unwrap()
            .iter()
            .any(|w| w.as_str().unwrap().contains("syntax errors"))
    );
    db.execute(
        "UPDATE files SET payload=json_set(payload,'$.text',?1)",
        [&text],
    )
    .unwrap();
    let target = graph.nodes.iter().find(|n| n.name == "hit").unwrap();
    db.execute(
        "UPDATE nodes SET payload=json_set(payload,'$.provenance.source',?1) WHERE id=?2",
        rusqlite::params!["x".repeat(40_000), target.id],
    )
    .unwrap();
    let (_, oversized) = call(&app, selector).await;
    assert_eq!(oversized["truncated"], true);
    assert!(same_class_targets(&oversized).is_empty());

    let mut many = "class Many {\n".to_owned();
    for n in 0..150 {
        many.push_str(&format!(" void hit(T{n} arg) {{}}\n"));
    }
    many.push_str(" void run() {\n  hit();\n }\n}\n");
    let (_dir, _store, _graph, app) = setup(&[("Many.java", &many)]);
    let (_, limited) = call(&app, source("Many.java", line_of(&many, "  hit();"))).await;
    assert_eq!(limited["truncated"], true);
    assert!(!same_class_targets(&limited).is_empty());
    assert!(limited["targets"].as_array().unwrap().len() <= 64);
    assert!(serde_json::to_vec(&limited).unwrap().len() <= 512 * 1024);
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
    let (_dir, _store, graph, app) = setup(&[("Café.java", text)]);
    assert_eq!(graph.stats.parse_error_files, 0);
    for marker in [
        "// invocation starts",
        "// nested argument",
        "// invocation ends",
        "// constructor argument",
    ] {
        let (_, view) = call(&app, source("Café.java", line_of(text, marker))).await;
        assert_eq!(targets(&view, "call"), ["méthode"], "{marker}: {view}");
        assert_eq!(same_class_targets(&view).len(), 1);
    }
    let (_, view) = call(
        &app,
        source("Café.java", line_of(text, "// anonymous no outer fallback")),
    )
    .await;
    assert!(targets(&view, "call").is_empty());
}

#[tokio::test]
async fn java_same_class_row_and_structural_work_limits_are_explicit() {
    let mut text = "class A {\n void hit() {}\n void run() {\n ".to_owned();
    text.push_str(&"hit(); ".repeat(200));
    text.push_str("// many calls\n }\n}\n");
    let (_dir, _store, _graph, app) = setup(&[("ManyCalls.java", &text)]);
    let (_, view) = call(
        &app,
        source("ManyCalls.java", line_of(&text, "// many calls")),
    )
    .await;
    assert_eq!(view["truncated"], true);
    assert_eq!(same_class_targets(&view).len(), 1);
    assert!(view["targets"].as_array().unwrap().len() <= 64);
    // A valid, deeply nested call is not re-associated by climbing an unbounded AST.
    let deep = format!(
        "class A {{\n Object hit() {{ return null; }}\n void run() {{\n Object x = {}hit(){};\n }}\n}}\n",
        "(".repeat(70),
        ")".repeat(70)
    );
    let (_dir, _store, graph, app) = setup(&[("Deep.java", &deep)]);
    assert_eq!(graph.stats.parse_error_files, 0);
    let (_, view) = call(&app, source("Deep.java", 4)).await;
    assert_eq!(view["truncated"], true);
    assert!(same_class_targets(&view).is_empty());
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
    let (_dir, _store, graph, app) = setup(&[("Inheritance.java", text)]);
    for marker in ["// constructor caller", "// inherited only"] {
        let (_, view) = call(&app, source("Inheritance.java", line_of(text, marker))).await;
        assert!(targets(&view, "call").is_empty(), "{marker}: {view}");
    }
    let (_, view) = call(
        &app,
        source("Inheritance.java", line_of(text, "// own declaration only")),
    )
    .await;
    let candidates = same_class_targets(&view);
    assert_eq!(candidates.len(), 1);
    assert_eq!(candidates[0]["symbol"]["parent"], id(&graph, "Own"));
}
