//! Owner control routes for the read-only pilot.
//!
//! These are owner-bearer routes: they issue, describe and revoke grants but never serve evidence.
//! Every store read goes through the enrolled connection — never `Store::status()`, which opens by
//! path and whose helper can recreate a deleted cache, turning identity loss into a friendly
//! `no_published_index`.

use super::{
    ErrorCode, McpError,
    grants::{Binding, Capability, IssueRequest, Limits},
};
use crate::http::DaemonState;
use axum::{
    Json, Router,
    extract::{Path, State},
    http::{StatusCode, header},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::BTreeSet,
    sync::Arc,
    time::{Duration, Instant},
};

/// Backend operation deadline, also applied to owner reads so a wedged store cannot hold a route.
const OPERATION_DEADLINE: Duration = Duration::from_secs(5);

fn deadline() -> Instant {
    Instant::now() + OPERATION_DEADLINE
}

fn timed_out() -> McpError {
    McpError::new(
        ErrorCode::DeadlineExceeded,
        "The operation deadline elapsed",
    )
}

/// Run blocking store work under one deadline computed at request admission.
///
/// The deadline is decided before the work is queued, so time spent waiting for a blocking worker
/// counts against it. Computed inside the worker instead, a saturated pool could delay a response
/// past its deadline and still return success.
async fn blocking_within<T: Send + 'static>(
    deadline: Instant,
    f: impl FnOnce() -> Result<T, McpError> + Send + 'static,
) -> Result<T, McpError> {
    let remaining = deadline.saturating_duration_since(Instant::now());
    if remaining.is_zero() {
        return Err(timed_out());
    }
    match tokio::time::timeout(remaining, tokio::task::spawn_blocking(f)).await {
        Ok(Ok(result)) => result,
        Ok(Err(_)) => Err(unavailable()),
        Err(_) => Err(timed_out()),
    }
}

/// Contract error envelope as a value, so its size can be charged before it is sent.
fn error_body(e: &McpError) -> Value {
    json!({
        "schemaVersion": 1,
        "error": {"code": e.code.as_str(), "message": e.message, "retryable": e.code.retryable()},
        "requestId": uuid::Uuid::new_v4().to_string(),
    })
}

fn respond(status: u16, body: Value) -> Response {
    let status = StatusCode::from_u16(status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
    let mut response = (status, Json(body)).into_response();
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, "no-store".parse().unwrap());
    response
}

/// Failure on a request that could not be charged to a grant. Never carries SQL, token fragments,
/// absolute state paths or source.
pub(crate) fn fail(e: McpError) -> Response {
    respond(e.code.status(), error_body(&e))
}

/// Failure on an identified request. Ordinary authenticated errors are charged to the grant that
/// sent them: otherwise malformed traffic bypasses both lifetime ceilings entirely. Only the
/// unidentifiable case above goes uncharged, which is the contract's fixed error allowance.
fn charged(admission: crate::mcp::grants::Admission<'_>, e: McpError) -> Response {
    let status = e.code.status();
    let body = error_body(&e);
    let size = body.to_string().len() as u64;
    // No error is exempt from the ceiling. A deadline used to be, so that it would not be reported
    // as a budget denial, but the exemption let a response exceed a limit the owner set. Grants now
    // carry a floor on that ceiling, so every envelope fits and neither compromise is needed.
    if size > admission.max_response_bytes {
        // Emit a smaller envelope rather than sending the full one and clamping the charge
        // afterwards. If even this exceeds what remains, settlement clamps and the overshoot is the
        // contract's fixed, bounded error allowance.
        let denial = McpError::new(
            ErrorCode::BudgetExhausted,
            "The grant response budget is exhausted",
        );
        let status = denial.code.status();
        let body = error_body(&denial);
        admission.settle(body.to_string().len() as u64);
        return respond(status, body);
    }
    admission.settle(size);
    respond(status, body)
}

/// Tool routes are the only ones a limited grant may reach, and the only ones the owner bearer
/// may not. The two principals are disjoint by route, enforced here rather than by a tool
/// allowlist in the adapter.
pub(crate) fn is_tool_route(path: &str) -> bool {
    // The bare prefix counts too: otherwise a request to it is classified as an owner route and
    // answered by the guard before the tool handler can identify and charge it.
    path == "/api/mcp-pilot/tools" || path.starts_with("/api/mcp-pilot/tools/")
}

/// Any `/api/mcp-pilot/...` path answers in the contract envelope, including guard rejections.
pub(crate) fn is_pilot_route(path: &str) -> bool {
    path == "/api/mcp-pilot" || path.starts_with("/api/mcp-pilot/")
}

/// A well-formed grant credential. Shape only: the guard never consults the grant table, so a
/// rejection on an owner route cannot reveal whether a grant exists.
pub(crate) fn looks_like_grant(token: &str) -> bool {
    token
        .strip_prefix(super::grants::TOKEN_PREFIX)
        .is_some_and(|rest| {
            rest.len() == 64
                && rest
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        })
}

fn unavailable() -> McpError {
    McpError::new(
        ErrorCode::StoreUnavailable,
        "The bound store is unavailable",
    )
}

#[derive(Deserialize, Serialize, Clone)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct BindingBody {
    daemon_instance_id: String,
    store_generation: String,
}

#[derive(Deserialize, Serialize, Clone, Copy)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct LimitsBody {
    max_requests: u64,
    max_total_response_bytes: u64,
    max_response_bytes: u64,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct DisclosureBody {
    recipient: String,
    source_approved: bool,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct IssueBody {
    schema_version: u32,
    binding: BindingBody,
    expected_revision: u64,
    capabilities: Vec<String>,
    ttl_seconds: u64,
    limits: LimitsBody,
    client_label: String,
    disclosure: DisclosureBody,
}

/// Owner-only binding discovery. Reports revision 0 for an unindexed store so the owner helper can
/// explain the prerequisite before attempting issuance.
async fn binding(State(s): State<Arc<DaemonState>>) -> Response {
    let Some(enrollment) = s.enrollment.clone() else {
        return fail(unavailable());
    };
    if !enrollment.is_available() {
        return fail(unavailable());
    }
    let store_paths = (
        s.store_state_dir().to_string_lossy().into_owned(),
        s.store_workspace_root().to_string(),
    );
    let at = deadline();
    let gate = enrollment.clone();
    let result = blocking_within(at, move || {
        enrollment.admit_current(at, |revision| (enrollment.basis(revision), revision))
    })
    .await;
    // Checked after the awaited phase and before any result is matched, so success and failure
    // alike are refused once the operation's deadline has passed. A second confirmation is
    // deliberately not added: it would run in its own blocking phase and reopen exactly the
    // worker-to-handler window it was meant to close.
    if Instant::now() >= at {
        return fail(timed_out());
    }
    match result {
        Ok(((basis, revision), ticket)) => {
            // Committed under the same lock invalidation takes, so the response is either refused
            // or handed off with nothing able to interleave between the two.
            if let Err(e) = ticket.commit(&gate) {
                return fail(e);
            }
            Json(json!({
                "schemaVersion": 1,
                "binding": {
                    "daemonInstanceId": basis.daemon_instance_id,
                    "storeGeneration": basis.store_generation,
                },
                "stateDir": store_paths.0,
                "workspaceRoot": store_paths.1,
                "indexRevision": revision,
                "evidenceReadable": revision > 0,
            }))
            .into_response()
        }
        Err(e) => fail(e),
    }
}

/// Issue one pinned, short-lived grant.
///
/// Order is deliberate: request shape, then store binding, then a published index, and only then
/// the expected-revision comparison. A destroyed cache must surface as an unavailable store rather
/// than as an empty one.
async fn issue(State(s): State<Arc<DaemonState>>, body: String) -> Response {
    let body: IssueBody = match serde_json::from_str(&body) {
        Ok(body) => body,
        Err(_) => {
            return fail(McpError::new(
                ErrorCode::InvalidRequest,
                "The request is malformed or carries unknown fields",
            ));
        }
    };
    if body.schema_version != 1 {
        return fail(McpError::new(
            ErrorCode::InvalidRequest,
            "Unsupported schema version",
        ));
    }
    let mut capabilities = BTreeSet::new();
    for name in &body.capabilities {
        match Capability::parse(name) {
            Some(capability) => {
                capabilities.insert(capability);
            }
            None => {
                return fail(McpError::new(
                    ErrorCode::InvalidRequest,
                    "Unknown capability requested",
                ));
            }
        }
    }
    let request = IssueRequest {
        binding: Binding {
            daemon_instance_id: body.binding.daemon_instance_id.clone(),
            store_generation: body.binding.store_generation.clone(),
        },
        expected_revision: body.expected_revision,
        capabilities,
        ttl_seconds: body.ttl_seconds,
        limits: Limits {
            max_requests: body.limits.max_requests,
            max_total_response_bytes: body.limits.max_total_response_bytes,
            max_response_bytes: body.limits.max_response_bytes,
        },
        client_label: body.client_label,
        recipient: body.disclosure.recipient,
        source_approved: body.disclosure.source_approved,
    };
    if let Err(e) = request.validate() {
        return fail(e);
    }

    let Some(enrollment) = s.enrollment.clone() else {
        return fail(unavailable());
    };
    // A latched binding is an unavailable store, not a caller mistake. Checked before the binding
    // comparison so invalidation does not masquerade as a stale descriptor.
    if !enrollment.is_available() {
        return fail(unavailable());
    }
    if enrollment.daemon_instance_id() != request.binding.daemon_instance_id
        || enrollment.store_generation() != request.binding.store_generation
    {
        return fail(McpError::new(
            ErrorCode::BindingMismatch,
            "The request does not match this daemon's binding",
        ));
    }

    let at = deadline();
    let gate = enrollment.clone();
    let expected = request.expected_revision;
    let state = s.clone();
    // Read the revision and create the credential inside one admission boundary. Issuing outside
    // it would let an invalidation or a publication land in the gap and still mint a token.
    let issued = blocking_within(at, move || {
        let revision = enrollment.current_revision(at)?;
        if revision == 0 {
            return Err(McpError::new(
                ErrorCode::NoPublishedIndex,
                "The bound store has no published index; publish one before issuing a grant",
            ));
        }
        if revision != expected {
            return Err(McpError::new(
                ErrorCode::RevisionConflict,
                "The index revision changed",
            ));
        }
        // A timed-out blocking task keeps running: without this the operation could mint a live
        // grant nobody can reach, after the request it belonged to had already failed.
        if Instant::now() >= at {
            return Err(timed_out());
        }
        // The producer has an effect, so it reports the grant it created and the undo removes it
        // when verification after production fails. Discarding the value on the error path instead
        // leaves an unreachable row in the ledger.
        let cleanup = state.clone();
        let (issued, ticket) = enrollment.admit_with(
            at,
            |current| {
                if current != revision {
                    return (
                        Err(McpError::new(
                            ErrorCode::RevisionConflict,
                            "The index revision changed",
                        )),
                        None,
                    );
                }
                match state.grants.issue(request, Instant::now()) {
                    Ok(issued) => {
                        let id = issued.grant_id.clone();
                        (Ok(issued), Some(id))
                    }
                    Err(e) => (Err(e), None),
                }
            },
            move |handle, _| {
                if let Some(id) = handle {
                    cleanup.grants.remove(&id);
                }
            },
        )?;
        Ok((issued?, ticket))
    })
    .await;

    // Checked after the awaited phase and before any result is matched, so a delayed error is
    // refused for the same reason a delayed success is.
    if Instant::now() >= at {
        if let Ok((issued, _)) = &issued {
            s.grants.remove(&issued.grant_id);
        }
        return fail(timed_out());
    }
    match issued {
        Ok((issued, ticket)) => {
            if let Err(e) = ticket.commit(&gate) {
                // Refused at handoff: nobody received this credential, so it leaves no row.
                s.grants.remove(&issued.grant_id);
                return fail(e);
            }
            (
            StatusCode::CREATED,
            Json(json!({
                "schemaVersion": 1,
                "grantId": issued.grant_id,
                "token": issued.token,
                "expiresAt": issued.expires_at_unix,
                "binding": body.binding,
                "admittedRevision": issued.admitted_revision,
                "capabilities": issued.capabilities.iter().map(|c| c.wire()).collect::<Vec<_>>(),
                "effectiveLimits": LimitsBody {
                    max_requests: issued.limits.max_requests,
                    max_total_response_bytes: issued.limits.max_total_response_bytes,
                    max_response_bytes: issued.limits.max_response_bytes,
                },
            })),
        )
                .into_response()
        }
        Err(e) => fail(e),
    }
}

/// Idempotent revocation. Unknown or already revoked grants also return 204: the caller learns
/// nothing about which grant identifiers exist.
async fn revoke(State(s): State<Arc<DaemonState>>, Path(grant_id): Path<String>) -> Response {
    s.grants.revoke(&grant_id);
    StatusCode::NO_CONTENT.into_response()
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct DescribeBody {
    schema_version: u32,
    binding: BindingBody,
}

/// Success envelope shared by every tool.
fn envelope(
    basis: &crate::mcp::EvidenceBasis,
    data: Value,
    truncation: Option<&'static str>,
) -> Value {
    json!({
        "schemaVersion": 1,
        "requestId": uuid::Uuid::new_v4().to_string(),
        "evidenceBasis": {
            "daemonInstanceId": basis.daemon_instance_id,
            "storeGeneration": basis.store_generation,
            "indexRevision": basis.index_revision,
        },
        "data": data,
        "warnings": [],
        "truncated": truncation.is_some(),
        "truncationReason": truncation.map(Value::from).unwrap_or(Value::Null),
    })
}

/// Identify the grant and reserve its budget.
///
/// Runs before the body is parsed, so size, schema, binding and semantic failures are all charged.
/// The binding is checked separately once the body is available.
fn identify<'a>(
    s: &'a Arc<DaemonState>,
    principal: &crate::http::Principal,
) -> Result<crate::mcp::grants::Admission<'a>, McpError> {
    // The guard admits only grants on this prefix; treat anything else as a programming error
    // rather than silently accepting it.
    let Some(token) = principal.grant_token() else {
        return Err(McpError::new(
            ErrorCode::Forbidden,
            "This credential may not be used on this route",
        ));
    };
    // The enrollment itself, not a sampled generation: a sample can be stale by the time it is
    // compared. Admission also distinguishes an ended binding, which is unavailable and needs a
    // restart, from a grant that is merely unusable and needs reissuing.
    s.grants
        .admit(token, s.enrollment.as_deref(), Instant::now())
}

/// Shared request preamble after identification: size, schema and binding, each charged.
fn accept<T: serde::de::DeserializeOwned>(
    admission: &crate::mcp::grants::Admission<'_>,
    body: &[u8],
    schema_version: impl FnOnce(&T) -> u32,
    binding: impl FnOnce(&T) -> crate::mcp::grants::Binding,
) -> Result<T, McpError> {
    // Checked before deserialization so an oversized body is too large rather than malformed.
    if body.len() > MAX_TOOL_REQUEST_BYTES {
        return Err(McpError::new(
            ErrorCode::BodyTooLarge,
            "The tool request exceeds the permitted size",
        ));
    }
    // Decoded here rather than by an extractor: non-UTF-8 bytes must be charged like any other
    // authenticated malformed request, not rejected before the grant is even identified.
    let parsed: T = serde_json::from_slice(body).map_err(|_| {
        McpError::new(
            ErrorCode::InvalidRequest,
            "The request is malformed or carries unknown fields",
        )
    })?;
    if schema_version(&parsed) != 1 {
        return Err(McpError::new(
            ErrorCode::InvalidRequest,
            "Unsupported schema version",
        ));
    }
    admission.check_binding(&binding(&parsed))?;
    Ok(parsed)
}

fn to_binding(binding: &BindingBody) -> crate::mcp::grants::Binding {
    crate::mcp::grants::Binding {
        daemon_instance_id: binding.daemon_instance_id.clone(),
        store_generation: binding.store_generation.clone(),
    }
}

/// Bootstraps a session: reports the admitted binding, the current and admitted revisions, and
/// whether evidence reads are possible. Exposes no evidence, no file list and no owner paths, and
/// is the only tool that takes no expected revision.
async fn describe(
    State(s): State<Arc<DaemonState>>,
    axum::Extension(principal): axum::Extension<crate::http::Principal>,
    request: axum::extract::Request,
) -> Response {
    let at = deadline();
    let admission = match identify(&s, &principal) {
        Ok(admission) => admission,
        Err(e) => return fail(e),
    };
    if let Err(e) = admission.check_capability(Capability::WorkspaceDescribe) {
        return charged(admission, e);
    }
    // Read only now, with the budget reserved, and only to the documented ceiling.
    let body = match read_body(request).await {
        Ok(body) => body,
        Err(()) => {
            return charged(
                admission,
                McpError::new(
                    ErrorCode::BodyTooLarge,
                    "The tool request exceeds the permitted size",
                ),
            );
        }
    };
    if let Err(e) = accept::<DescribeBody>(
        &admission,
        &body,
        |b| b.schema_version,
        |b| to_binding(&b.binding),
    ) {
        return charged(admission, e);
    }
    let Some(enrollment) = s.enrollment.clone() else {
        return charged(admission, unavailable());
    };

    let grant_id = admission.grant_id.clone();
    let max_bytes = admission.max_response_bytes;
    let label = std::path::Path::new(s.store_workspace_root())
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let state = s.clone();
    let gate = enrollment.clone();
    // The whole response is built inside the admission the store identity is checked in, so a
    // describe pending when another request observes invalidation cannot still report readable
    // evidence.
    let outcome = blocking_within(at, move || {
        let (admitted, ticket) = enrollment.admit_current(at, |revision| {
            state
                .grants
                .still_valid(&grant_id, state.enrollment.as_deref(), Instant::now())?;
            let Some(summary) = state.grants.summary(&grant_id) else {
                return Err(McpError::new(
                    ErrorCode::Unauthorized,
                    "The grant is not usable",
                ));
            };
            // A stale admitted revision is reported honestly rather than failing: describe still
            // bootstraps a session, it just cannot back an evidence read until the owner reissues.
            let readable = revision > 0 && revision == summary.admitted_revision;
            let basis = enrollment.basis(revision);
            let data = json!({
                "workspaceLabel": label,
                "binding": {
                    "daemonInstanceId": basis.daemon_instance_id.clone(),
                    "storeGeneration": basis.store_generation.clone(),
                },
                "indexRevision": revision,
                "admittedRevision": summary.admitted_revision,
                "evidenceReadable": readable,
                "toolSchemaVersion": 1,
                "serverVersion": env!("CARGO_PKG_VERSION"),
                "capabilities": summary.capabilities.iter().map(|c| c.wire()).collect::<Vec<_>>(),
                "effectiveLimits": LimitsBody {
                    max_requests: summary.limits.max_requests,
                    max_total_response_bytes: summary.limits.max_total_response_bytes,
                    max_response_bytes: summary.limits.max_response_bytes,
                },
                "expiresAt": summary.expires_at_unix,
            });
            let encoded = envelope(&basis, data, None).to_string();
            let size = encoded.len() as u64;
            if size > max_bytes {
                return Err(McpError::new(
                    ErrorCode::BudgetExhausted,
                    "The response exceeds the remaining response budget",
                ));
            }
            Ok((encoded, size, revision))
        })?;
        admitted.map(|value| (value, ticket))
    })
    .await;

    // Checked after the awaited phase and before any result is matched, so a delayed error is
    // refused for the same reason a delayed success is. A second confirmation is deliberately not
    // added: it would run in its own blocking phase and reopen the window it was meant to close.
    if Instant::now() >= at {
        return charged(admission, timed_out());
    }
    match outcome {
        Ok(((encoded, size, _revision), ticket)) => {
            // Committed under the same lock invalidation takes, so the response is either refused
            // or handed off with nothing able to interleave between the two.
            if let Err(e) = ticket.commit(&gate) {
                return charged(admission, e);
            }
            if let Err(e) =
                s.grants
                    .still_valid(&admission.grant_id, s.enrollment.as_deref(), Instant::now())
            {
                return charged(admission, e);
            }
            admission.settle(size);
            json_ok(encoded)
        }
        Err(e) => charged(admission, e),
    }
}

/// Maximum literal source fragment carried on one call row.
const MAX_CALLEE_TEXT_BYTES: usize = 1024;
/// Depth-one only: `baleyg_inspect` never expands recursively.
const MAX_CALLS: usize = 50;
const MAX_SOURCE_LINES: usize = 200;
const MAX_SOURCE_BYTES: usize = 16 * 1024;
const MAX_QUERY_BYTES: usize = 256;
const MAX_SYMBOL_ID_BYTES: usize = 8192;
const MAX_PATH_BYTES: usize = 4096;
const DEFAULT_LIMIT: u32 = 20;
const MAX_LIMIT: u32 = 50;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct FindBody {
    schema_version: u32,
    binding: BindingBody,
    expected_revision: u64,
    query: String,
    limit: Option<u32>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct InspectBody {
    schema_version: u32,
    binding: BindingBody,
    expected_revision: u64,
    symbol_id: String,
    view: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ReadSourceBody {
    schema_version: u32,
    binding: BindingBody,
    expected_revision: u64,
    path: String,
    start_line: usize,
    end_line: usize,
}

fn invalid(message: &'static str) -> McpError {
    McpError::new(ErrorCode::InvalidRequest, message)
}

/// Clip at a code-point boundary, never mid-character.
fn clip(text: &str, max: usize) -> (&str, bool) {
    if text.len() <= max {
        return (text, false);
    }
    let mut end = max;
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    (&text[..end], true)
}

/// Explicit projection of a measured declaration. Native DTOs are never serialized through:
/// unlisted fields stay out of the pilot surface until they have their own bounds review.
fn symbol_json(symbol: &crate::model::Symbol) -> Value {
    json!({
        "symbolId": symbol.id,
        "name": symbol.name,
        "kind": symbol.kind,
        "path": symbol.path,
        "range": {
            "startLine": symbol.range.start_line,
            "startColumn": symbol.range.start_column,
            "endLine": symbol.range.end_line,
            "endColumn": symbol.range.end_column,
            "startByte": symbol.range.start_byte,
            "endByte": symbol.range.end_byte,
        },
        "parentSymbolId": symbol.parent,
        "accessor": symbol.accessor,
        "evidence": {
            "source": symbol.provenance.source,
            "semantic": symbol.provenance.semantic,
        },
    })
}

/// One measured call site. `calleeText` is a literal source fragment, which is why inspect needs
/// source approval. Control regions, callback arguments and candidate bodies are excluded.
fn call_json(call: &crate::model::CallSite) -> (Value, bool) {
    let (text, clipped) = clip(&call.callee_text, MAX_CALLEE_TEXT_BYTES);
    (
        json!({
            "callId": call.id,
            "callerSymbolId": call.caller,
            "targetSymbolId": call.target,
            "resolution": call.resolution,
            "path": call.path,
            "range": {
                "startLine": call.range.start_line,
                "startColumn": call.range.start_column,
                "endLine": call.range.end_line,
                "endColumn": call.range.end_column,
                "startByte": call.range.start_byte,
                "endByte": call.range.end_byte,
            },
            "ordinal": call.ordinal,
            "calleeText": text,
            "calleeTextTruncated": clipped,
            "candidateCount": call.candidate_symbols.len(),
            "evidence": {
                "source": call.provenance.source,
                "semantic": call.provenance.semantic,
            },
        }),
        clipped,
    )
}

/// Relative indexed paths only. Never resolved against the filesystem: this addresses a cached row.
fn valid_relative_path(path: &str) -> bool {
    !path.is_empty()
        && path.len() <= MAX_PATH_BYTES
        && !path.starts_with('/')
        && !path.contains('\\')
        && !path.contains(':')
        && !path.contains('\0')
        && path
            .split('/')
            .all(|segment| !segment.is_empty() && segment != "." && segment != "..")
}

/// Shared evidence-read preamble: both the admitted revision and the transaction-pinned revision
/// must equal the request's expected revision.
fn check_revision(expected: u64, admitted: u64) -> Result<(), McpError> {
    if expected == 0 {
        return Err(invalid("An expected revision above zero is required"));
    }
    if expected != admitted {
        return Err(McpError::new(
            ErrorCode::RevisionConflict,
            "The index revision changed",
        ));
    }
    Ok(())
}

fn json_ok(encoded: String) -> Response {
    let mut response = (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "application/json")],
        encoded,
    )
        .into_response();
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, "no-store".parse().unwrap());
    response
}

/// The tail shared by every evidence tool.
///
/// One guarded read at the admitted revision, then the response built inside enrollment admission
/// so that grant validity, store identity and the current revision are all re-verified before any
/// evidence starts being sent. A publication or an invalidation that lands after the read is a
/// refusal, not a relabelled snapshot.
async fn serve_evidence<'a, T: Send + 'static>(
    s: &'a Arc<DaemonState>,
    admission: crate::mcp::grants::Admission<'a>,
    at: Instant,
    expected: u64,
    read: impl FnOnce(&rusqlite::Connection) -> rusqlite::Result<T> + Send + 'static,
    render: impl FnOnce(T) -> Result<(Value, Option<&'static str>), McpError> + Send + 'static,
) -> Response {
    let Some(enrollment) = s.enrollment.clone() else {
        return charged(admission, unavailable());
    };
    let grant_id = admission.grant_id.clone();
    let max_bytes = admission.max_response_bytes;
    let state = s.clone();
    let gate = enrollment.clone();
    let outcome = blocking_within(at, move || {
        let (revision, value) = enrollment.read(Some(expected), at, read)?;
        let basis = enrollment.basis(revision);
        let (admitted, ticket) = enrollment.admit(revision, at, move || {
            state
                .grants
                .still_valid(&grant_id, state.enrollment.as_deref(), Instant::now())?;
            let (data, truncation) = render(value)?;
            let encoded = envelope(&basis, data, truncation).to_string();
            let size = encoded.len() as u64;
            if size > max_bytes {
                return Err(McpError::new(
                    ErrorCode::BudgetExhausted,
                    "The response exceeds the remaining response budget",
                ));
            }
            Ok((encoded, size))
        })?;
        let (encoded, size) = admitted?;
        Ok((encoded, size, revision, ticket))
    })
    .await;
    // Checked after the awaited phase and before any result is matched, so a delayed error is
    // refused for the same reason a delayed success is. A second confirmation is deliberately not
    // added: it runs in its own blocking phase and reopens the very worker-to-handler window it
    // was meant to close. The guarantee lives in admission, which verifies before and after its
    // producer inside one boundary hold.
    if Instant::now() >= at {
        return charged(admission, timed_out());
    }
    match outcome {
        Ok((encoded, size, _revision, ticket)) => {
            // Committed under the same lock invalidation takes, so evidence is either refused or
            // handed off with nothing able to interleave between the two.
            if let Err(e) = ticket.commit(&gate) {
                return charged(admission, e);
            }
            if let Err(e) =
                s.grants
                    .still_valid(&admission.grant_id, s.enrollment.as_deref(), Instant::now())
            {
                return charged(admission, e);
            }
            admission.settle(size);
            json_ok(encoded)
        }
        Err(e) => charged(admission, e),
    }
}

async fn find_symbols(
    State(s): State<Arc<DaemonState>>,
    axum::Extension(principal): axum::Extension<crate::http::Principal>,
    request: axum::extract::Request,
) -> Response {
    let at = deadline();
    let admission = match identify(&s, &principal) {
        Ok(admission) => admission,
        Err(e) => return fail(e),
    };
    if let Err(e) = admission.check_capability(Capability::FindSymbols) {
        return charged(admission, e);
    }
    // Read only now, with the budget reserved, and only to the documented ceiling.
    let body = match read_body(request).await {
        Ok(body) => body,
        Err(()) => {
            return charged(
                admission,
                McpError::new(
                    ErrorCode::BodyTooLarge,
                    "The tool request exceeds the permitted size",
                ),
            );
        }
    };
    let parsed = match accept::<FindBody>(
        &admission,
        &body,
        |b| b.schema_version,
        |b| to_binding(&b.binding),
    ) {
        Ok(parsed) => parsed,
        Err(e) => return charged(admission, e),
    };
    if let Err(e) = check_revision(parsed.expected_revision, admission.admitted_revision) {
        return charged(admission, e);
    }
    if parsed.query.is_empty() || parsed.query.len() > MAX_QUERY_BYTES {
        return charged(admission, invalid("The query is empty or too long"));
    }
    let limit = parsed.limit.unwrap_or(DEFAULT_LIMIT);
    if limit == 0 || limit > MAX_LIMIT {
        return charged(
            admission,
            invalid("The requested limit is outside the permitted range"),
        );
    }
    let query = parsed.query.clone();
    // One extra row distinguishes a full page from a clipped one.
    let probe = i64::from(limit) + 1;
    serve_evidence(
        &s,
        admission,
        at,
        parsed.expected_revision,
        move |c| {
            let mut stmt = c.prepare(crate::store::sql::NODE_SEARCH)?;
            let rows =
                stmt.query_map(rusqlite::params![query, probe], |r| r.get::<_, String>(0))?;
            rows.collect::<rusqlite::Result<Vec<String>>>()
        },
        move |rows: Vec<String>| {
            let truncated = rows.len() > limit as usize;
            let symbols: Vec<Value> = rows
                .iter()
                .take(limit as usize)
                .filter_map(|row| serde_json::from_str::<crate::model::Symbol>(row).ok())
                .map(|symbol| symbol_json(&symbol))
                .collect();
            let data = json!({"symbols": symbols, "returned": symbols.len()});
            Ok((data, truncated.then_some("result_limit")))
        },
    )
    .await
}

async fn inspect(
    State(s): State<Arc<DaemonState>>,
    axum::Extension(principal): axum::Extension<crate::http::Principal>,
    request: axum::extract::Request,
) -> Response {
    let at = deadline();
    let admission = match identify(&s, &principal) {
        Ok(admission) => admission,
        Err(e) => return fail(e),
    };
    if let Err(e) = admission.check_capability(Capability::Inspect) {
        return charged(admission, e);
    }
    // Read only now, with the budget reserved, and only to the documented ceiling.
    let body = match read_body(request).await {
        Ok(body) => body,
        Err(()) => {
            return charged(
                admission,
                McpError::new(
                    ErrorCode::BodyTooLarge,
                    "The tool request exceeds the permitted size",
                ),
            );
        }
    };
    let parsed = match accept::<InspectBody>(
        &admission,
        &body,
        |b| b.schema_version,
        |b| to_binding(&b.binding),
    ) {
        Ok(parsed) => parsed,
        Err(e) => return charged(admission, e),
    };
    if let Err(e) = check_revision(parsed.expected_revision, admission.admitted_revision) {
        return charged(admission, e);
    }
    if parsed.symbol_id.is_empty() || parsed.symbol_id.len() > MAX_SYMBOL_ID_BYTES {
        return charged(
            admission,
            invalid("The symbol identifier is empty or too long"),
        );
    }
    let declaration = match parsed.view.as_str() {
        "declaration" => true,
        "outgoing_calls" => false,
        _ => return charged(admission, invalid("Unknown inspect view")),
    };
    let symbol_id = parsed.symbol_id.clone();
    serve_evidence(
        &s,
        admission,
        at,
        parsed.expected_revision,
        move |c| {
            let symbol: Option<String> = c
                .query_row(crate::store::sql::NODE_BY_ID, [&symbol_id], |r| r.get(0))
                .ok();
            let Some(symbol) = symbol else {
                return Ok((None, vec![]));
            };
            if declaration {
                return Ok((Some(symbol), vec![]));
            }
            let sql = format!("{} LIMIT ?2", crate::store::sql::CALLS_BY_CALLER);
            let mut stmt = c.prepare(&sql)?;
            let probe = MAX_CALLS as i64 + 1;
            let rows = stmt.query_map(rusqlite::params![symbol_id, probe], |r| {
                r.get::<_, String>(0)
            })?;
            Ok((
                Some(symbol),
                rows.collect::<rusqlite::Result<Vec<String>>>()?,
            ))
        },
        move |(symbol, calls): (Option<String>, Vec<String>)| {
            let Some(symbol) =
                symbol.and_then(|s| serde_json::from_str::<crate::model::Symbol>(&s).ok())
            else {
                return Err(McpError::new(
                    ErrorCode::NotFound,
                    "No such symbol at the authorized basis",
                ));
            };
            if declaration {
                let data = json!({"view": "declaration", "declaration": symbol_json(&symbol)});
                return Ok((data, None));
            }
            let clipped_list = calls.len() > MAX_CALLS;
            let mut fragment_clipped = false;
            let rendered: Vec<Value> = calls
                .iter()
                .take(MAX_CALLS)
                .filter_map(|row| serde_json::from_str::<crate::model::CallSite>(row).ok())
                .map(|call| {
                    let (value, clipped) = call_json(&call);
                    fragment_clipped |= clipped;
                    value
                })
                .collect();
            let reason = if clipped_list {
                Some("result_limit")
            } else if fragment_clipped {
                Some("source_fragment_limit")
            } else {
                None
            };
            let data = json!({
                "view": "outgoing_calls",
                "declaration": symbol_json(&symbol),
                "calls": rendered,
                "returned": rendered.len(),
            });
            Ok((data, reason))
        },
    )
    .await
}

async fn read_source(
    State(s): State<Arc<DaemonState>>,
    axum::Extension(principal): axum::Extension<crate::http::Principal>,
    request: axum::extract::Request,
) -> Response {
    let at = deadline();
    let admission = match identify(&s, &principal) {
        Ok(admission) => admission,
        Err(e) => return fail(e),
    };
    if let Err(e) = admission.check_capability(Capability::ReadSource) {
        return charged(admission, e);
    }
    // Read only now, with the budget reserved, and only to the documented ceiling.
    let body = match read_body(request).await {
        Ok(body) => body,
        Err(()) => {
            return charged(
                admission,
                McpError::new(
                    ErrorCode::BodyTooLarge,
                    "The tool request exceeds the permitted size",
                ),
            );
        }
    };
    let parsed = match accept::<ReadSourceBody>(
        &admission,
        &body,
        |b| b.schema_version,
        |b| to_binding(&b.binding),
    ) {
        Ok(parsed) => parsed,
        Err(e) => return charged(admission, e),
    };
    if let Err(e) = check_revision(parsed.expected_revision, admission.admitted_revision) {
        return charged(admission, e);
    }
    if !valid_relative_path(&parsed.path) {
        return charged(
            admission,
            invalid("The path is not a valid indexed relative path"),
        );
    }
    if parsed.start_line == 0 || parsed.end_line < parsed.start_line {
        return charged(admission, invalid("The requested line range is invalid"));
    }
    let path = parsed.path.clone();
    let (start_line, end_line) = (parsed.start_line, parsed.end_line);
    serve_evidence(
        &s,
        admission,
        at,
        parsed.expected_revision,
        move |c| {
            Ok(c.query_row(crate::store::sql::FILE_BY_PATH, [&path], |r| {
                r.get::<_, String>(0)
            })
            .ok())
        },
        move |row: Option<String>| {
            let Some(file) =
                row.and_then(|r| serde_json::from_str::<crate::model::SourceFile>(&r).ok())
            else {
                // Never falls back to reading the path from disk.
                return Err(McpError::new(
                    ErrorCode::NotFound,
                    "No such cached file at the authorized basis",
                ));
            };
            let lines: Vec<&str> = file.text.split_inclusive('\n').collect();
            // Both endpoints must lie within the cached file. Clamping the end instead would
            // return a shortened range and report it as complete.
            if start_line > lines.len() || end_line > lines.len() {
                return Err(invalid("The requested range is outside the cached file"));
            }
            let mut end = end_line.min(start_line + MAX_SOURCE_LINES - 1);
            let line_clipped = end < end_line;

            let start_byte: usize = lines[..start_line - 1].iter().map(|l| l.len()).sum();
            let mut bytes: usize = lines[start_line - 1..end].iter().map(|l| l.len()).sum();
            let mut byte_clipped = false;
            // Clip whole lines only, so a returned range never ends mid-line or mid-character.
            while bytes > MAX_SOURCE_BYTES && end > start_line {
                end -= 1;
                bytes = lines[start_line - 1..end].iter().map(|l| l.len()).sum();
                byte_clipped = true;
            }
            if bytes > MAX_SOURCE_BYTES {
                return Err(McpError::new(
                    ErrorCode::RangeTooLarge,
                    "A single line exceeds the source text budget",
                ));
            }
            let text: String = lines[start_line - 1..end].concat();
            let reason = if byte_clipped {
                Some("byte_limit")
            } else if line_clipped {
                Some("line_limit")
            } else {
                None
            };
            let data = json!({
                "path": file.path,
                "fileHash": file.hash,
                "language": file.language,
                "startLine": start_line,
                "endLine": end,
                "startByte": start_byte,
                "endByte": start_byte + text.len(),
                "text": text,
            });
            Ok((data, reason))
        },
    )
    .await
}

/// Unknown tool names under the tool prefix are denied in the contract envelope rather than
/// falling through to the daemon's generic handler. The principal is always a grant here: the
/// guard rejects the owner bearer on this prefix before dispatch.
/// Catch-all for the tool prefix: any method, any tail, including none.
///
/// Router-level rejections would answer before the request was identified, so an unusable
/// credential would learn which tools exist and a live grant could send unlimited misses for
/// free. Everything under the prefix is admitted first and answered afterwards.
async fn unknown_tool(
    State(s): State<Arc<DaemonState>>,
    axum::Extension(principal): axum::Extension<crate::http::Principal>,
    request: axum::extract::Request,
) -> Response {
    let Some(token) = principal.grant_token() else {
        return fail(McpError::new(
            ErrorCode::Forbidden,
            "This credential may not be used on this route",
        ));
    };
    // The enrollment itself, not a sampled generation string: a sample can be stale by the time it
    // is compared, which would let a grant outlive the binding it was issued against.
    let admission = match s
        .grants
        .admit(token, s.enrollment.as_deref(), Instant::now())
    {
        Ok(admission) => admission,
        Err(e) => return fail(e),
    };
    // Read only now, with the budget already reserved, and only to the documented ceiling. Doing
    // the work first and accounting for it afterwards would let a stalled or oversized stream
    // consume the server without ever being charged to the grant that caused it.
    let outcome = if read_body(request).await.is_err() {
        McpError::new(
            ErrorCode::BodyTooLarge,
            "The tool request exceeds the permitted size",
        )
    } else {
        McpError::new(
            ErrorCode::NotFound,
            "No such tool is available on this binding",
        )
    };
    charged(admission, outcome)
}

/// Read a tool request body, bounded by the documented ceiling.
///
/// Called after admission has reserved, so bytes a caller makes the server hold are charged to a
/// grant. `Err` means the body exceeded the ceiling.
async fn read_body(request: axum::extract::Request) -> Result<axum::body::Bytes, ()> {
    match axum::body::to_bytes(request.into_body(), MAX_TOOL_REQUEST_BYTES + 1).await {
        Ok(bytes) if bytes.len() <= MAX_TOOL_REQUEST_BYTES => Ok(bytes),
        _ => Err(()),
    }
}

/// Backend ceiling on a tool request body.
pub(crate) const MAX_TOOL_REQUEST_BYTES: usize = 16 * 1024;

pub fn routes() -> Router<Arc<DaemonState>> {
    Router::new()
        .route("/api/mcp-pilot/binding", get(binding))
        .route("/api/mcp-pilot/grants", post(issue))
        .route(
            "/api/mcp-pilot/grants/{grant_id}",
            axum::routing::delete(revoke),
        )
        // A method fallback, so a wrong method on a real tool is admitted and charged like any
        // other miss instead of being answered with a bare 405 before identification.
        .route(
            "/api/mcp-pilot/tools/baleyg_workspace_describe",
            post(describe).fallback(unknown_tool),
        )
        .route(
            "/api/mcp-pilot/tools/baleyg_find_symbols",
            post(find_symbols).fallback(unknown_tool),
        )
        .route(
            "/api/mcp-pilot/tools/baleyg_inspect",
            post(inspect).fallback(unknown_tool),
        )
        .route(
            "/api/mcp-pilot/tools/baleyg_read_source",
            post(read_source).fallback(unknown_tool),
        )
        // Every other method and tail under the prefix, so a router-level 404 or 405 can never
        // answer an authenticated request before it has been identified and charged.
        .route("/api/mcp-pilot/tools", axum::routing::any(unknown_tool))
        .route("/api/mcp-pilot/tools/", axum::routing::any(unknown_tool))
        .route(
            "/api/mcp-pilot/tools/{*tool}",
            axum::routing::any(unknown_tool),
        )
}
