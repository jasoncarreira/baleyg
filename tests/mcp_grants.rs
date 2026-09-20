//! Slice 2: owner-only grant bootstrap, budgets and control routes.
//!
//! Covers acceptance test 3 (bootstrap authority) and the owner half of test 8 (expiry/revoke).
use axum::{
    Router,
    body::{Body, to_bytes},
    http::{Request, StatusCode},
};
use baleyg::{http, indexer::IndexOptions, model::*, store::Store};
use serde_json::{Value, json};
use std::sync::{Arc, atomic::AtomicBool};
use tower::ServiceExt;

const TOKEN: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

/// Replace a database with a different inode, the way an external restore would.
fn replace_with_new_inode(state: &std::path::Path, name: &str) {
    let staging = state.join("staging.db");
    std::fs::write(&staging, std::fs::read(state.join(name)).unwrap()).unwrap();
    std::fs::rename(&staging, state.join(name)).unwrap();
}

fn setup() -> (tempfile::TempDir, Store, Router) {
    let dir = tempfile::tempdir().unwrap();
    let workspace = dir.path().join("workspace");
    std::fs::create_dir(&workspace).unwrap();
    let store = Store::open(&dir.path().join("state"), &workspace).unwrap();
    let state = http::new(
        store.clone(),
        IndexOptions::new(workspace),
        TOKEN.into(),
        "127.0.0.1:7331".parse().unwrap(),
    )
    .unwrap();
    let router = http::router(state);
    (dir, store, router)
}

async fn call(
    app: &Router,
    method: &str,
    path: &str,
    token: Option<&str>,
    body: Value,
) -> (StatusCode, Value) {
    let mut req = Request::builder()
        .method(method)
        .uri(path)
        .header("host", "127.0.0.1:7331")
        .header("content-type", "application/json");
    if let Some(token) = token {
        req = req.header("authorization", format!("Bearer {token}"));
    }
    let response = app
        .clone()
        .oneshot(req.body(Body::from(body.to_string())).unwrap())
        .await
        .unwrap();
    let code = response.status();
    let bytes = to_bytes(response.into_body(), 2 * 1024 * 1024)
        .await
        .unwrap();
    (code, serde_json::from_slice(&bytes).unwrap_or(Value::Null))
}

async fn owner(app: &Router, method: &str, path: &str, body: Value) -> (StatusCode, Value) {
    call(app, method, path, Some(TOKEN), body).await
}

fn publish(store: &Store, expected: Option<u64>) {
    let graph = Graph {
        files: vec![SourceFile {
            path: "a.js".into(),
            hash: "hash".into(),
            language: "javascript".into(),
            text: "function a() {}".into(),
        }],
        nodes: vec![Symbol {
            id: "a".into(),
            name: "a".into(),
            kind: SymbolKind::Function,
            path: "a.js".into(),
            range: SourceRange {
                start_line: 1,
                start_column: 1,
                end_line: 1,
                end_column: 1,
                ..SourceRange::default()
            },
            parent: None,
            accessor: false,
            provenance: Provenance {
                source: "syntax".into(),
                semantic: SemanticState::Unavailable,
            },
        }],
        ..Graph::default()
    };
    store
        .publish(
            &graph,
            expected,
            &(Arc::new(AtomicBool::new(false)) as CancelFlag),
        )
        .unwrap();
}

fn issue_body(binding: &Value, revision: u64, capabilities: Value, source_approved: bool) -> Value {
    json!({
        "schemaVersion": 1,
        "binding": binding,
        "expectedRevision": revision,
        "capabilities": capabilities,
        "ttlSeconds": 900,
        "limits": {"maxRequests": 200, "maxTotalResponseBytes": 2097152, "maxResponseBytes": 65536},
        "clientLabel": "terminal-pilot",
        "disclosure": {"recipient": "approved local client", "sourceApproved": source_approved},
    })
}

const ALL: [&str; 4] = [
    "baleyg_workspace_describe",
    "baleyg_find_symbols",
    "baleyg_inspect",
    "baleyg_read_source",
];

#[tokio::test]
async fn binding_discovery_reports_an_unindexed_store_as_revision_zero() {
    let (_d, store, app) = setup();
    let (code, body) = owner(&app, "GET", "/api/mcp-pilot/binding", Value::Null).await;
    assert_eq!(code, StatusCode::OK);
    assert_eq!(body["indexRevision"], 0);
    assert_eq!(body["evidenceReadable"], false);
    assert!(body["binding"]["daemonInstanceId"].is_string());

    publish(&store, Some(0));
    let (_, body) = owner(&app, "GET", "/api/mcp-pilot/binding", Value::Null).await;
    assert_eq!(body["indexRevision"], 1);
    assert_eq!(body["evidenceReadable"], true);
}

#[tokio::test]
async fn issuance_against_an_unindexed_store_is_no_published_index() {
    let (_d, _store, app) = setup();
    let (_, discovery) = owner(&app, "GET", "/api/mcp-pilot/binding", Value::Null).await;
    let body = issue_body(&discovery["binding"], 1, json!(ALL), true);
    let (code, body) = owner(&app, "POST", "/api/mcp-pilot/grants", body).await;
    assert_eq!(code, StatusCode::CONFLICT);
    assert_eq!(body["error"]["code"], "no_published_index");
    assert_eq!(body["error"]["retryable"], false);
    assert!(body["token"].is_null(), "no grant or token may be created");
}

#[tokio::test]
async fn issuance_checks_binding_then_revision() {
    let (_d, store, app) = setup();
    publish(&store, Some(0));
    let (_, discovery) = owner(&app, "GET", "/api/mcp-pilot/binding", Value::Null).await;

    let mut wrong = discovery["binding"].clone();
    wrong["storeGeneration"] = json!("someone-elses-generation");
    let (code, body) = owner(
        &app,
        "POST",
        "/api/mcp-pilot/grants",
        issue_body(&wrong, 1, json!(ALL), true),
    )
    .await;
    assert_eq!(code, StatusCode::CONFLICT);
    assert_eq!(body["error"]["code"], "binding_mismatch");

    let (code, body) = owner(
        &app,
        "POST",
        "/api/mcp-pilot/grants",
        issue_body(&discovery["binding"], 99, json!(ALL), true),
    )
    .await;
    assert_eq!(code, StatusCode::CONFLICT);
    assert_eq!(body["error"]["code"], "revision_conflict");
}

#[tokio::test]
async fn a_valid_request_issues_one_grant_and_returns_its_token_once() {
    let (_d, store, app) = setup();
    publish(&store, Some(0));
    let (_, discovery) = owner(&app, "GET", "/api/mcp-pilot/binding", Value::Null).await;
    let (code, body) = owner(
        &app,
        "POST",
        "/api/mcp-pilot/grants",
        issue_body(&discovery["binding"], 1, json!(ALL), true),
    )
    .await;
    assert_eq!(code, StatusCode::CREATED);
    assert!(body["token"].as_str().unwrap().starts_with("bgp_"));
    assert_eq!(body["admittedRevision"], 1);
    assert_eq!(body["capabilities"].as_array().unwrap().len(), 4);
    assert_eq!(body["effectiveLimits"]["maxResponseBytes"], 65536);

    // There is no retrieval endpoint: the grant identifier is not exchangeable for the token.
    let grant_id = body["grantId"].as_str().unwrap().to_string();
    let (code, _) = owner(
        &app,
        "GET",
        &format!("/api/mcp-pilot/grants/{grant_id}"),
        Value::Null,
    )
    .await;
    assert_eq!(code, StatusCode::METHOD_NOT_ALLOWED);
}

#[tokio::test]
async fn malformed_and_over_ceiling_requests_are_refused() {
    let (_d, store, app) = setup();
    publish(&store, Some(0));
    let (_, discovery) = owner(&app, "GET", "/api/mcp-pilot/binding", Value::Null).await;
    let binding = &discovery["binding"];

    let mut unknown_field = issue_body(binding, 1, json!(ALL), true);
    unknown_field["surprise"] = json!(true);
    let (code, body) = owner(&app, "POST", "/api/mcp-pilot/grants", unknown_field).await;
    assert_eq!(code, StatusCode::BAD_REQUEST);
    assert_eq!(body["error"]["code"], "invalid_request");

    let (code, _) = owner(
        &app,
        "POST",
        "/api/mcp-pilot/grants",
        issue_body(binding, 1, json!(["baleyg_run_shell"]), true),
    )
    .await;
    assert_eq!(code, StatusCode::BAD_REQUEST);

    // Describe is mandatory.
    let (code, _) = owner(
        &app,
        "POST",
        "/api/mcp-pilot/grants",
        issue_body(binding, 1, json!(["baleyg_find_symbols"]), true),
    )
    .await;
    assert_eq!(code, StatusCode::BAD_REQUEST);

    let mut over = issue_body(binding, 1, json!(ALL), true);
    over["limits"]["maxRequests"] = json!(5000);
    let (code, body) = owner(&app, "POST", "/api/mcp-pilot/grants", over).await;
    assert_eq!(code, StatusCode::BAD_REQUEST);
    assert_eq!(body["error"]["code"], "invalid_request");
}

#[tokio::test]
async fn source_capabilities_are_refused_without_explicit_approval() {
    let (_d, store, app) = setup();
    publish(&store, Some(0));
    let (_, discovery) = owner(&app, "GET", "/api/mcp-pilot/binding", Value::Null).await;
    let binding = &discovery["binding"];

    for capability in ["baleyg_inspect", "baleyg_read_source"] {
        let body = issue_body(
            binding,
            1,
            json!(["baleyg_workspace_describe", capability]),
            false,
        );
        let (code, body) = owner(&app, "POST", "/api/mcp-pilot/grants", body).await;
        assert_eq!(code, StatusCode::FORBIDDEN, "{capability} needs approval");
        assert_eq!(body["error"]["code"], "forbidden");
    }

    // Structural capabilities remain issuable under the owner's metadata approval alone.
    let body = issue_body(
        binding,
        1,
        json!(["baleyg_workspace_describe", "baleyg_find_symbols"]),
        false,
    );
    let (code, _) = owner(&app, "POST", "/api/mcp-pilot/grants", body).await;
    assert_eq!(code, StatusCode::CREATED);
}

#[tokio::test]
async fn revocation_is_idempotent_and_never_reveals_which_grants_exist() {
    let (_d, store, app) = setup();
    publish(&store, Some(0));
    let (_, discovery) = owner(&app, "GET", "/api/mcp-pilot/binding", Value::Null).await;
    let (_, issued) = owner(
        &app,
        "POST",
        "/api/mcp-pilot/grants",
        issue_body(&discovery["binding"], 1, json!(ALL), true),
    )
    .await;
    let grant_id = issued["grantId"].as_str().unwrap().to_string();

    for path in [
        format!("/api/mcp-pilot/grants/{grant_id}"),
        format!("/api/mcp-pilot/grants/{grant_id}"),
        "/api/mcp-pilot/grants/no-such-grant".to_string(),
    ] {
        let (code, _) = owner(&app, "DELETE", &path, Value::Null).await;
        assert_eq!(code, StatusCode::NO_CONTENT);
    }
}

#[tokio::test]
async fn the_control_routes_require_the_owner_bearer() {
    let (_d, store, app) = setup();
    publish(&store, Some(0));
    for (method, path) in [
        ("GET", "/api/mcp-pilot/binding"),
        ("POST", "/api/mcp-pilot/grants"),
        ("DELETE", "/api/mcp-pilot/grants/anything"),
    ] {
        let (code, _) = call(&app, method, path, None, Value::Null).await;
        assert_eq!(code, StatusCode::UNAUTHORIZED, "{method} {path}");
        let (code, _) = call(
            &app,
            method,
            path,
            Some("bgp_not_the_owner_token"),
            Value::Null,
        )
        .await;
        assert_eq!(code, StatusCode::UNAUTHORIZED, "{method} {path}");
    }
}

#[tokio::test]
async fn a_latched_binding_reports_an_unavailable_store_not_a_mismatch() {
    let (dir, store, app) = setup();
    publish(&store, Some(0));
    let state = dir.path().join("state");
    let (_, discovery) = owner(&app, "GET", "/api/mcp-pilot/binding", Value::Null).await;

    // An external restore between discovery and issuance.
    replace_with_new_inode(&state, "cache.db");

    let (code, body) = owner(&app, "GET", "/api/mcp-pilot/binding", Value::Null).await;
    assert_eq!(code, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(body["error"]["code"], "store_unavailable");

    let (code, body) = owner(
        &app,
        "POST",
        "/api/mcp-pilot/grants",
        issue_body(&discovery["binding"], 1, json!(ALL), true),
    )
    .await;
    assert_eq!(code, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(body["error"]["code"], "store_unavailable");

    // Republishing must not clear the latch or allow a new grant in this daemon lifetime.
    publish(&store, None);
    let (code, _) = owner(&app, "GET", "/api/mcp-pilot/binding", Value::Null).await;
    assert_eq!(code, StatusCode::SERVICE_UNAVAILABLE);
}
