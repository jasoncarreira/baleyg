//! Slice 3: the principal split and default-deny dispatch.
//!
//! Covers acceptance test 4 (server authorization). The sweep below walks the daemon's whole route
//! table rather than a sample: a limited grant must be denied everywhere except the tool prefix,
//! and the owner bearer must be denied on the tool prefix.
use axum::{
    Router,
    body::{Body, to_bytes},
    http::{Request, StatusCode},
};
use baleyg::{http, indexer::IndexOptions, store::Store};
use serde_json::{Value, json};
use tower::ServiceExt;

const OWNER: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
/// Well formed but never issued. The guard must not consult the grant table, so this is denied
/// exactly like a real grant would be on the wrong route.
const GRANT: &str = "bgp_fedcba9876543210fedcba9876543210fedcba9876543210fedcba9876543210";

fn setup() -> (tempfile::TempDir, Router) {
    let dir = tempfile::tempdir().unwrap();
    let workspace = dir.path().join("workspace");
    std::fs::create_dir(&workspace).unwrap();
    let store = Store::open(&dir.path().join("state"), &workspace).unwrap();
    let state = http::new(
        store,
        IndexOptions::new(workspace),
        OWNER.into(),
        "127.0.0.1:7331".parse().unwrap(),
    )
    .unwrap();
    (dir, http::router(state))
}

async fn call(app: &Router, method: &str, path: &str, token: Option<&str>) -> (StatusCode, Value) {
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
        .oneshot(req.body(Body::from(json!({}).to_string())).unwrap())
        .await
        .unwrap();
    let code = response.status();
    let bytes = to_bytes(response.into_body(), 4 * 1024 * 1024)
        .await
        .unwrap();
    (code, serde_json::from_slice(&bytes).unwrap_or(Value::Null))
}

/// Every owner route the daemon exposes, by method and path.
const OWNER_ROUTES: &[(&str, &str)] = &[
    ("GET", "/api/status"),
    ("GET", "/api/jev/status"),
    ("GET", "/api/acp/status"),
    ("POST", "/api/index"),
    ("GET", "/api/jobs/current"),
    ("GET", "/api/jobs/x"),
    ("POST", "/api/jobs/x/cancel"),
    ("GET", "/api/dependencies"),
    ("POST", "/api/dependencies/refresh"),
    ("GET", "/api/dependencies/symbols"),
    ("GET", "/api/dependencies/source"),
    ("GET", "/api/rust-sources"),
    ("GET", "/api/rust-sources/tree"),
    ("GET", "/api/rust-sources/file"),
    ("GET", "/api/tree"),
    ("GET", "/api/files"),
    ("GET", "/api/methods"),
    ("POST", "/api/sequence"),
    ("GET", "/api/classes"),
    ("POST", "/api/class-diagram"),
    ("POST", "/api/navigation"),
    ("GET", "/api/symbols"),
    ("GET", "/api/symbol"),
    ("GET", "/api/source"),
    ("POST", "/api/query"),
    ("POST", "/api/questions/preview"),
    ("GET", "/api/questions/p/jev-request"),
    ("POST", "/api/questions/p/jev-response"),
    ("POST", "/api/questions/p/selection"),
    ("POST", "/api/questions/p/acp-answer"),
    ("POST", "/api/questions/p/jev-run"),
    ("GET", "/api/views"),
    ("GET", "/api/views/v"),
    ("PUT", "/api/views/v"),
    ("DELETE", "/api/views/v"),
    ("GET", "/api/annotations"),
    ("PUT", "/api/annotations/a"),
    ("DELETE", "/api/annotations/a"),
    // Grant management is owner-only too: a grant can never mint or revoke another.
    ("GET", "/api/mcp-pilot/binding"),
    ("POST", "/api/mcp-pilot/grants"),
    ("DELETE", "/api/mcp-pilot/grants/g"),
];

#[tokio::test]
async fn a_grant_is_denied_on_every_owner_route() {
    let (_d, app) = setup();
    for (method, path) in OWNER_ROUTES {
        let (code, _) = call(&app, method, path, Some(GRANT)).await;
        assert_eq!(
            code,
            StatusCode::FORBIDDEN,
            "a grant reached {method} {path}"
        );
    }
}

#[tokio::test]
async fn the_owner_bearer_is_denied_on_tool_routes() {
    let (_d, app) = setup();
    for tool in [
        "baleyg_workspace_describe",
        "baleyg_find_symbols",
        "baleyg_inspect",
        "baleyg_read_source",
        "baleyg_anything_else",
    ] {
        let (code, body) = call(
            &app,
            "POST",
            &format!("/api/mcp-pilot/tools/{tool}"),
            Some(OWNER),
        )
        .await;
        assert_eq!(code, StatusCode::FORBIDDEN, "owner reached {tool}");
        assert_eq!(body["error"]["code"], "forbidden");
    }
}

#[tokio::test]
async fn unknown_tools_are_denied_in_the_contract_envelope() {
    let (_d, app) = setup();
    // GRANT is well formed but was never issued. The guard checks only shape, so the handler
    // admits it through the grant table: an unusable credential is refused uniformly rather than
    // learning which tools do not exist.
    for tool in [
        "baleyg_run_shell",
        "baleyg_workspace_describe",
        "baleyg_nope",
    ] {
        let (code, body) = call(
            &app,
            "POST",
            &format!("/api/mcp-pilot/tools/{tool}"),
            Some(GRANT),
        )
        .await;
        assert_eq!(code, StatusCode::UNAUTHORIZED, "{tool}");
        assert_eq!(body["error"]["code"], "unauthorized");
        assert_eq!(body["schemaVersion"], 1);
        assert!(body["requestId"].is_string());
    }
}

#[tokio::test]
async fn malformed_credentials_are_unauthorized_not_forbidden() {
    let (_d, app) = setup();
    for token in [
        None,
        Some("not-a-token"),
        // Right prefix, wrong shape: still not a credential the guard will carry.
        Some("bgp_short"),
        Some("bgp_ZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZ"),
        // The owner token with one character changed.
        Some("0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdee"),
    ] {
        let (code, _) = call(&app, "GET", "/api/status", token).await;
        assert_eq!(code, StatusCode::UNAUTHORIZED, "{token:?}");
        let (code, _) = call(
            &app,
            "POST",
            "/api/mcp-pilot/tools/baleyg_workspace_describe",
            token,
        )
        .await;
        assert_eq!(code, StatusCode::UNAUTHORIZED, "{token:?}");
    }
}

#[tokio::test]
async fn the_owner_still_reaches_its_own_routes() {
    let (_d, app) = setup();
    let (code, _) = call(&app, "GET", "/api/status", Some(OWNER)).await;
    assert_eq!(code, StatusCode::OK);
    let (code, _) = call(&app, "GET", "/api/mcp-pilot/binding", Some(OWNER)).await;
    assert_eq!(code, StatusCode::OK);
}

#[tokio::test]
async fn duplicate_authorization_headers_are_refused() {
    let (_d, app) = setup();
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/status")
                .header("host", "127.0.0.1:7331")
                .header("authorization", format!("Bearer {OWNER}"))
                .header("authorization", format!("Bearer {GRANT}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn every_shape_under_the_tool_prefix_is_admitted_before_it_is_answered() {
    let (_d, app) = setup();
    // Wrong method, missing tail, trailing slash and a nested tail all used to be answered by the
    // router with 404 or 405 before the credential was ever examined.
    for (method, path) in [
        ("GET", "/api/mcp-pilot/tools/baleyg_workspace_describe"),
        ("DELETE", "/api/mcp-pilot/tools/baleyg_workspace_describe"),
        ("POST", "/api/mcp-pilot/tools"),
        ("POST", "/api/mcp-pilot/tools/"),
        ("POST", "/api/mcp-pilot/tools/a/b/c"),
        ("PUT", "/api/mcp-pilot/tools/anything"),
    ] {
        let (code, body) = call(&app, method, path, Some(GRANT)).await;
        assert_eq!(
            code,
            StatusCode::UNAUTHORIZED,
            "{method} {path} answered before admission"
        );
        assert_eq!(body["error"]["code"], "unauthorized");
        assert_eq!(body["schemaVersion"], 1);
    }
}

#[tokio::test]
async fn the_owner_is_still_denied_across_the_whole_tool_prefix() {
    let (_d, app) = setup();
    for (method, path) in [
        ("GET", "/api/mcp-pilot/tools/baleyg_workspace_describe"),
        ("POST", "/api/mcp-pilot/tools"),
        ("POST", "/api/mcp-pilot/tools/a/b/c"),
    ] {
        let (code, _) = call(&app, method, path, Some(OWNER)).await;
        assert_eq!(code, StatusCode::FORBIDDEN, "{method} {path}");
    }
}

#[tokio::test]
async fn an_oversized_tool_body_is_admitted_and_charged_not_refused_first() {
    let (_d, app) = setup();
    // Far beyond the daemon-wide buffer that used to refuse this before the grant was identified.
    let huge = "x".repeat(2 * 1024 * 1024);
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/mcp-pilot/tools/baleyg_workspace_describe")
                .header("host", "127.0.0.1:7331")
                .header("authorization", format!("Bearer {GRANT}"))
                .header("content-type", "application/json")
                .body(Body::from(huge))
                .unwrap(),
        )
        .await
        .unwrap();
    // Unusable credential, so the answer is the uniform 401 in the contract envelope rather than
    // a legacy 413 emitted before admission.
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    let bytes = axum::body::to_bytes(response.into_body(), 64 * 1024)
        .await
        .unwrap();
    let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(body["schemaVersion"], 1);
    assert_eq!(body["error"]["code"], "unauthorized");
}
