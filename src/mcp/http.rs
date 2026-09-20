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

/// Contract error envelope. Distinct from the daemon's existing shape, and never carries SQL,
/// token fragments, absolute state paths or source.
pub(crate) fn fail(e: McpError) -> Response {
    let status = StatusCode::from_u16(e.code.status()).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
    let mut response = (
        status,
        Json(json!({
            "schemaVersion": 1,
            "error": {"code": e.code.as_str(), "message": e.message, "retryable": e.code.retryable()},
            "requestId": uuid::Uuid::new_v4().to_string(),
        })),
    )
        .into_response();
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, "no-store".parse().unwrap());
    response
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
    // Settled with the size actually emitted: dropping the admission would charge the whole
    // reservation for a short error. Emission is also capped by what remains, so an ordinary error
    // never exceeds the allowance and is replaced by the smaller budget denial when it would.
    let mut status = outcome.code.status();
    let mut rendered = envelope_for(&outcome);
    if rendered.to_string().len() as u64 > admission.max_response_bytes {
        let denial = McpError::new(
            ErrorCode::BudgetExhausted,
            "The grant response budget is exhausted",
        );
        status = denial.code.status();
        rendered = envelope_for(&denial);
    }
    admission.settle(rendered.to_string().len() as u64);
    let status = StatusCode::from_u16(status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
    let mut response = (status, Json(rendered)).into_response();
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, "no-store".parse().unwrap());
    response
}

fn envelope_for(e: &McpError) -> Value {
    json!({
        "schemaVersion": 1,
        "error": {
            "code": e.code.as_str(),
            "message": e.message,
            "retryable": e.code.retryable(),
        },
        "requestId": uuid::Uuid::new_v4().to_string(),
    })
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
        // Every method and every tail under the tool prefix, so a router-level 404 or 405 can
        // never answer an authenticated request before it has been identified and charged.
        .route("/api/mcp-pilot/tools", axum::routing::any(unknown_tool))
        .route("/api/mcp-pilot/tools/", axum::routing::any(unknown_tool))
        .route(
            "/api/mcp-pilot/tools/{*tool}",
            axum::routing::any(unknown_tool),
        )
}
