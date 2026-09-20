//! Synthetic Java/Python file -> method -> static sequence integration.
//! Workspace code, build scripts, and Python imports must never execute.
use axum::{
    Router,
    body::{Body, to_bytes},
    http::Request,
};
use baleyg::{
    behavior::{SequenceStep, SequenceView},
    http,
    indexer::{IndexOptions, index_workspace},
    model::{Graph, SymbolKind},
    store::Store,
};
use serde_json::{Value, json};
use std::{
    path::PathBuf,
    sync::{Arc, atomic::AtomicBool},
};
use tower::ServiceExt;

const TOKEN: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
const JAVA: &str = r#"interface Port { void signatureOnly(); }
abstract class Worker {
    abstract void abstractOnly();
    native void nativeOnly();
    Worker() { setup(); }
    int run(int value) {
        if (value > 0) { save(value); }
        return finish(value);
    }
}
"#;
const PYTHON: &str = r#"class Worker:
    def run(self, value):
        if value > 0:
            save(value)
        return finish(value)

    def empty(self):
        pass
"#;

struct Fixture {
    temp: tempfile::TempDir,
    workspace: PathBuf,
    sentinel: PathBuf,
    store: Store,
    graph: Graph,
    app: Router,
}
fn setup() -> Fixture {
    let temp = tempfile::tempdir().unwrap();
    let workspace = temp.path().join("workspace");
    std::fs::create_dir(&workspace).unwrap();
    let sentinel = temp.path().join("MUST_NOT_EXECUTE");
    // A Gradle configuration and executable wrapper tripwire, not a real build.
    std::fs::write(
        workspace.join("build.gradle"),
        format!(
            "new File({:?}).text = 'executed'\n",
            sentinel.to_str().unwrap()
        ),
    )
    .unwrap();
    let wrapper = workspace.join("gradlew");
    std::fs::write(
        &wrapper,
        format!(
            "#!/bin/sh\nprintf executed > '{}'\nexit 99\n",
            sentinel.display()
        ),
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(wrapper, std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    // Importing this source would create the sentinel. Parsing it must not.
    std::fs::write(
        workspace.join("worker.py"),
        format!(
            "from pathlib import Path\nPath({:?}).write_text('executed')\n{}",
            sentinel.to_str().unwrap(),
            PYTHON
        ),
    )
    .unwrap();
    for (path, text) in [
        ("Worker.java", JAVA),
        ("helper.js", "function helper() { notify(); }\n"),
        ("helper.rs", "fn helper() { notify(); }\n"),
        ("unsupported.ts", "function unsupported(): void {}\n"),
        ("README.md", "Synthetic source-only integration fixture\n"),
    ] {
        std::fs::write(workspace.join(path), text).unwrap();
    }
    let options = IndexOptions::new(workspace.clone());
    let cancel = Arc::new(AtomicBool::new(false));
    let graph = index_workspace(&options, &cancel, |_| {}).unwrap();
    assert!(!sentinel.exists(), "indexing executed workspace code");
    let store = Store::open(&temp.path().join("state"), &workspace).unwrap();
    assert_eq!(store.publish(&graph, Some(0), &cancel).unwrap(), 1);
    let app = http::router(
        http::new(
            store.clone(),
            options,
            TOKEN.into(),
            "127.0.0.1:7331".parse().unwrap(),
        )
        .unwrap(),
    );
    Fixture {
        temp,
        workspace,
        sentinel,
        store,
        graph,
        app,
    }
}
async fn call(app: &Router, method: &str, path: &str, body: Value) -> (u16, Value) {
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
    let status = response.status().as_u16();
    let bytes = to_bytes(response.into_body(), 4 * 1024 * 1024)
        .await
        .unwrap();
    (status, serde_json::from_slice(&bytes).unwrap())
}
fn flatten<'a>(steps: &'a [SequenceStep], out: &mut Vec<&'a SequenceStep>) {
    for step in steps {
        out.push(step);
        flatten(&step.children, out);
        flatten(&step.alternate, out);
    }
}

#[tokio::test]
async fn mixed_catalog_tree_and_lexical_parents_remain_language_neutral() {
    let f = setup();
    let (code, files) = call(&f.app, "GET", "/api/files?revision=1", Value::Null).await;
    assert_eq!(code, 200);
    let (code, tree) = call(&f.app, "GET", "/api/tree", Value::Null).await;
    assert_eq!(code, 200);
    for (path, language) in [
        ("Worker.java", "java"),
        ("worker.py", "python"),
        ("helper.js", "javascript"),
        ("helper.rs", "rust"),
    ] {
        let count = f
            .graph
            .nodes
            .iter()
            .filter(|n| {
                n.path == path && matches!(n.kind, SymbolKind::Function | SymbolKind::Method)
            })
            .count();
        assert!(count > 0, "missing methods for {path}");
        let file = files["items"]
            .as_array()
            .unwrap()
            .iter()
            .find(|i| i["path"] == path)
            .unwrap();
        assert_eq!(file["language"], language);
        assert_eq!(file["methodCount"], count);
        let entry = tree["items"]
            .as_array()
            .unwrap()
            .iter()
            .find(|i| i["path"] == path)
            .unwrap();
        assert_eq!(entry["indexedPath"], path);
        assert_eq!(entry["methodCount"], count);
        assert!(entry.get("unindexedReason").is_none());
        let (code, methods) = call(
            &f.app,
            "GET",
            &format!("/api/methods?path={path}&revision=1"),
            Value::Null,
        )
        .await;
        assert_eq!(code, 200);
        assert_eq!(methods["items"].as_array().unwrap().len(), count);
        for item in methods["items"].as_array().unwrap() {
            let symbol = &item["symbol"];
            let indexed = f.graph.nodes.iter().find(|n| n.id == symbol["id"]).unwrap();
            assert_eq!(symbol, &serde_json::to_value(indexed).unwrap());
            if symbol["name"] == "run" {
                let parent = f
                    .graph
                    .nodes
                    .iter()
                    .find(|n| Some(&n.id) == indexed.parent.as_ref())
                    .unwrap();
                assert_eq!(parent.kind, SymbolKind::Class);
                assert_eq!(parent.name, "Worker");
                assert_eq!(parent.path, path);
            }
        }
    }
    assert_eq!(files["items"].as_array().unwrap().len(), 4);
    // Supported but not in the published snapshot is not an unsupported language.
    std::fs::write(f.workspace.join("Pending.java"), "class Pending {}\n").unwrap();
    std::fs::write(f.workspace.join("pending.py"), "pass\n").unwrap();
    let (_, tree) = call(&f.app, "GET", "/api/tree", Value::Null).await;
    for (path, reason) in [
        (
            "Pending.java",
            "Not indexed yet (may be excluded or size-limited)",
        ),
        (
            "pending.py",
            "Not indexed yet (may be excluded or size-limited)",
        ),
        ("unsupported.ts", "TypeScript indexing not supported yet"),
        ("README.md", "Unsupported source type"),
    ] {
        let entry = tree["items"]
            .as_array()
            .unwrap()
            .iter()
            .find(|i| i["path"] == path)
            .unwrap();
        assert!(entry["indexedPath"].is_null());
        assert_eq!(entry["unindexedReason"], reason);
    }
    assert!(!f.sentinel.exists());
}

#[tokio::test]
async fn cached_java_python_sequences_keep_measured_calls_after_source_deletion() {
    let f = setup();
    for path in ["Worker.java", "worker.py"] {
        let seed = f
            .graph
            .nodes
            .iter()
            .find(|n| n.path == path && n.name == "run")
            .unwrap();
        let request = json!({"seed":seed.id,"expectedRevision":1});
        let before = call(&f.app, "POST", "/api/sequence", request.clone()).await;
        assert_eq!(before.0, 200, "{}", before.1);
        let sequence: SequenceView = serde_json::from_value(before.1.clone()).unwrap();
        assert_eq!(sequence.seed, *seed);
        let mut steps = Vec::new();
        flatten(&sequence.steps, &mut steps);
        assert!(steps.iter().filter_map(|s| s.call_id.as_ref()).count() >= 2);
        for step in steps.iter().filter(|s| s.call_id.is_some()) {
            let measured = f
                .graph
                .calls
                .iter()
                .find(|c| Some(&c.id) == step.call_id.as_ref())
                .unwrap();
            assert_eq!(measured.caller, seed.id, "no foreign method body calls");
            assert_eq!(step.range, measured.range);
            assert_eq!(step.path, measured.path);
            assert_eq!(step.resolution, Some(measured.resolution));
            assert!(measured.target.is_none(), "no semantic target was proven");
            assert_eq!(measured.resolution, baleyg::model::Resolution::Unresolved);
            if let Some(target) = &step.target {
                let participant = sequence
                    .participants
                    .iter()
                    .find(|p| &p.id == target)
                    .unwrap();
                assert!(matches!(
                    participant.kind.as_str(),
                    "unresolvedReceiver" | "unresolvedCallee" | "boundary"
                ));
            }
            assert!(
                step.children.is_empty() && step.alternate.is_empty(),
                "unresolved invocation must stay terminal"
            );
        }
        std::fs::remove_file(f.workspace.join(path)).unwrap();
        assert_eq!(call(&f.app, "POST", "/api/sequence", request).await, before);
        let (code, source) = call(
            &f.app,
            "GET",
            &format!("/api/source?path={path}&revision=1"),
            Value::Null,
        )
        .await;
        assert_eq!(code, 200);
        assert_eq!(
            source["file"]["text"],
            f.graph
                .files
                .iter()
                .find(|file| file.path == path)
                .unwrap()
                .text
        );
        assert_eq!(
            call(
                &f.app,
                "GET",
                &format!("/api/methods?path={path}&revision=1"),
                Value::Null
            )
            .await
            .0,
            200
        );
        let (code, all) = call(
            &f.app,
            "POST",
            "/api/sequence",
            json!({"seed":seed.id,"expectedRevision":1,"showAll":true}),
        )
        .await;
        assert_eq!(code, 200);
        let all: SequenceView = serde_json::from_value(all).unwrap();
        let mut all_steps = Vec::new();
        flatten(&all.steps, &mut all_steps);
        for step in all_steps.iter().filter(|s| s.call_id.is_some()) {
            assert_eq!(step.resolution, Some(baleyg::model::Resolution::Unresolved));
            assert!(step.children.is_empty() && step.alternate.is_empty());
            if let Some(target) = &step.target {
                let participant = all.participants.iter().find(|p| &p.id == target).unwrap();
                assert!(matches!(
                    participant.kind.as_str(),
                    "unresolvedReceiver" | "unresolvedCallee" | "boundary"
                ));
            }
        }
    }
    assert!(!f.sentinel.exists());
    assert!(f.temp.path().join("state").is_dir());
}

#[tokio::test]
async fn declarations_without_bodies_are_navigable_without_fabricated_calls() {
    let f = setup();
    let (code, methods) = call(
        &f.app,
        "GET",
        "/api/methods?path=Worker.java&revision=1",
        Value::Null,
    )
    .await;
    assert_eq!(code, 200);
    for name in ["signatureOnly", "abstractOnly", "nativeOnly"] {
        let method = methods["items"]
            .as_array()
            .unwrap()
            .iter()
            .find(|i| i["symbol"]["name"] == name)
            .unwrap();
        let seed = method["symbol"]["id"].as_str().unwrap();
        let (code, value) = call(
            &f.app,
            "POST",
            "/api/sequence",
            json!({"seed":seed,"expectedRevision":1}),
        )
        .await;
        assert_eq!(code, 200, "{name}: {value}");
        let sequence: SequenceView = serde_json::from_value(value).unwrap();
        let mut steps = Vec::new();
        flatten(&sequence.steps, &mut steps);
        assert!(steps.iter().all(|s| s.call_id.is_none()));
        assert!(!f.graph.calls.iter().any(|c| c.caller == seed));
        let explanation = format!("{:?} {:?}", sequence.warnings, sequence.steps).to_lowercase();
        assert!(
            explanation.contains("body"),
            "bodyless declaration must be explicit: {explanation}"
        );
    }
    let empty = f
        .graph
        .nodes
        .iter()
        .find(|n| n.path == "worker.py" && n.name == "empty")
        .unwrap();
    let sequence = f.store.sequence_at(&empty.id, 1, false).unwrap().unwrap();
    let mut steps = Vec::new();
    flatten(&sequence.steps, &mut steps);
    assert!(steps.iter().all(|s| s.call_id.is_none()));
    assert!(!f.sentinel.exists());
}

#[tokio::test]
async fn java_python_cached_endpoints_enforce_revision_and_auth() {
    let f = setup();
    let cancel = Arc::new(AtomicBool::new(false));
    assert_eq!(f.store.publish(&f.graph, Some(1), &cancel).unwrap(), 2);
    for path in ["Worker.java", "worker.py"] {
        let seed = &f
            .graph
            .nodes
            .iter()
            .find(|n| n.path == path && n.name == "run")
            .unwrap()
            .id;
        for endpoint in [
            format!("/api/methods?path={path}&revision=1"),
            format!("/api/source?path={path}&revision=1"),
            "/api/files?revision=1".into(),
        ] {
            assert_eq!(call(&f.app, "GET", &endpoint, Value::Null).await.0, 409);
        }
        assert_eq!(
            call(
                &f.app,
                "POST",
                "/api/sequence",
                json!({"seed":seed,"expectedRevision":1})
            )
            .await
            .0,
            409
        );
        assert_eq!(
            call(
                &f.app,
                "POST",
                "/api/sequence",
                json!({"seed":seed,"expectedRevision":2})
            )
            .await
            .0,
            200
        );
        for (method, endpoint) in [
            ("GET", format!("/api/methods?path={path}")),
            ("GET", format!("/api/source?path={path}")),
            ("POST", "/api/sequence".into()),
            ("GET", "/api/files".into()),
            ("GET", "/api/tree".into()),
        ] {
            let response = f
                .app
                .clone()
                .oneshot(
                    Request::builder()
                        .method(method)
                        .uri(endpoint)
                        .header("host", "127.0.0.1:7331")
                        .header("content-type", "application/json")
                        .body(Body::from(
                            json!({"seed":seed,"expectedRevision":2}).to_string(),
                        ))
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), 401);
        }
    }
    assert!(!f.sentinel.exists());
}
