mod common;
use axum::{
    Router,
    body::{Body, to_bytes},
    http::Request,
};
use baleyg::{
    http,
    indexer::{IndexOptions, index_workspace},
    model::{CancelFlag, Graph, IndexPin},
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
        root.join("Types.java"),
        "class A { int value; void run() { save(); } }",
    )
    .unwrap();
    let options = IndexOptions::new(root.clone());
    let cancel: CancelFlag = Arc::new(AtomicBool::new(false));
    let graph = index_workspace(&options, &cancel, |_| {}).unwrap();
    let id = graph
        .nodes
        .iter()
        .find(|n| n.name == "run")
        .unwrap()
        .id
        .clone();
    let store = crate::common::open_store(&temp.path().join("state"), &root).unwrap();
    let baseline = store.status().unwrap().revision;
    store
        .publish(&graph, &store.leader().unwrap(), baseline, &cancel)
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
    (temp, store, graph, app, id)
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
    let bytes = to_bytes(response.into_body(), 4 * 1024 * 1024)
        .await
        .unwrap();
    (code, serde_json::from_slice(&bytes).unwrap_or(Value::Null))
}
fn query(pin: IndexPin) -> String {
    format!(
        "indexGeneration={}&indexRevision={}",
        pin.index_generation, pin.index_revision
    )
}
#[tokio::test]
async fn optional_endpoint_matrix() {
    let (_temp, store, _graph, app, id) = fixture();
    let pin = store.status().unwrap().revision;
    let old = IndexPin {
        index_generation: pin.index_generation,
        index_revision: 0,
    };
    for path in [
        "/api/files",
        "/api/methods?path=Types.java",
        "/api/classes",
        "/api/symbol?",
        "/api/source?path=Types.java",
    ] {
        let path = if path.ends_with('?') {
            format!("{path}id={id}")
        } else {
            path.to_owned()
        };
        assert_eq!(call(&app, "GET", &path, Value::Null).await.0, 200, "{path}");
        let separator = if path.contains('?') { '&' } else { '?' };
        let pinned = format!("{path}{separator}{}", query(pin));
        let (status, value) = call(&app, "GET", &pinned, Value::Null).await;
        assert_eq!(status, 200, "{pinned}: {value}");
        assert_eq!(value["revision"], json!(pin));
        let stale = format!("{path}{separator}{}", query(old));
        assert_eq!(
            call(&app, "GET", &stale, Value::Null).await.0,
            409,
            "{stale}"
        );
        for bad in [
            format!("{path}{separator}indexRevision=1"),
            format!("{path}{separator}indexGeneration={}", pin.index_generation),
            format!("{path}{separator}revision=1"),
            format!("{pinned}&indexRevision=1"),
            format!("{path}{separator}indexGeneration=no&indexRevision=1"),
        ] {
            assert_eq!(call(&app, "GET", &bad, Value::Null).await.0, 400, "{bad}");
        }
    }
}
#[tokio::test]
async fn producer_endpoint_matrix() {
    let (_temp, store, _graph, app, id) = fixture();
    let pin = store.status().unwrap().revision;
    for path in ["/api/status", "/api/tree", "/api/symbols?q=run"] {
        let (status, value) = call(&app, "GET", path, Value::Null).await;
        assert_eq!(status, 200, "{path}: {value}");
        assert_eq!(value["revision"], json!(pin));
        let separator = if path.contains('?') { '&' } else { '?' };
        assert_eq!(
            call(
                &app,
                "GET",
                &format!("{path}{separator}{}", query(pin)),
                Value::Null
            )
            .await
            .0,
            400
        );
    }
    let (status, result) = call(&app, "POST", "/api/query", json!({"seed":id})).await;
    assert_eq!(status, 200);
    assert_eq!(result["revision"], json!(pin));
    assert_eq!(
        call(
            &app,
            "POST",
            "/api/query",
            json!({"seed":id,"expectedRevision":pin})
        )
        .await
        .0,
        400
    );
}
#[tokio::test]
async fn required_endpoint_matrix() {
    let (_temp, store, _graph, app, id) = fixture();
    let pin = store.status().unwrap().revision;
    let stale = IndexPin {
        index_generation: uuid::Uuid::new_v4(),
        index_revision: pin.index_revision,
    };
    let class = store
        .classes_at(None, "A", None, 0, 10)
        .unwrap()
        .items
        .into_iter()
        .next()
        .unwrap();
    let first = class.fields.into_iter().next().unwrap();
    let selectors = [
        ("/api/sequence", json!({"seed":id,"expectedRevision":pin})),
        (
            "/api/navigation",
            json!({"path":"Types.java","line":1,"expectedRevision":pin}),
        ),
        (
            "/api/navigation",
            json!({"classId":class.symbol.id,"memberName":first.name,"startByte":first.range.start_byte,"endByte":first.range.end_byte,"expectedRevision":pin}),
        ),
        (
            "/api/class-diagram",
            json!({"seed":class.symbol.id,"expectedRevision":pin}),
        ),
        (
            "/api/questions/preview",
            json!({"seed":id,"question":"What runs?","expectedRevision":pin}),
        ),
    ];
    for (path, body) in selectors {
        let (status, value) = call(&app, "POST", path, body.clone()).await;
        assert_eq!(status, 200, "{path}: {value}");
        let response_pin = if path == "/api/questions/preview" {
            &value["packet"]["revision"]
        } else {
            &value["revision"]
        };
        assert_eq!(*response_pin, json!(pin));
        for bad in [
            json!(1),
            json!(null),
            json!({"indexGeneration":pin.index_generation.to_string()}),
        ] {
            let mut invalid = body.clone();
            invalid["expectedRevision"] = bad;
            assert_eq!(call(&app, "POST", path, invalid).await.0, 400, "{path}");
        }
        let mut conflict = body;
        conflict["expectedRevision"] = json!(stale);
        assert_eq!(call(&app, "POST", path, conflict).await.0, 409, "{path}");
    }
}
#[tokio::test]
async fn index_admission_and_publication_pair() {
    let (_temp, store, _graph, app, _id) = fixture();
    let pin = store.status().unwrap().revision;
    for bad in [json!(null), json!(1), json!({"indexRevision":1})] {
        assert_eq!(
            call(&app, "POST", "/api/index", json!({"expectedRevision":bad}))
                .await
                .0,
            400
        );
    }
    let conflict = IndexPin {
        index_generation: uuid::Uuid::new_v4(),
        index_revision: pin.index_revision,
    };
    assert_eq!(
        call(
            &app,
            "POST",
            "/api/index",
            json!({"expectedRevision":conflict})
        )
        .await
        .0,
        409
    );
    let (code, job) = call(&app, "POST", "/api/index", json!({"expectedRevision":pin})).await;
    assert_eq!(code, 202, "{job}");
    assert!(job["revision"].is_null());
    let id = job["id"].as_str().unwrap();
    let completed = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            let (_, result) = call(&app, "GET", &format!("/api/jobs/{id}"), Value::Null).await;
            if !result["finishedAt"].is_null() {
                break result;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert_eq!(completed["state"], "completed", "{completed}");
    assert_eq!(
        completed["revision"],
        json!(store.status().unwrap().revision)
    );
    assert_eq!(
        store.status().unwrap().revision.index_revision,
        pin.index_revision + 1
    );
}

#[test]
fn sqlite_journal_child() {
    let Some(path) = std::env::var_os("BALEYG_SQLITE_JOURNAL_CHILD") else {
        return;
    };
    let db = rusqlite::Connection::open(path).unwrap();
    db.execute_batch("PRAGMA cache_size=10; BEGIN IMMEDIATE; UPDATE index_metadata SET index_revision=3 WHERE singleton=1; UPDATE files SET payload=hex(randomblob(2048)) WHERE path LIKE 'spill-%'")
        .unwrap();
    println!("JOURNAL_READY");
    use std::io::Write;
    std::io::stdout().flush().unwrap();
    let mut input = String::new();
    std::io::stdin().read_line(&mut input).unwrap();
    db.execute_batch("ROLLBACK").unwrap();
}

#[tokio::test]
async fn active_and_hot_journal_keep_pinned_http_safe() {
    use std::io::BufRead;
    use std::process::{Command, Stdio};
    let (temp, store, _graph, app, _id) = fixture();
    let previous = store.status().unwrap().revision;
    let db_path = std::fs::read_dir(temp.path().join("state/cache/indexes"))
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .find(|path| path.is_dir())
        .unwrap()
        .join("index.db");
    let db = rusqlite::Connection::open(&db_path).unwrap();
    db.execute_batch("BEGIN IMMEDIATE").unwrap();
    for i in 0..100 {
        db.execute(
            "INSERT INTO files(path,hash,payload) VALUES(?1,'x',?2)",
            rusqlite::params![format!("spill-{i}"), "x".repeat(4096)],
        )
        .unwrap();
    }
    db.execute_batch("COMMIT").unwrap();
    drop(db);
    let journal = db_path.with_file_name("index.db-journal");
    let leader_path = db_path.with_file_name("leader.lock");
    let leader = store.leader().unwrap();
    let mut child = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "sqlite_journal_child", "--nocapture"])
        .env("BALEYG_SQLITE_JOURNAL_CHILD", &db_path)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut output = std::io::BufReader::new(child.stdout.take().unwrap());
    let mut line = String::new();
    while output.read_line(&mut line).unwrap() != 0 && !line.contains("JOURNAL_READY") {
        line.clear();
    }
    assert!(
        line.contains("JOURNAL_READY"),
        "writer exited before barrier: {line}"
    );
    assert!(journal.exists());
    match store.status() {
        Ok(status) => assert_eq!(status.revision, previous),
        Err(error) => assert!(error.to_string().contains("storage_busy"), "{error:#}"),
    }
    let url = format!("/api/source?path=Types.java&{}", query(previous));
    let (code, body) = call(&app, "GET", &url, Value::Null).await;
    assert!(
        code == 200 || ((code == 409 || code == 503) && body["error"]["code"] == "storage_busy"),
        "{code}: {body}"
    );
    child.kill().unwrap();
    child.wait().unwrap();
    assert_eq!(
        &std::fs::read(&journal).unwrap()[..8],
        &[0xd9, 0xd5, 0x05, 0xf9, 0x20, 0xa1, 0x63, 0xd7],
        "SQLite did not flush a genuine hot journal"
    );
    drop(leader);
    let unrelated =
        baleyg::store::topology::UseGuard::acquire_existing(&leader_path, true, true).unwrap();
    let before = [
        std::fs::read(&db_path).unwrap(),
        std::fs::read(&journal).unwrap(),
        std::fs::read(&leader_path).unwrap(),
    ];
    assert!(
        store
            .status()
            .unwrap_err()
            .to_string()
            .contains("recovery_required")
    );
    let (code, body) = call(&app, "GET", &url, Value::Null).await;
    assert_eq!(code, 503, "{body}");
    assert_eq!(body["error"]["code"], "recovery_required");
    assert!(store.leader().is_err());
    assert_eq!(
        before,
        [
            std::fs::read(&db_path).unwrap(),
            std::fs::read(&journal).unwrap(),
            std::fs::read(&leader_path).unwrap()
        ]
    );
    drop(unrelated);
}
