//! Slice 5: the three evidence tools.
//!
//! Covers acceptance tests 1, 6 and 7 for find_symbols, inspect and read_source.
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
const FIND: &str = "/api/mcp-pilot/tools/baleyg_find_symbols";
const INSPECT: &str = "/api/mcp-pilot/tools/baleyg_inspect";
const READ: &str = "/api/mcp-pilot/tools/baleyg_read_source";
/// 300 lines, so line and byte clipping are both reachable.
const LINES: usize = 300;

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
    let bytes = to_bytes(response.into_body(), 8 * 1024 * 1024)
        .await
        .unwrap();
    (code, serde_json::from_slice(&bytes).unwrap_or(Value::Null))
}

/// The store validates that line/column are derived from the byte offsets, so ranges are computed
/// from the real cached text rather than invented.
fn text() -> String {
    (1..=LINES)
        .map(|n| format!("line {n} of the cached file\n"))
        .collect()
}

fn line_starts(text: &str) -> Vec<usize> {
    std::iter::once(0)
        .chain(text.match_indices('\n').map(|(i, _)| i + 1))
        .collect()
}

fn range(line: usize) -> SourceRange {
    let body = text();
    let start = line_starts(&body)[line - 1];
    SourceRange {
        start_byte: start,
        end_byte: start + 5,
        start_line: line,
        start_column: 1,
        end_line: line,
        end_column: 6,
    }
}

fn provenance() -> Provenance {
    Provenance {
        source: "syntax".into(),
        semantic: SemanticState::Unavailable,
    }
}

fn symbol(id: &str, line: usize) -> Symbol {
    Symbol {
        id: id.into(),
        name: id.into(),
        kind: SymbolKind::Function,
        path: "a.js".into(),
        range: range(line),
        parent: None,
        accessor: false,
        provenance: provenance(),
    }
}

/// One caller with 51 outgoing calls, one carrying an oversized literal fragment, plus a control
/// region and a callback argument that must not reach the tool surface.
fn graph() -> Graph {
    let mut calls = vec![CallSite {
        id: "call:long".into(),
        caller: "caller".into(),
        callee_text: "x".repeat(4096),
        path: "a.js".into(),
        range: range(1),
        target: Some("callee".into()),
        candidate_symbols: vec!["callee".into()],
        resolution: Resolution::Internal,
        ordinal: 0,
        regions: vec!["region:1".into()],
        callback_arguments: vec!["callee".into()],
        provenance: provenance(),
    }];
    for n in 0..51 {
        calls.push(CallSite {
            id: format!("call:{n:03}"),
            caller: "caller".into(),
            callee_text: format!("helper{n}()"),
            path: "a.js".into(),
            range: range(n + 4),
            target: None,
            candidate_symbols: vec![],
            resolution: Resolution::Unresolved,
            ordinal: n + 1,
            regions: vec![],
            callback_arguments: vec![],
            provenance: provenance(),
        });
    }
    Graph {
        files: vec![SourceFile {
            path: "a.js".into(),
            hash: "cachedhash".into(),
            language: "javascript".into(),
            text: text(),
        }],
        nodes: vec![symbol("caller", 1), symbol("callee", 2), symbol("other", 3)],
        calls,
        regions: vec![ControlRegion {
            id: "region:1".into(),
            kind: "if".into(),
            label: "secretCondition === true".into(),
            parent: None,
            owner: "caller".into(),
            path: "a.js".into(),
            range: range(1),
        }],
        ..Graph::default()
    }
}

fn publish(store: &Store, expected: Option<u64>) {
    store
        .publish(
            &graph(),
            expected,
            &(Arc::new(AtomicBool::new(false)) as CancelFlag),
        )
        .unwrap();
}

async fn grant(
    app: &Router,
    revision: u64,
    capabilities: Value,
    source_approved: bool,
) -> (String, Value) {
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
            "capabilities": capabilities,
            "ttlSeconds": 900,
            "limits": {"maxRequests": 200, "maxTotalResponseBytes": 2097152, "maxResponseBytes": 65536},
            "clientLabel": "terminal-pilot",
            "disclosure": {"recipient": "approved local client", "sourceApproved": source_approved},
        }),
    )
    .await;
    assert_eq!(code, StatusCode::CREATED, "{issued}");
    (issued["token"].as_str().unwrap().to_string(), binding)
}

const ALL: [&str; 4] = [
    "baleyg_workspace_describe",
    "baleyg_find_symbols",
    "baleyg_inspect",
    "baleyg_read_source",
];

async fn ready(app: &Router, store: &Store) -> (String, Value) {
    publish(store, Some(0));
    grant(app, 1, json!(ALL), true).await
}

#[tokio::test]
async fn find_symbols_projects_declarations_without_source() {
    let (_d, store, app) = setup();
    let (token, binding) = ready(&app, &store).await;
    let (code, body) = call(
        &app,
        "POST",
        FIND,
        &token,
        json!({"schemaVersion": 1, "binding": binding, "expectedRevision": 1, "query": "call"}),
    )
    .await;
    assert_eq!(code, StatusCode::OK, "{body}");
    let symbols = body["data"]["symbols"].as_array().unwrap();
    assert_eq!(symbols.len(), 2);
    // Deterministic ordering: exact name, then name prefix, then substring; ties by name and id.
    // Both are prefix matches here, so they order by name.
    assert_eq!(symbols[0]["symbolId"], "callee");
    assert_eq!(symbols[1]["symbolId"], "caller");
    assert_eq!(symbols[0]["path"], "a.js");
    assert_eq!(symbols[0]["evidence"]["semantic"], "unavailable");
    assert!(symbols[0]["range"]["startLine"].is_number());
    assert!(
        !body.to_string().contains("cached file"),
        "leaked source text"
    );
    assert_eq!(body["evidenceBasis"]["indexRevision"], 1);
    assert_eq!(body["truncated"], false);
}

#[tokio::test]
async fn find_symbols_reports_a_clipped_page_rather_than_claiming_completeness() {
    let (_d, store, app) = setup();
    let (token, binding) = ready(&app, &store).await;
    let (code, body) = call(
        &app,
        "POST",
        FIND,
        &token,
        json!({"schemaVersion": 1, "binding": binding, "expectedRevision": 1, "query": "e", "limit": 1}),
    )
    .await;
    assert_eq!(code, StatusCode::OK);
    assert_eq!(body["data"]["symbols"].as_array().unwrap().len(), 1);
    assert_eq!(body["truncated"], true);
    assert_eq!(body["truncationReason"], "result_limit");
}

#[tokio::test]
async fn find_symbols_validates_its_inputs() {
    let (_d, store, app) = setup();
    let (token, binding) = ready(&app, &store).await;
    let base = json!({"schemaVersion": 1, "binding": binding, "expectedRevision": 1, "query": "a"});
    for mutate in [
        json!({"query": ""}),
        json!({"query": "x".repeat(257)}),
        json!({"limit": 0}),
        json!({"limit": 51}),
        json!({"expectedRevision": 0}),
        json!({"surprise": true}),
    ] {
        let mut body = base.clone();
        for (k, v) in mutate.as_object().unwrap() {
            body[k] = v.clone();
        }
        let (code, _) = call(&app, "POST", FIND, &token, body).await;
        assert_eq!(code, StatusCode::BAD_REQUEST, "{mutate}");
    }
}

#[tokio::test]
async fn inspect_returns_depth_one_calls_and_excludes_control_evidence() {
    let (_d, store, app) = setup();
    let (token, binding) = ready(&app, &store).await;
    let (code, body) = call(
        &app,
        "POST",
        INSPECT,
        &token,
        json!({"schemaVersion": 1, "binding": binding, "expectedRevision": 1, "symbolId": "caller", "view": "outgoing_calls"}),
    )
    .await;
    assert_eq!(code, StatusCode::OK, "{body}");
    let calls = body["data"]["calls"].as_array().unwrap();
    assert_eq!(calls.len(), 50, "depth-one calls are capped at 50");
    assert_eq!(body["truncated"], true);
    assert_eq!(body["truncationReason"], "result_limit");

    // Original identities, ranges and resolution labels survive the projection.
    let first = &calls[0];
    assert_eq!(first["callId"], "call:long");
    assert_eq!(first["callerSymbolId"], "caller");
    assert_eq!(first["resolution"], "internal");
    assert_eq!(first["candidateCount"], 1);
    assert!(first["range"]["startByte"].is_number());

    // An unresolved call stays unresolved and is never promoted to a binding.
    let unresolved = calls.iter().find(|c| c["callId"] == "call:000").unwrap();
    assert_eq!(unresolved["resolution"], "unresolved");
    assert!(unresolved["targetSymbolId"].is_null());

    let rendered = body.to_string();
    assert!(
        !rendered.contains("secretCondition"),
        "leaked a control-region label"
    );
    assert!(
        !rendered.contains("regions"),
        "leaked control-region evidence"
    );
    assert!(
        !rendered.contains("callbackArguments"),
        "leaked callback arguments"
    );
}

#[tokio::test]
async fn inspect_clips_an_oversized_literal_fragment_at_a_code_point_boundary() {
    let (_d, store, app) = setup();
    let (token, binding) = ready(&app, &store).await;
    let (_, body) = call(
        &app,
        "POST",
        INSPECT,
        &token,
        json!({"schemaVersion": 1, "binding": binding, "expectedRevision": 1, "symbolId": "caller", "view": "outgoing_calls"}),
    )
    .await;
    let long = &body["data"]["calls"][0];
    assert_eq!(long["calleeTextTruncated"], true);
    assert_eq!(long["calleeText"].as_str().unwrap().len(), 1024);
}

#[tokio::test]
async fn inspect_declaration_returns_no_calls() {
    let (_d, store, app) = setup();
    let (token, binding) = ready(&app, &store).await;
    let (code, body) = call(
        &app,
        "POST",
        INSPECT,
        &token,
        json!({"schemaVersion": 1, "binding": binding, "expectedRevision": 1, "symbolId": "callee", "view": "declaration"}),
    )
    .await;
    assert_eq!(code, StatusCode::OK);
    assert_eq!(body["data"]["view"], "declaration");
    assert_eq!(body["data"]["declaration"]["symbolId"], "callee");
    assert!(body["data"]["calls"].is_null());
}

#[tokio::test]
async fn inspect_rejects_unknown_symbols_and_views() {
    let (_d, store, app) = setup();
    let (token, binding) = ready(&app, &store).await;
    let (code, body) = call(
        &app,
        "POST",
        INSPECT,
        &token,
        json!({"schemaVersion": 1, "binding": binding, "expectedRevision": 1, "symbolId": "nope", "view": "declaration"}),
    )
    .await;
    assert_eq!(code, StatusCode::NOT_FOUND);
    assert_eq!(body["error"]["code"], "not_found");

    let (code, _) = call(
        &app,
        "POST",
        INSPECT,
        &token,
        json!({"schemaVersion": 1, "binding": binding, "expectedRevision": 1, "symbolId": "caller", "view": "callers"}),
    )
    .await;
    assert_eq!(code, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn read_source_returns_exact_cached_bytes_with_its_hash() {
    let (_d, store, app) = setup();
    let (token, binding) = ready(&app, &store).await;
    let (code, body) = call(
        &app,
        "POST",
        READ,
        &token,
        json!({"schemaVersion": 1, "binding": binding, "expectedRevision": 1, "path": "a.js", "startLine": 2, "endLine": 3}),
    )
    .await;
    assert_eq!(code, StatusCode::OK, "{body}");
    assert_eq!(body["data"]["fileHash"], "cachedhash");
    assert_eq!(body["data"]["startLine"], 2);
    assert_eq!(body["data"]["endLine"], 3);
    assert_eq!(
        body["data"]["text"],
        "line 2 of the cached file\nline 3 of the cached file\n"
    );
    let start = body["data"]["startByte"].as_u64().unwrap();
    let end = body["data"]["endByte"].as_u64().unwrap();
    assert_eq!(
        end - start,
        body["data"]["text"].as_str().unwrap().len() as u64
    );
    assert_eq!(body["truncated"], false);
}

#[tokio::test]
async fn read_source_clips_at_the_line_ceiling_and_reports_the_actual_range() {
    let (_d, store, app) = setup();
    let (token, binding) = ready(&app, &store).await;
    let (code, body) = call(
        &app,
        "POST",
        READ,
        &token,
        json!({"schemaVersion": 1, "binding": binding, "expectedRevision": 1, "path": "a.js", "startLine": 1, "endLine": LINES}),
    )
    .await;
    assert_eq!(code, StatusCode::OK);
    assert_eq!(body["data"]["endLine"], 200, "capped at 200 lines");
    assert_eq!(body["truncated"], true);
    assert_eq!(body["truncationReason"], "line_limit");
    // A clipped read never ends mid-line.
    assert!(body["data"]["text"].as_str().unwrap().ends_with('\n'));
}

#[tokio::test]
async fn read_source_validates_paths_and_never_falls_back_to_disk() {
    let (_d, store, app) = setup();
    let (token, binding) = ready(&app, &store).await;
    for path in [
        "/etc/passwd",
        "../outside.js",
        "a/../../b.js",
        "c:\\windows\\file",
        "",
        "./a.js",
    ] {
        let (code, _) = call(
            &app,
            "POST",
            READ,
            &token,
            json!({"schemaVersion": 1, "binding": binding, "expectedRevision": 1, "path": path, "startLine": 1, "endLine": 1}),
        )
        .await;
        assert_eq!(code, StatusCode::BAD_REQUEST, "{path}");
    }
    // A well-formed path that is simply not in the snapshot is not found, never read from disk.
    let (code, body) = call(
        &app,
        "POST",
        READ,
        &token,
        json!({"schemaVersion": 1, "binding": binding, "expectedRevision": 1, "path": "absent.js", "startLine": 1, "endLine": 1}),
    )
    .await;
    assert_eq!(code, StatusCode::NOT_FOUND);
    assert_eq!(body["error"]["code"], "not_found");
}

#[tokio::test]
async fn evidence_reads_require_source_approval() {
    let (_d, store, app) = setup();
    publish(&store, Some(0));
    let (token, binding) = grant(
        &app,
        1,
        json!(["baleyg_workspace_describe", "baleyg_find_symbols"]),
        false,
    )
    .await;
    // The structural tool works under metadata approval alone.
    let (code, _) = call(
        &app,
        "POST",
        FIND,
        &token,
        json!({"schemaVersion": 1, "binding": binding, "expectedRevision": 1, "query": "call"}),
    )
    .await;
    assert_eq!(code, StatusCode::OK);

    for (route, extra) in [
        (
            INSPECT,
            json!({"symbolId": "caller", "view": "declaration"}),
        ),
        (READ, json!({"path": "a.js", "startLine": 1, "endLine": 1})),
    ] {
        let mut body = json!({"schemaVersion": 1, "binding": binding, "expectedRevision": 1});
        for (k, v) in extra.as_object().unwrap() {
            body[k] = v.clone();
        }
        let (code, body) = call(&app, "POST", route, &token, body).await;
        assert_eq!(code, StatusCode::FORBIDDEN, "{route}");
        assert_eq!(body["error"]["code"], "forbidden");
    }
}

#[tokio::test]
async fn a_revision_published_after_issuance_conflicts_on_every_evidence_read() {
    let (_d, store, app) = setup();
    let (token, binding) = ready(&app, &store).await;
    publish(&store, Some(1));

    for (route, extra) in [
        (FIND, json!({"query": "call"})),
        (
            INSPECT,
            json!({"symbolId": "caller", "view": "declaration"}),
        ),
        (READ, json!({"path": "a.js", "startLine": 1, "endLine": 1})),
    ] {
        // The request still claims the admitted revision, which no longer matches the snapshot.
        let mut body = json!({"schemaVersion": 1, "binding": binding, "expectedRevision": 1});
        for (k, v) in extra.as_object().unwrap() {
            body[k] = v.clone();
        }
        let (code, body) = call(&app, "POST", route, &token, body).await;
        assert_eq!(code, StatusCode::CONFLICT, "{route}");
        assert_eq!(body["error"]["code"], "revision_conflict");
    }
}

#[tokio::test]
async fn read_source_rejects_an_end_line_beyond_the_cached_file() {
    let (_d, store, app) = setup();
    let (token, binding) = ready(&app, &store).await;

    // Clamping the end silently would return line 300 only and report it as a complete range.
    let (code, body) = call(
        &app,
        "POST",
        READ,
        &token,
        json!({"schemaVersion": 1, "binding": binding, "expectedRevision": 1, "path": "a.js", "startLine": LINES, "endLine": LINES + 1}),
    )
    .await;
    assert_eq!(code, StatusCode::BAD_REQUEST);
    assert_eq!(body["error"]["code"], "invalid_request");

    // The last line on its own is still readable.
    let (code, body) = call(
        &app,
        "POST",
        READ,
        &token,
        json!({"schemaVersion": 1, "binding": binding, "expectedRevision": 1, "path": "a.js", "startLine": LINES, "endLine": LINES}),
    )
    .await;
    assert_eq!(code, StatusCode::OK);
    assert_eq!(body["data"]["endLine"], LINES);
    assert_eq!(body["truncated"], false);
}

#[tokio::test]
async fn authenticated_errors_on_evidence_tools_are_charged() {
    let (_d, store, app) = setup();
    publish(&store, Some(0));
    let (_, discovery) = call(&app, "GET", "/api/mcp-pilot/binding", OWNER, Value::Null).await;
    let binding = discovery["binding"].clone();
    let (code, issued) = call(
        &app,
        "POST",
        "/api/mcp-pilot/grants",
        OWNER,
        json!({
            "schemaVersion": 1,
            "binding": binding,
            "expectedRevision": 1,
            "capabilities": ALL,
            "ttlSeconds": 900,
            "limits": {"maxRequests": 50, "maxTotalResponseBytes": 1000, "maxResponseBytes": 65536},
            "clientLabel": "terminal-pilot",
            "disclosure": {"recipient": "approved local client", "sourceApproved": true},
        }),
    )
    .await;
    assert_eq!(code, StatusCode::CREATED);
    let token = issued["token"].as_str().unwrap();

    let mut malformed =
        json!({"schemaVersion": 1, "binding": binding, "expectedRevision": 1, "query": "a"});
    malformed["surprise"] = json!(true);
    let mut exhausted = false;
    for _ in 0..12 {
        let (code, _) = call(&app, "POST", FIND, token, malformed.clone()).await;
        if code == StatusCode::TOO_MANY_REQUESTS {
            exhausted = true;
            break;
        }
        assert_eq!(code, StatusCode::BAD_REQUEST);
    }
    assert!(
        exhausted,
        "evidence-tool errors were not charged to the budget"
    );
}

#[tokio::test]
async fn a_forbidden_capability_call_is_charged() {
    let (_d, store, app) = setup();
    publish(&store, Some(0));
    let limits =
        json!({"maxRequests": 1, "maxTotalResponseBytes": 2097152, "maxResponseBytes": 65536});
    let (_, discovery) = call(&app, "GET", "/api/mcp-pilot/binding", OWNER, Value::Null).await;
    let binding = discovery["binding"].clone();
    let (code, issued) = call(
        &app,
        "POST",
        "/api/mcp-pilot/grants",
        OWNER,
        json!({
            "schemaVersion": 1,
            "binding": binding,
            "expectedRevision": 1,
            "capabilities": ["baleyg_workspace_describe"],
            "ttlSeconds": 900,
            "limits": limits,
            "clientLabel": "terminal-pilot",
            "disclosure": {"recipient": "approved local client", "sourceApproved": false},
        }),
    )
    .await;
    assert_eq!(code, StatusCode::CREATED, "{issued}");
    let token = issued["token"].as_str().unwrap();

    // Naming a capability the grant does not carry is authenticated error traffic, so it spends
    // the budget rather than being free.
    let (code, _) = call(
        &app,
        "POST",
        "/api/mcp-pilot/tools/baleyg_inspect",
        token,
        json!({"schemaVersion": 1, "binding": binding, "expectedRevision": 1, "symbolId": "a", "view": "declaration"}),
    )
    .await;
    assert_eq!(code, StatusCode::FORBIDDEN);

    let (code, body) = call(
        &app,
        "POST",
        "/api/mcp-pilot/tools/baleyg_workspace_describe",
        token,
        json!({"schemaVersion": 1, "binding": binding}),
    )
    .await;
    assert_eq!(code, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(body["error"]["code"], "budget_exhausted");
}
