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
use serde_json::json;
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

/// Contract error envelope. Distinct from the daemon's existing shape, and never carries SQL,
/// token fragments, absolute state paths or source.
fn fail(e: McpError) -> Response {
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
    let result = tokio::task::spawn_blocking(move || {
        let revision = enrollment.current_revision(deadline())?;
        Ok::<_, McpError>((enrollment.basis(revision), revision))
    })
    .await;
    match result {
        Ok(Ok((basis, revision))) => Json(json!({
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
        .into_response(),
        Ok(Err(e)) => fail(e),
        Err(_) => fail(unavailable()),
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

    let revision =
        match tokio::task::spawn_blocking(move || enrollment.current_revision(deadline())).await {
            Ok(Ok(revision)) => revision,
            Ok(Err(e)) => return fail(e),
            Err(_) => return fail(unavailable()),
        };
    if revision == 0 {
        return fail(McpError::new(
            ErrorCode::NoPublishedIndex,
            "The bound store has no published index; publish one before issuing a grant",
        ));
    }
    if revision != request.expected_revision {
        return fail(McpError::new(
            ErrorCode::RevisionConflict,
            "The index revision changed",
        ));
    }

    match s.grants.issue(request, Instant::now()) {
        Ok(issued) => (
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
            .into_response(),
        Err(e) => fail(e),
    }
}

/// Idempotent revocation. Unknown or already revoked grants also return 204: the caller learns
/// nothing about which grant identifiers exist.
async fn revoke(State(s): State<Arc<DaemonState>>, Path(grant_id): Path<String>) -> Response {
    s.grants.revoke(&grant_id);
    StatusCode::NO_CONTENT.into_response()
}

pub fn routes() -> Router<Arc<DaemonState>> {
    Router::new()
        .route("/api/mcp-pilot/binding", get(binding))
        .route("/api/mcp-pilot/grants", post(issue))
        .route(
            "/api/mcp-pilot/grants/{grant_id}",
            axum::routing::delete(revoke),
        )
}
