mod common;
use axum::{
    Router,
    body::{Body, to_bytes},
    http::Request,
};
use baleyg::{
    dependencies::CatalogOptions,
    http,
    indexer::{IndexOptions, index_workspace_bundle},
    model::Graph,
    store::Store,
};
use serde_json::{Value, json};
use std::sync::{Arc, atomic::AtomicBool};
use tower::ServiceExt;
const TOKEN: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

struct Fixture {
    temp: tempfile::TempDir,
    store: Store,
    state: Arc<http::DaemonState>,
    app: Router,
}
fn setup(enabled: bool) -> Fixture {
    let temp = tempfile::tempdir().unwrap();
    let workspace = temp.path().join("workspace");
    let library = temp.path().join("library");
    std::fs::create_dir_all(workspace.join("src")).unwrap();
    std::fs::create_dir_all(library.join("std/src")).unwrap();
    std::fs::write(
        workspace.join("Cargo.toml"),
        "[package]\nname = 'app'\nversion = '0.1.0'\nedition = '2021'\n",
    )
    .unwrap();
    std::fs::write(workspace.join("src/lib.rs"), "pub fn workspace_only() {}\n").unwrap();
    std::fs::write(
        library.join("std/Cargo.toml"),
        "[package]\nname = 'std'\nversion = '0.0.0'\n",
    )
    .unwrap();
    std::fs::write(library.join("std/src/lib.rs"), "// café\npub struct LibraryType;\nimpl LibraryType { pub fn method(&self) { hidden_call(); } }\npub enum Other { A }\n").unwrap();
    let store = crate::common::open_store(&temp.path().join("state"), &workspace).unwrap();
    assert!(store.status().is_err());
    let cancel = Arc::new(AtomicBool::new(false));
    let (graph, native, capture) = index_workspace_bundle(
        &IndexOptions::new(workspace.clone()),
        store.root_id(),
        &cancel,
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
            &cancel,
        )
        .unwrap();
    let state = http::new_with_dependency_options(
        store.clone(),
        IndexOptions::new(workspace.clone()),
        TOKEN.into(),
        "127.0.0.1:7331".parse().unwrap(),
        None,
        None,
        workspace,
        vec![],
        enabled.then_some(CatalogOptions {
            cargo_home: None,
            rust_library: Some(library),
        }),
    )
    .unwrap();
    let app = http::router(state.clone());
    Fixture {
        temp,
        store,
        state,
        app,
    }
}
async fn request(app: &Router, method: &str, path: &str, body: &str) -> (u16, Value) {
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method(method)
                .uri(path)
                .header("host", "127.0.0.1:7331")
                .header("authorization", format!("Bearer {TOKEN}"))
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.headers()["cache-control"], "no-store");
    let status = response.status().as_u16();
    let bytes = to_bytes(response.into_body(), 8 * 1024 * 1024)
        .await
        .unwrap();
    (status, serde_json::from_slice(&bytes).unwrap())
}
async fn ready(app: &Router) -> Value {
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        loop {
            let (status, body) = request(app, "GET", "/api/dependencies", "").await;
            assert_eq!(status, 200);
            if body["state"] != "loading" {
                assert_eq!(body["state"], "ready", "{body}");
                return body;
            }
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("dependency build timed out")
}
fn symbol_url(catalog: &Value) -> String {
    format!(
        "/api/dependencies/symbols?catalogId={}",
        catalog["catalogId"].as_str().unwrap()
    )
}
async fn source_url(app: &Router, catalog: &Value) -> String {
    let (_, symbols) = request(app, "GET", &symbol_url(catalog), "").await;
    let symbol = symbols["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["name"] == "LibraryType")
        .unwrap();
    format!(
        "/api/dependencies/source?catalogId={}&sourceRef={}",
        catalog["catalogId"].as_str().unwrap(),
        symbol["sourceRef"].as_str().unwrap()
    )
}
#[tokio::test]
async fn startup_catalog_is_separate_paged_and_source_is_explicit() {
    let fixture = setup(true);
    let before = serde_json::to_value(fixture.store.status().unwrap()).unwrap();
    fixture.state.start_dependency_index();
    let status = ready(&fixture.app).await;
    assert!(status["symbolCount"].as_u64().unwrap() >= 3);
    assert!(status.get("symbols").is_none());
    assert!(!status.to_string().contains("hidden_call"));
    assert!(
        !status
            .to_string()
            .contains(fixture.temp.path().to_str().unwrap())
    );
    let package = status["packages"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["name"] == "std")
        .unwrap();
    let (_, page) = request(
        &fixture.app,
        "GET",
        &format!(
            "{}&packageId={}&limit=1",
            symbol_url(&status),
            package["id"].as_str().unwrap()
        ),
        "",
    )
    .await;
    assert_eq!(page["items"].as_array().unwrap().len(), 1);
    assert_eq!(page["nextOffset"], 1);
    let (_, filtered) = request(
        &fixture.app,
        "GET",
        &format!("{}&q=libraryTYPE", symbol_url(&status)),
        "",
    )
    .await;
    assert!(
        filtered["items"]
            .as_array()
            .unwrap()
            .iter()
            .any(|s| s["name"] == "LibraryType")
    );
    for symbol in filtered["items"].as_array().unwrap() {
        assert!(
            !symbol["signature"]
                .as_str()
                .unwrap()
                .contains("hidden_call")
        );
    }
    let source = source_url(&fixture.app, &status).await;
    let (code, snapshot) = request(&fixture.app, "GET", &source, "").await;
    assert_eq!(code, 200, "{snapshot}");
    assert_eq!(snapshot["hash"], snapshot["file"]["hash"]);
    let definition = snapshot["definitions"]
        .as_array()
        .unwrap()
        .iter()
        .find(|d| d["name"] == "LibraryType")
        .unwrap();
    let text = snapshot["file"]["text"].as_str().unwrap();
    assert_eq!(
        &text[definition["range"]["startByte"].as_u64().unwrap() as usize
            ..definition["range"]["endByte"].as_u64().unwrap() as usize],
        "pub struct LibraryType;"
    );
    assert_eq!(
        serde_json::to_value(fixture.store.status().unwrap()).unwrap(),
        before
    );
    let external_id = definition["id"].as_str().unwrap();
    assert_eq!(
        request(
            &fixture.app,
            "POST",
            "/api/sequence",
            &json!({"seed":external_id,"expectedRevision":fixture.store.status().unwrap().revision}).to_string()
        )
        .await
        .0,
        404
    );
    let (_, workspace_symbols) =
        request(&fixture.app, "GET", "/api/symbols?q=LibraryType", "").await;
    assert!(workspace_symbols["items"].as_array().unwrap().is_empty());
}
#[tokio::test]
async fn source_hash_and_workspace_revision_reject_stale_reads() {
    let fixture = setup(true);
    fixture.state.start_dependency_index();
    let catalog = ready(&fixture.app).await;
    let source = source_url(&fixture.app, &catalog).await;
    std::fs::write(
        fixture.temp.path().join("library/std/src/lib.rs"),
        "pub struct Changed;\n",
    )
    .unwrap();
    let (code, response) = request(&fixture.app, "GET", &source, "").await;
    assert_eq!(code, 409);
    assert!(!response.to_string().contains("Changed"));
    assert_eq!(
        request(&fixture.app, "POST", "/api/dependencies/refresh", "{}")
            .await
            .0,
        202
    );
    let replacement = ready(&fixture.app).await;
    assert_ne!(replacement["catalogId"], catalog["catalogId"]);
    assert_eq!(
        request(&fixture.app, "GET", &symbol_url(&catalog), "")
            .await
            .0,
        409
    );
    let before = fixture.store.index_baseline().unwrap();
    let error = fixture
        .store
        .publish(
            &Graph::default(),
            &fixture.store.leader().unwrap(),
            before,
            &Arc::new(AtomicBool::new(false)),
        )
        .unwrap_err();
    assert!(
        error.to_string().contains("native_evidence_required"),
        "{error}"
    );
    assert_eq!(fixture.store.index_baseline().unwrap(), before);
    let workspace = fixture.temp.path().join("workspace");
    let cancel = Arc::new(AtomicBool::new(false));
    let (graph, native, capture) = index_workspace_bundle(
        &IndexOptions::new(workspace),
        fixture.store.root_id(),
        &cancel,
        |_| {},
    )
    .unwrap();
    fixture
        .store
        .publish_native(
            &graph,
            &capture,
            &native,
            &fixture.store.leader().unwrap(),
            before,
            &cancel,
        )
        .unwrap();
    assert_eq!(
        request(&fixture.app, "GET", &symbol_url(&replacement), "")
            .await
            .0,
        409
    );
    let (_, status) = request(&fixture.app, "GET", "/api/dependencies", "").await;
    assert_eq!(status["state"], "failed");
    assert_eq!(
        status["workspaceRevision"],
        serde_json::json!(fixture.store.status().unwrap().revision)
    );
    assert!(status["catalogId"].is_null());
    assert_eq!(
        request(&fixture.app, "POST", "/api/dependencies/refresh", "{}")
            .await
            .0,
        202
    );
    assert_eq!(
        ready(&fixture.app).await["workspaceRevision"],
        serde_json::json!(fixture.store.status().unwrap().revision)
    );
}
#[tokio::test]
async fn request_validation_and_capability_only_access() {
    let fixture = setup(true);
    fixture.state.start_dependency_index();
    let catalog = ready(&fixture.app).await;
    for body in [
        "",
        "null",
        "[]",
        "[{}]",
        "{\"path\":\"/etc\"}",
        "{\"force\":true}",
        "{\"expectedRevision\":0}",
    ] {
        assert_eq!(
            request(&fixture.app, "POST", "/api/dependencies/refresh", body)
                .await
                .0,
            400,
            "{body}"
        );
    }
    for suffix in [
        "&limit=0",
        "&limit=201",
        "&offset=50001",
        "&root=/etc",
        "&path=secret.rs",
    ] {
        assert_eq!(
            request(&fixture.app, "GET", &(symbol_url(&catalog) + suffix), "")
                .await
                .0,
            400
        );
    }
    let id = catalog["catalogId"].as_str().unwrap();
    for source_ref in [
        "unknown",
        "%2Fetc%2Fpasswd",
        "..%2Fsecret",
        "std/src/lib.rs",
    ] {
        assert_eq!(
            request(
                &fixture.app,
                "GET",
                &format!("/api/dependencies/source?catalogId={id}&sourceRef={source_ref}"),
                ""
            )
            .await
            .0,
            404
        );
    }
    let source = source_url(&fixture.app, &catalog).await;
    assert_eq!(
        request(&fixture.app, "GET", &(source + "&path=/etc/passwd"), "")
            .await
            .0,
        400
    );
}
#[tokio::test]
async fn all_dependency_endpoints_require_auth_host_and_origin() {
    let fixture = setup(false);
    for (method, path) in [
        ("GET", "/api/dependencies"),
        ("GET", "/api/dependencies/symbols?catalogId=x"),
        ("GET", "/api/dependencies/source?catalogId=x&sourceRef=x"),
        ("POST", "/api/dependencies/refresh"),
    ] {
        for (host, origin, token, expected) in [
            ("127.0.0.1:7331", None, None, 401),
            ("evil.test", None, Some(TOKEN), 403),
            ("127.0.0.1:7331", Some("http://evil.test"), Some(TOKEN), 403),
        ] {
            let mut req = Request::builder()
                .method(method)
                .uri(path)
                .header("host", host);
            if let Some(origin) = origin {
                req = req.header("origin", origin);
            }
            if let Some(token) = token {
                req = req.header("authorization", format!("Bearer {token}"));
            }
            assert_eq!(
                fixture
                    .app
                    .clone()
                    .oneshot(req.body(Body::from("{}")).unwrap())
                    .await
                    .unwrap()
                    .status(),
                expected
            );
        }
    }
}
#[tokio::test]
async fn disabled_and_shutdown_do_not_launch_catalog_work() {
    let fixture = setup(false);
    fixture.state.start_dependency_index();
    assert_eq!(
        request(&fixture.app, "GET", "/api/dependencies", "")
            .await
            .1["state"],
        "disabled"
    );
    assert_eq!(
        request(&fixture.app, "POST", "/api/dependencies/refresh", "{}")
            .await
            .0,
        409
    );
    let old_state = http::new(
        fixture.store.clone(),
        IndexOptions::new(fixture.temp.path().join("workspace")),
        TOKEN.into(),
        "127.0.0.1:7331".parse().unwrap(),
    )
    .unwrap();
    old_state.start_dependency_index();
    assert_eq!(
        request(&http::router(old_state), "GET", "/api/dependencies", "")
            .await
            .1["state"],
        "disabled"
    );
    let fixture = setup(true);
    // No scheduler yield between starting and shutdown: cancelled generation cannot publish.
    fixture.state.start_dependency_index();
    fixture.state.cancel_active();
    fixture.state.start_dependency_index();
    assert_eq!(
        request(&fixture.app, "POST", "/api/dependencies/refresh", "{}")
            .await
            .0,
        503
    );
    let (_, status) = request(&fixture.app, "GET", "/api/dependencies", "").await;
    assert_eq!(status["state"], "failed");
    assert!(status["catalogId"].is_null());
}
#[tokio::test]
async fn successful_workspace_index_automatically_rebuilds_catalog() {
    use std::time::{Duration, Instant};
    // The old 10s wall timer raced native capture, projection, revalidation,
    // paired CAS and other parallel macOS fixtures. Keep separate finite
    // phase budgets and one overall watchdog, with every observed transition.
    const START_BUDGET: Duration = Duration::from_secs(20);
    const PROJECT_BUDGET: Duration = Duration::from_secs(60);
    const PUBLISH_BUDGET: Duration = Duration::from_secs(60);
    const JOB_BUDGET: Duration = Duration::from_secs(120);
    const CATALOG_BUDGET: Duration = Duration::from_secs(45);
    const HTTP_BUDGET: Duration = Duration::from_secs(10);
    const SAMPLE: Duration = Duration::from_millis(20);

    let fixture = setup(true);
    fixture.state.start_dependency_index();
    let old = ready(&fixture.app).await;
    let old_revision = json!(fixture.store.status().unwrap().revision);
    assert_eq!(old["workspaceRevision"], old_revision);
    let old_catalog_id = old["catalogId"].as_str().expect("old ready catalog ID");
    let (accepted, started_job) = request(&fixture.app, "POST", "/api/index", "{}").await;
    assert_eq!(accepted, 202, "index admission response: {started_job}");
    let job_id = started_job["id"].as_str().expect("202 must contain job ID");
    assert_eq!(started_job["state"], "running", "{started_job}");
    let job_path = format!("/api/jobs/{job_id}");
    let began = Instant::now();
    let mut stage_started = began;
    let mut stage = "start";
    let mut last_job = started_job.clone();
    let mut last_state = started_job["state"].as_str().unwrap().to_owned();
    let mut last_phase = started_job["progress"]["phase"]
        .as_str()
        .expect("202 must include progress.phase")
        .to_owned();
    let mut transitions = vec![format!("0ms state={last_state} phase={last_phase:?}")];
    let new_revision = loop {
        let (status, job) = tokio::time::timeout(
            HTTP_BUDGET, request(&fixture.app, "GET", &job_path, ""),
        ).await.unwrap_or_else(|elapsed| panic!(
            "job {job_id} GET stalled: {elapsed}; stage={stage} stageElapsed={:?} totalElapsed={:?}; transitions={transitions:?}; lastJob={last_job}; oldRevision={old_revision}",
            stage_started.elapsed(), began.elapsed(),
        ));
        assert_eq!(
            status, 200,
            "job {job_id} GET status={status}; body={job}; stage={stage}; transitions={transitions:?}; lastJob={last_job}"
        );
        assert_eq!(
            job["id"].as_str(),
            Some(job_id),
            "wrong/missing job ID; expected={job_id}; body={job}; transitions={transitions:?}"
        );
        let state = job["state"]
            .as_str()
            .unwrap_or_else(|| panic!("missing job state: {job}"));
        let progress = &job["progress"];
        let phase = progress["phase"]
            .as_str()
            .unwrap_or_else(|| panic!("missing job progress.phase: {job}"));
        assert!(
            progress["completed"].as_u64().is_some() && progress["total"].as_u64().is_some(),
            "malformed job progress counters: {job}"
        );
        assert!(
            matches!(phase, "" | "parse" | "complete"),
            "unexpected progress phase: {job}"
        );
        if state != last_state || phase != last_phase {
            transitions.push(format!(
                "{:?} state={state} phase={phase:?} progress={}/{} revision={} error={}",
                began.elapsed(),
                progress["completed"],
                progress["total"],
                job["revision"],
                job["error"]
            ));
            last_state = state.to_owned();
            last_phase = phase.to_owned();
        }
        last_job = job.clone();
        match state {
            "failed" | "cancelled" | "cancelling" => panic!(
                "job {job_id} ended {state}: {job}; transitions={transitions:?}; oldRevision={old_revision}"
            ),
            "running" | "completed" => {}
            _ => panic!("job {job_id} unexpected state: {job}; transitions={transitions:?}"),
        }
        assert!(
            job["error"].is_null(),
            "job {job_id} exposes unexpected error: {job}; transitions={transitions:?}"
        );
        if phase == "complete" && stage != "publish/commit" {
            stage = "publish/commit";
            stage_started = Instant::now();
            transitions.push(format!(
                "{:?} graph projection complete; publication/commit not yet proven",
                began.elapsed()
            ));
        } else if phase == "parse" && stage == "start" {
            stage = "capture/project";
            stage_started = Instant::now();
            transitions.push(format!(
                "{:?} capture/project progress observed",
                began.elapsed()
            ));
        }
        let phase_budget = match stage {
            "start" => START_BUDGET,
            "capture/project" => PROJECT_BUDGET,
            _ => PUBLISH_BUDGET,
        };
        if began.elapsed() > JOB_BUDGET || stage_started.elapsed() > phase_budget {
            let current = fixture
                .store
                .status()
                .map(|s| json!(s.revision))
                .unwrap_or_else(|e| json!({"statusError":e.to_string()}));
            panic!(
                "job {job_id} exceeded bounded {stage} budget {phase_budget:?} or overall {JOB_BUDGET:?}; stageElapsed={:?} totalElapsed={:?}; transitions={transitions:?}; lastJob={last_job}; oldRevision={old_revision}; currentStoreRevision={current}",
                stage_started.elapsed(),
                began.elapsed()
            );
        }
        if state == "completed" {
            assert_eq!(
                phase, "complete",
                "job completed without projection evidence: {job}"
            );
            let revision = job
                .get("revision")
                .filter(|value| !value.is_null())
                .unwrap_or_else(|| panic!("completed job has no revision: {job}"));
            assert!(
                revision["indexGeneration"].as_str().is_some()
                    && revision["indexRevision"].as_u64().is_some(),
                "malformed completed revision: {job}"
            );
            assert_ne!(
                revision, &old_revision,
                "job completed without changing the workspace revision: {job}"
            );
            let current = json!(fixture.store.status().unwrap().revision);
            assert_eq!(
                revision, &current,
                "job/store revision mismatch; transitions={transitions:?}; job={job}"
            );
            break revision.clone();
        }
        assert!(
            job["revision"].is_null(),
            "running job already has revision: {job}"
        );
        tokio::time::sleep(SAMPLE).await;
    };

    // Job completion precedes the automatic dependency-index trigger. Do not
    // accept the old ready catalog just because its state is still `ready`.
    let catalog_started = Instant::now();
    let mut last_catalog = old.clone();
    let new = loop {
        let (status, catalog) = tokio::time::timeout(
            HTTP_BUDGET, request(&fixture.app, "GET", "/api/dependencies", ""),
        ).await.unwrap_or_else(|elapsed| panic!(
            "catalog GET stalled after job {job_id}: {elapsed}; catalogElapsed={:?}; oldCatalogId={old_catalog_id}; jobRevision={new_revision}; lastCatalog={last_catalog}; lastJob={last_job}; transitions={transitions:?}",
            catalog_started.elapsed(),
        ));
        assert_eq!(
            status, 200,
            "catalog GET failed status={status}; body={catalog}; jobRevision={new_revision}; lastJob={last_job}"
        );
        let catalog_state = catalog["state"]
            .as_str()
            .unwrap_or_else(|| panic!("malformed catalog state: {catalog}"));
        assert!(
            matches!(catalog_state, "loading" | "ready" | "failed"),
            "unexpected catalog state: {catalog}"
        );
        let fresh = catalog_state == "ready"
            && catalog["workspaceRevision"] == new_revision
            && catalog["catalogId"]
                .as_str()
                .is_some_and(|id| id != old_catalog_id);
        last_catalog = catalog.clone();
        if fresh {
            break catalog;
        }
        if catalog_started.elapsed() > CATALOG_BUDGET {
            panic!(
                "automatic dependency catalog did not reach NEW ready revision within {CATALOG_BUDGET:?}; elapsed={:?}; oldCatalogId={old_catalog_id}; oldRevision={old_revision}; jobRevision={new_revision}; lastCatalog={last_catalog}; lastJob={last_job}; transitions={transitions:?}",
                catalog_started.elapsed()
            );
        }
        tokio::time::sleep(SAMPLE).await;
    };
    assert_eq!(new["workspaceRevision"], new_revision);
    assert_ne!(old["catalogId"], new["catalogId"]);
    assert_eq!(
        new["workspaceRevision"],
        json!(fixture.store.status().unwrap().revision)
    );
}
#[cfg(unix)]
#[tokio::test]
async fn sources_keep_pinned_roots_and_reject_symlink_replacement() {
    let fixture = setup(true);
    fixture.state.start_dependency_index();
    let catalog = ready(&fixture.app).await;
    let source = source_url(&fixture.app, &catalog).await;
    let library = fixture.temp.path().join("library");
    let moved = fixture.temp.path().join("moved-library");
    std::fs::rename(&library, &moved).unwrap();
    std::fs::create_dir_all(library.join("std/src")).unwrap();
    std::fs::write(
        library.join("std/src/lib.rs"),
        "replacement must not be read",
    )
    .unwrap();
    assert_eq!(request(&fixture.app, "GET", &source, "").await.0, 200);
    std::fs::remove_file(moved.join("std/src/lib.rs")).unwrap();
    std::os::unix::fs::symlink(library.join("std/src/lib.rs"), moved.join("std/src/lib.rs"))
        .unwrap();
    let (status, response) = request(&fixture.app, "GET", &source, "").await;
    assert_eq!(status, 403);
    assert!(!response.to_string().contains("replacement"));
}

#[tokio::test]
async fn workspace_generation_reuse() {
    use baleyg::store::topology::UseGuard;
    let fixture = setup(true);
    fixture.state.start_dependency_index();
    let catalog = ready(&fixture.app).await;
    let old = fixture.store.status().unwrap().revision;
    let source = source_url(&fixture.app, &catalog).await;
    let indexes = fixture.temp.path().join("state/cache/indexes");
    let index = std::fs::read_dir(&indexes)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .find(|p| p.is_dir())
        .unwrap();
    let lock = indexes.join(format!(
        "{}.lock",
        index.file_name().unwrap().to_string_lossy()
    ));
    let exclusive = UseGuard::acquire_existing(&lock, true, true).unwrap();
    std::fs::remove_file(index.join("index.db")).unwrap();
    std::fs::remove_file(index.join("leader.lock")).unwrap();
    std::fs::remove_dir(index).unwrap();
    exclusive.remove_last().unwrap();
    let replacement = crate::common::open_store(
        &fixture.temp.path().join("state"),
        &fixture.temp.path().join("workspace"),
    )
    .unwrap();
    assert!(replacement.status().is_err());
    let cancel = Arc::new(AtomicBool::new(false));
    let (graph, native, capture) = index_workspace_bundle(
        &IndexOptions::new(fixture.temp.path().join("workspace")),
        replacement.root_id(),
        &cancel,
        |_| {},
    )
    .unwrap();
    let fresh = replacement
        .publish_native(
            &graph,
            &capture,
            &native,
            &replacement.leader().unwrap(),
            replacement.index_baseline().unwrap(),
            &cancel,
        )
        .unwrap();
    assert_eq!(fresh.index_revision, old.index_revision);
    assert_ne!(fresh.index_generation, old.index_generation);
    assert_eq!(
        request(&fixture.app, "GET", &symbol_url(&catalog), "")
            .await
            .0,
        409
    );
    assert_eq!(request(&fixture.app, "GET", &source, "").await.0, 409);
    let (_, status) = request(&fixture.app, "GET", "/api/dependencies", "").await;
    assert_eq!(status["workspaceRevision"], serde_json::json!(fresh));
    assert_eq!(status["state"], "failed");
}
