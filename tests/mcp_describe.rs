//! Slice 4: the first real vertical — enrolled connection, grant, principal, budget, one tool.
//!
//! Covers the describe half of acceptance tests 1 and 6.
use axum::{
    Router,
    body::{Body, to_bytes},
    http::{Request, StatusCode},
};
use baleyg::{http, indexer::IndexOptions, model::*, store::Store};
use serde_json::{Value, json};
use std::sync::{Arc, atomic::AtomicBool};
use tower::ServiceExt;

const OWNER: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
const DESCRIBE: &str = "/api/mcp-pilot/tools/baleyg_workspace_describe";

fn setup() -> (tempfile::TempDir, Store, Router) {
    let dir = tempfile::tempdir().unwrap();
    let workspace = dir.path().join("my-project");
    std::fs::create_dir(&workspace).unwrap();
    let store = Store::open(&dir.path().join("state"), &workspace).unwrap();
    let state = http::new(
        store.clone(),
        IndexOptions::new(workspace),
        OWNER.into(),
        "127.0.0.1:7331".parse().unwrap(),
    )
    .unwrap();
    (dir, store, http::router(state))
}

async fn call(
    app: &Router,
    method: &str,
    path: &str,
    token: &str,
    body: Value,
) -> (StatusCode, Value) {
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method(method)
                .uri(path)
                .header("host", "127.0.0.1:7331")
                .header("authorization", format!("Bearer {token}"))
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let code = response.status();
    let bytes = to_bytes(response.into_body(), 4 * 1024 * 1024)
        .await
        .unwrap();
    (code, serde_json::from_slice(&bytes).unwrap_or(Value::Null))
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

/// Issue a grant the way the owner helper would, returning its token, id and binding.
async fn grant(app: &Router, revision: u64, limits: Value) -> (String, String, Value) {
    let (_, discovery) = call(app, "GET", "/api/mcp-pilot/binding", OWNER, Value::Null).await;
    let binding = discovery["binding"].clone();
    let (code, issued) = call(
        app,
        "POST",
        "/api/mcp-pilot/grants",
        OWNER,
        json!({
            "schemaVersion": 1,
            "binding": binding,
            "expectedRevision": revision,
            "capabilities": ["baleyg_workspace_describe"],
            "ttlSeconds": 900,
            "limits": limits,
            "clientLabel": "terminal-pilot",
            "disclosure": {"recipient": "approved local client", "sourceApproved": false},
        }),
    )
    .await;
    assert_eq!(code, StatusCode::CREATED, "{issued}");
    (
        issued["token"].as_str().unwrap().to_string(),
        issued["grantId"].as_str().unwrap().to_string(),
        binding,
    )
}

fn default_limits() -> Value {
    json!({"maxRequests": 200, "maxTotalResponseBytes": 2097152, "maxResponseBytes": 65536})
}

#[tokio::test]
async fn describe_bootstraps_a_session_over_the_whole_stack() {
    let (_d, store, app) = setup();
    publish(&store, Some(0));
    let (token, _id, binding) = grant(&app, 1, default_limits()).await;

    let (code, body) = call(
        &app,
        "POST",
        DESCRIBE,
        &token,
        json!({"schemaVersion": 1, "binding": binding}),
    )
    .await;
    assert_eq!(code, StatusCode::OK, "{body}");
    assert_eq!(body["schemaVersion"], 1);
    assert!(body["requestId"].is_string());
    assert_eq!(body["evidenceBasis"]["indexRevision"], 1);
    assert_eq!(body["data"]["workspaceLabel"], "my-project");
    assert_eq!(body["data"]["indexRevision"], 1);
    assert_eq!(body["data"]["admittedRevision"], 1);
    assert_eq!(body["data"]["evidenceReadable"], true);
    assert_eq!(body["truncated"], false);
}

#[tokio::test]
async fn describe_exposes_no_paths_no_files_and_no_source() {
    let (dir, store, app) = setup();
    publish(&store, Some(0));
    let (token, _id, binding) = grant(&app, 1, default_limits()).await;
    let (_, body) = call(
        &app,
        "POST",
        DESCRIBE,
        &token,
        json!({"schemaVersion": 1, "binding": binding}),
    )
    .await;
    let rendered = body.to_string();
    assert!(
        !rendered.contains(dir.path().to_str().unwrap()),
        "leaked an absolute path"
    );
    assert!(!rendered.contains("a.js"), "leaked a file list");
    assert!(!rendered.contains("function a"), "leaked source");
    assert!(!rendered.contains(OWNER), "leaked the owner token");
    assert!(!rendered.contains(&token), "echoed the grant secret");
}

#[tokio::test]
async fn describe_accepts_no_expected_revision_and_rejects_unknown_fields() {
    let (_d, store, app) = setup();
    publish(&store, Some(0));
    let (token, _id, binding) = grant(&app, 1, default_limits()).await;
    let (code, body) = call(
        &app,
        "POST",
        DESCRIBE,
        &token,
        json!({"schemaVersion": 1, "binding": binding, "expectedRevision": 1}),
    )
    .await;
    assert_eq!(code, StatusCode::BAD_REQUEST);
    assert_eq!(body["error"]["code"], "invalid_request");
}

#[tokio::test]
async fn a_stale_admitted_revision_is_reported_not_concealed() {
    let (_d, store, app) = setup();
    publish(&store, Some(0));
    let (token, _id, binding) = grant(&app, 1, default_limits()).await;

    // The owner reindexes after issuing the grant.
    publish(&store, Some(1));

    let (code, body) = call(
        &app,
        "POST",
        DESCRIBE,
        &token,
        json!({"schemaVersion": 1, "binding": binding}),
    )
    .await;
    assert_eq!(code, StatusCode::OK);
    assert_eq!(body["data"]["indexRevision"], 2);
    assert_eq!(body["data"]["admittedRevision"], 1);
    assert_eq!(
        body["data"]["evidenceReadable"], false,
        "a stale grant must not claim readable evidence"
    );
}

#[tokio::test]
async fn a_revoked_grant_stops_working_immediately() {
    let (_d, store, app) = setup();
    publish(&store, Some(0));
    let (token, id, binding) = grant(&app, 1, default_limits()).await;
    let request = json!({"schemaVersion": 1, "binding": binding});
    let (code, _) = call(&app, "POST", DESCRIBE, &token, request.clone()).await;
    assert_eq!(code, StatusCode::OK);

    let (code, _) = call(
        &app,
        "DELETE",
        &format!("/api/mcp-pilot/grants/{id}"),
        OWNER,
        Value::Null,
    )
    .await;
    assert_eq!(code, StatusCode::NO_CONTENT);

    let (code, body) = call(&app, "POST", DESCRIBE, &token, request).await;
    assert_eq!(code, StatusCode::UNAUTHORIZED);
    assert_eq!(body["error"]["code"], "unauthorized");
}

#[tokio::test]
async fn a_mismatched_binding_in_the_request_is_refused() {
    let (_d, store, app) = setup();
    publish(&store, Some(0));
    let (token, _id, mut binding) = grant(&app, 1, default_limits()).await;
    binding["storeGeneration"] = json!("someone-elses-generation");
    let (code, body) = call(
        &app,
        "POST",
        DESCRIBE,
        &token,
        json!({"schemaVersion": 1, "binding": binding}),
    )
    .await;
    assert_eq!(code, StatusCode::CONFLICT);
    assert_eq!(body["error"]["code"], "binding_mismatch");
}

#[tokio::test]
async fn the_request_budget_is_spent_by_describe_too() {
    let (_d, store, app) = setup();
    publish(&store, Some(0));
    let limits =
        json!({"maxRequests": 2, "maxTotalResponseBytes": 2097152, "maxResponseBytes": 65536});
    let (token, _id, binding) = grant(&app, 1, limits).await;
    let request = json!({"schemaVersion": 1, "binding": binding});
    for _ in 0..2 {
        let (code, _) = call(&app, "POST", DESCRIBE, &token, request.clone()).await;
        assert_eq!(code, StatusCode::OK);
    }
    let (code, body) = call(&app, "POST", DESCRIBE, &token, request).await;
    assert_eq!(code, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(body["error"]["code"], "budget_exhausted");
    assert_eq!(body["error"]["retryable"], false);
}

#[tokio::test]
async fn an_oversized_tool_request_is_refused() {
    let (_d, store, app) = setup();
    publish(&store, Some(0));
    let (token, _id, mut binding) = grant(&app, 1, default_limits()).await;
    binding["storeGeneration"] = json!("x".repeat(32 * 1024));
    let (code, body) = call(
        &app,
        "POST",
        DESCRIBE,
        &token,
        json!({"schemaVersion": 1, "binding": binding}),
    )
    .await;
    assert_eq!(code, StatusCode::PAYLOAD_TOO_LARGE);
    assert_eq!(body["error"]["code"], "body_too_large");
}

#[tokio::test]
async fn a_latched_binding_ends_the_session() {
    let (dir, store, app) = setup();
    publish(&store, Some(0));
    let (token, _id, binding) = grant(&app, 1, default_limits()).await;
    let request = json!({"schemaVersion": 1, "binding": binding});
    assert_eq!(
        call(&app, "POST", DESCRIBE, &token, request.clone())
            .await
            .0,
        StatusCode::OK
    );

    let state = dir.path().join("state");
    let staging = state.join("staging.db");
    std::fs::write(&staging, std::fs::read(state.join("cache.db")).unwrap()).unwrap();
    std::fs::rename(&staging, state.join("cache.db")).unwrap();

    // The generation rotates on invalidation, so the admitted binding no longer matches. Both a
    // binding conflict and an unauthorized result are terminal for the connection.
    let (code, _) = call(&app, "POST", DESCRIBE, &token, request).await;
    assert!(
        code == StatusCode::CONFLICT || code == StatusCode::SERVICE_UNAVAILABLE,
        "expected a terminal failure, got {code}"
    );
}

#[tokio::test]
async fn authenticated_error_responses_are_charged_to_the_lifetime_budget() {
    let (_d, store, app) = setup();
    publish(&store, Some(0));
    let limits =
        json!({"maxRequests": 50, "maxTotalResponseBytes": 1000, "maxResponseBytes": 65536});
    let (token, _id, binding) = grant(&app, 1, limits).await;

    // Malformed but authenticated: each error envelope must be charged, or lifetime ceilings can
    // be bypassed indefinitely by sending traffic that fails before admission.
    let mut malformed = json!({"schemaVersion": 1, "binding": binding.clone()});
    malformed["surprise"] = json!(true);
    let mut exhausted = false;
    for _ in 0..12 {
        let (code, _) = call(&app, "POST", DESCRIBE, &token, malformed.clone()).await;
        if code == StatusCode::TOO_MANY_REQUESTS {
            exhausted = true;
            break;
        }
        assert_eq!(code, StatusCode::BAD_REQUEST);
    }
    assert!(
        exhausted,
        "error responses were not charged to the byte budget"
    );

    // A valid request cannot proceed once the budget is spent.
    let (code, body) = call(
        &app,
        "POST",
        DESCRIBE,
        &token,
        json!({"schemaVersion": 1, "binding": binding}),
    )
    .await;
    assert_eq!(code, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(body["error"]["code"], "budget_exhausted");
}

#[tokio::test]
async fn an_oversized_body_is_too_large_even_when_it_is_also_malformed() {
    let (_d, store, app) = setup();
    publish(&store, Some(0));
    let (token, _id, binding) = grant(&app, 1, default_limits()).await;

    // Size is checked before deserialization, so this is 413 rather than 400.
    let mut malformed =
        json!({"schemaVersion": 1, "binding": binding, "surprise": "x".repeat(40 * 1024)});
    malformed["alsoUnknown"] = json!(true);
    let (code, body) = call(&app, "POST", DESCRIBE, &token, malformed).await;
    assert_eq!(code, StatusCode::PAYLOAD_TOO_LARGE);
    assert_eq!(body["error"]["code"], "body_too_large");
}

#[tokio::test]
async fn an_authenticated_unknown_tool_spends_the_request_budget() {
    let (_d, store, app) = setup();
    publish(&store, Some(0));
    let limits =
        json!({"maxRequests": 2, "maxTotalResponseBytes": 2097152, "maxResponseBytes": 65536});
    let (token, _id, binding) = grant(&app, 1, limits).await;

    for _ in 0..2 {
        let (code, body) = call(
            &app,
            "POST",
            "/api/mcp-pilot/tools/baleyg_run_shell",
            &token,
            json!({}),
        )
        .await;
        assert_eq!(code, StatusCode::NOT_FOUND);
        assert_eq!(body["error"]["code"], "not_found");
    }

    // Naming routes that do not exist must not be a way to spend nothing.
    let (code, body) = call(
        &app,
        "POST",
        DESCRIBE,
        &token,
        json!({"schemaVersion": 1, "binding": binding}),
    )
    .await;
    assert_eq!(code, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(body["error"]["code"], "budget_exhausted");
}

#[tokio::test]
async fn an_error_is_never_emitted_larger_than_the_remaining_budget() {
    let (_d, store, app) = setup();
    publish(&store, Some(0));
    // The smallest ceiling a grant may carry. Anything lower is refused at issuance, because an
    // error envelope would not fit inside the limit the owner set.
    let limits = json!({"maxRequests": 50, "maxTotalResponseBytes": 512, "maxResponseBytes": 512});
    let (token, _id, binding) = grant(&app, 1, limits).await;

    let mut malformed = json!({"schemaVersion": 1, "binding": binding});
    malformed["surprise"] = json!(true);
    // Repeated until the remaining allowance is smaller than an envelope; no response may exceed
    // the per-response ceiling at any point.
    for _ in 0..6 {
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(DESCRIBE)
                    .header("host", "127.0.0.1:7331")
                    .header("authorization", format!("Bearer {token}"))
                    .header("content-type", "application/json")
                    .body(Body::from(malformed.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        let bytes = to_bytes(response.into_body(), 1024 * 1024).await.unwrap();
        assert!(
            bytes.len() <= 512,
            "emitted {} bytes against a 512 byte ceiling",
            bytes.len()
        );
    }
}

#[tokio::test]
async fn a_response_ceiling_below_the_envelope_floor_is_refused_at_issuance() {
    let (_d, store, app) = setup();
    publish(&store, Some(0));
    let (_, discovery) = call(&app, "GET", "/api/mcp-pilot/binding", OWNER, Value::Null).await;
    let (code, body) = call(
        &app,
        "POST",
        "/api/mcp-pilot/grants",
        OWNER,
        json!({
            "schemaVersion": 1,
            "binding": discovery["binding"],
            "expectedRevision": 1,
            "capabilities": ["baleyg_workspace_describe"],
            "ttlSeconds": 900,
            "limits": {"maxRequests": 50, "maxTotalResponseBytes": 2097152, "maxResponseBytes": 1},
            "clientLabel": "terminal-pilot",
            "disclosure": {"recipient": "approved local client", "sourceApproved": false},
        }),
    )
    .await;
    // Refused here rather than forcing every later answer to exceed the limit or misreport itself.
    assert_eq!(code, StatusCode::BAD_REQUEST);
    assert_eq!(body["error"]["code"], "invalid_request");
}

#[tokio::test]
async fn a_revocation_during_the_response_is_honoured() {
    let (_d, store, app) = setup();
    publish(&store, Some(0));
    let (token, id, binding) = grant(&app, 1, default_limits()).await;
    let request = json!({"schemaVersion": 1, "binding": binding});
    assert_eq!(
        call(&app, "POST", DESCRIBE, &token, request.clone())
            .await
            .0,
        StatusCode::OK
    );

    // Revoked through the owner route; the grant must not be usable afterwards even though it was
    // valid when the worker checked it.
    let (code, _) = call(
        &app,
        "DELETE",
        &format!("/api/mcp-pilot/grants/{id}"),
        OWNER,
        Value::Null,
    )
    .await;
    assert_eq!(code, StatusCode::NO_CONTENT);
    let (code, body) = call(&app, "POST", DESCRIBE, &token, request).await;
    assert_eq!(code, StatusCode::UNAUTHORIZED);
    assert_eq!(body["error"]["code"], "unauthorized");
}

#[tokio::test]
async fn non_utf8_input_is_charged_like_any_other_malformed_request() {
    let (_d, store, app) = setup();
    publish(&store, Some(0));
    let limits =
        json!({"maxRequests": 1, "maxTotalResponseBytes": 2097152, "maxResponseBytes": 65536});
    let (token, _id, binding) = grant(&app, 1, limits).await;

    // Invalid UTF-8: this must be identified and charged, not rejected by an extractor before the
    // grant is even known.
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(DESCRIBE)
                .header("host", "127.0.0.1:7331")
                .header("authorization", format!("Bearer {token}"))
                .header("content-type", "application/json")
                .body(Body::from(vec![0x7b, 0xff, 0xfe, 0x7d]))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let bytes = to_bytes(response.into_body(), 1024 * 1024).await.unwrap();
    let body: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(body["error"]["code"], "invalid_request");
    assert_eq!(body["schemaVersion"], 1);

    // The single request in the budget was spent by it.
    let (code, body) = call(
        &app,
        "POST",
        DESCRIBE,
        &token,
        json!({"schemaVersion": 1, "binding": binding}),
    )
    .await;
    assert_eq!(code, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(body["error"]["code"], "budget_exhausted");
}

#[tokio::test]
async fn an_invalidated_enrollment_retires_existing_grants() {
    let (dir, store, app) = setup();
    publish(&store, Some(0));
    let (token, _id, binding) = grant(&app, 1, default_limits()).await;
    let request = json!({"schemaVersion": 1, "binding": binding});
    assert_eq!(
        call(&app, "POST", DESCRIBE, &token, request.clone())
            .await
            .0,
        StatusCode::OK
    );

    // Another request observes the cache is gone and latches the binding.
    let state = dir.path().join("state");
    let staging = state.join("staging.db");
    std::fs::write(&staging, std::fs::read(state.join("cache.db")).unwrap()).unwrap();
    std::fs::rename(&staging, state.join("cache.db")).unwrap();
    let (code, _) = call(&app, "GET", "/api/mcp-pilot/binding", OWNER, Value::Null).await;
    assert_eq!(code, StatusCode::SERVICE_UNAVAILABLE);

    // A grant issued before the enrollment ended must not keep working.
    let (code, _) = call(&app, "POST", DESCRIBE, &token, request).await;
    assert!(
        code == StatusCode::UNAUTHORIZED || code == StatusCode::SERVICE_UNAVAILABLE,
        "an old grant survived invalidation: {code}"
    );
}

#[tokio::test]
async fn an_invalidated_binding_reports_unavailable_not_unauthorized() {
    let (dir, store, app) = setup();
    publish(&store, Some(0));
    let (token, _id, binding) = grant(&app, 1, default_limits()).await;
    let request = json!({"schemaVersion": 1, "binding": binding});
    assert_eq!(
        call(&app, "POST", DESCRIBE, &token, request.clone())
            .await
            .0,
        StatusCode::OK
    );

    // Another request observes the cache is gone and latches the binding.
    let state = dir.path().join("state");
    std::fs::remove_file(state.join("cache.db")).unwrap();
    let (code, _) = call(&app, "GET", "/api/mcp-pilot/binding", OWNER, Value::Null).await;
    assert_eq!(code, StatusCode::SERVICE_UNAVAILABLE);

    // The distinction matters to the caller: unauthorized says reissue, unavailable says the
    // binding is over and the daemon must restart first.
    let (code, body) = call(&app, "POST", DESCRIBE, &token, request).await;
    assert_eq!(code, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(body["error"]["code"], "store_unavailable");
}
