//! Owner-issued, short-lived, server-enforced read grants.
//!
//! A grant is a bearer secret: theft permits replay within its exact scope until expiry or
//! revocation. It is least privilege for this tool channel only, and does not constrain a runtime
//! that separately holds shell or broad filesystem authority.
//!
//! Only the digest and policy are retained. Grants live in memory and never survive a restart.

use super::{ErrorCode, McpError};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeSet, HashMap},
    sync::Mutex,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

/// Server ceilings. An owner may request less; a request for more is rejected rather than clamped.
pub const MAX_REQUESTS: u64 = 200;
pub const MAX_TOTAL_RESPONSE_BYTES: u64 = 2 * 1024 * 1024;
pub const MAX_RESPONSE_BYTES: u64 = 64 * 1024;
/// Floor on a grant's per-response ceiling.
///
/// Every answer, including the smallest error envelope, has to fit inside the ceiling the owner
/// chose. Permitting a ceiling below that would force the server either to exceed the limit it was
/// given or to answer with something untrue, so the ceiling is rejected at issuance instead.
pub const MIN_RESPONSE_BYTES: u64 = 512;
pub const MAX_TTL_SECONDS: u64 = 900;
pub const MAX_CONCURRENT_READS: u32 = 2;
pub const TOKEN_PREFIX: &str = "bgp_";

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Capability {
    WorkspaceDescribe,
    FindSymbols,
    Inspect,
    ReadSource,
}

impl Capability {
    pub fn wire(self) -> &'static str {
        match self {
            Self::WorkspaceDescribe => "baleyg_workspace_describe",
            Self::FindSymbols => "baleyg_find_symbols",
            Self::Inspect => "baleyg_inspect",
            Self::ReadSource => "baleyg_read_source",
        }
    }
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "baleyg_workspace_describe" => Some(Self::WorkspaceDescribe),
            "baleyg_find_symbols" => Some(Self::FindSymbols),
            "baleyg_inspect" => Some(Self::Inspect),
            "baleyg_read_source" => Some(Self::ReadSource),
            _ => None,
        }
    }
    /// `CallSite.callee_text` and cached ranges are literal source substrings, so inspect discloses
    /// source in either view. Structural metadata alone is still owner-approved disclosure.
    pub fn needs_source_approval(self) -> bool {
        matches!(self, Self::Inspect | Self::ReadSource)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Binding {
    pub daemon_instance_id: String,
    pub store_generation: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    pub max_requests: u64,
    pub max_total_response_bytes: u64,
    pub max_response_bytes: u64,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_requests: MAX_REQUESTS,
            max_total_response_bytes: MAX_TOTAL_RESPONSE_BYTES,
            max_response_bytes: MAX_RESPONSE_BYTES,
        }
    }
}

impl Limits {
    fn validate(&self) -> Result<(), McpError> {
        let over = self.max_requests > MAX_REQUESTS
            || self.max_total_response_bytes > MAX_TOTAL_RESPONSE_BYTES
            || self.max_response_bytes > MAX_RESPONSE_BYTES;
        let empty = self.max_requests == 0
            || self.max_total_response_bytes == 0
            || self.max_response_bytes < MIN_RESPONSE_BYTES
            || self.max_total_response_bytes < MIN_RESPONSE_BYTES;
        if over || empty {
            return Err(McpError::new(
                ErrorCode::InvalidRequest,
                "Requested limits exceed the server ceilings",
            ));
        }
        Ok(())
    }
}

pub struct IssueRequest {
    pub binding: Binding,
    pub expected_revision: u64,
    pub capabilities: BTreeSet<Capability>,
    pub ttl_seconds: u64,
    pub limits: Limits,
    pub client_label: String,
    pub recipient: String,
    pub source_approved: bool,
}

impl IssueRequest {
    /// Request-shape, capability and disclosure rules. Checked before any store read so a
    /// malformed request never depends on storage state.
    pub fn validate(&self) -> Result<(), McpError> {
        if self.capabilities.is_empty()
            || !self.capabilities.contains(&Capability::WorkspaceDescribe)
        {
            return Err(McpError::new(
                ErrorCode::InvalidRequest,
                "A grant must include the describe capability",
            ));
        }
        if !self.source_approved && self.capabilities.iter().any(|c| c.needs_source_approval()) {
            return Err(McpError::new(
                ErrorCode::Forbidden,
                "Inspect and source reads require explicit source approval",
            ));
        }
        if self.ttl_seconds == 0 || self.ttl_seconds > MAX_TTL_SECONDS {
            return Err(McpError::new(
                ErrorCode::InvalidRequest,
                "Requested lifetime is outside the permitted range",
            ));
        }
        self.limits.validate()?;
        Ok(())
    }
}

pub struct Issued {
    pub grant_id: String,
    /// Returned exactly once, to the owner helper. There is no retrieval endpoint.
    pub token: String,
    pub expires_at_unix: u64,
    pub admitted_revision: u64,
    pub capabilities: Vec<Capability>,
    pub limits: Limits,
}

/// Redacts the token. A grant secret must never reach a log line, a panic message or a test
/// failure dump.
impl std::fmt::Debug for Issued {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Issued")
            .field("grant_id", &self.grant_id)
            .field("token", &"<redacted>")
            .field("expires_at_unix", &self.expires_at_unix)
            .field("admitted_revision", &self.admitted_revision)
            .field("capabilities", &self.capabilities)
            .field("limits", &self.limits)
            .finish()
    }
}

struct Grant {
    grant_id: String,
    expires_at_unix: u64,
    binding: Binding,
    admitted_revision: u64,
    capabilities: BTreeSet<Capability>,
    source_approved: bool,
    expires_at: Instant,
    limits: Limits,
    used_requests: u64,
    reserved_bytes: u64,
    in_flight: u32,
    revoked: bool,
}

/// One admitted request. The reservation is settled on `settle`, or charged in full if this guard
/// is dropped without one: a cancelled request never refunds its admitted count, and over-charging
/// bytes is the safe direction.
pub struct Admission<'a> {
    grants: &'a Grants,
    digest: [u8; 32],
    pub grant_id: String,
    pub admitted_revision: u64,
    pub max_response_bytes: u64,
    binding: Binding,
    capabilities: BTreeSet<Capability>,
    source_approved: bool,
    reserved: u64,
    settled: bool,
}

/// Never prints the token digest or any secret-derived value.
impl std::fmt::Debug for Admission<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Admission")
            .field("grant_id", &self.grant_id)
            .field("admitted_revision", &self.admitted_revision)
            .field("reserved", &self.reserved)
            .finish()
    }
}

impl Admission<'_> {
    /// Checked after reservation, so a forbidden call is charged like any other authenticated
    /// error. Enforced here as well as at issuance, so an inconsistent policy cannot widen
    /// disclosure.
    pub fn check_capability(&self, capability: Capability) -> Result<(), McpError> {
        if !self.capabilities.contains(&capability) {
            return Err(McpError::new(
                ErrorCode::Forbidden,
                "The grant does not carry this capability",
            ));
        }
        if capability.needs_source_approval() && !self.source_approved {
            return Err(McpError::new(
                ErrorCode::Forbidden,
                "The grant does not approve source disclosure",
            ));
        }
        Ok(())
    }

    /// The generation this grant was admitted against. An enrollment that has been invalidated
    /// rotates its generation, which must retire every grant issued before it.
    pub fn store_generation(&self) -> &str {
        &self.binding.store_generation
    }

    /// Checked after the body is parsed. Admission itself happens first so that malformed and
    /// mismatched requests are still charged to the grant that sent them.
    pub fn check_binding(&self, presented: &Binding) -> Result<(), McpError> {
        if &self.binding == presented {
            Ok(())
        } else {
            Err(McpError::new(
                ErrorCode::BindingMismatch,
                "The request does not match the admitted binding",
            ))
        }
    }

    pub fn settle(mut self, actual_bytes: u64) {
        self.grants
            .settle(&self.digest, self.reserved, actual_bytes);
        self.settled = true;
    }
}

impl Drop for Admission<'_> {
    fn drop(&mut self) {
        if !self.settled {
            self.grants
                .settle(&self.digest, self.reserved, self.reserved);
        }
    }
}

#[derive(Debug, Clone)]
pub struct Summary {
    pub expires_at_unix: u64,
    pub admitted_revision: u64,
    pub capabilities: Vec<Capability>,
    pub limits: Limits,
}

#[derive(Default)]
pub struct Grants {
    table: Mutex<HashMap<[u8; 32], Grant>>,
}

fn digest(token: &str) -> [u8; 32] {
    Sha256::digest(token.as_bytes()).into()
}

/// Uniform failure for a missing, invalid, expired or revoked grant: the caller learns only that
/// the credential is not usable, never whether the secret itself was recognized.
fn unauthorized() -> McpError {
    McpError::new(ErrorCode::Unauthorized, "The grant is not usable")
}

impl Grants {
    pub fn new() -> Self {
        Self::default()
    }

    /// Issue a grant. Callers must have authenticated the owner and already checked the store
    /// binding and published revision; this enforces request shape, capability and disclosure rules.
    pub fn issue(&self, request: IssueRequest, now: Instant) -> Result<Issued, McpError> {
        // Validated again here even though callers validate first, so no route can widen a grant.
        request.validate()?;

        let bytes: [u8; 32] = rand::random();
        let token = format!("{TOKEN_PREFIX}{}", hex::encode(bytes));
        let grant_id = uuid::Uuid::new_v4().to_string();
        let ttl = Duration::from_secs(request.ttl_seconds);
        let expires_at_unix = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs() + request.ttl_seconds)
            .unwrap_or(request.ttl_seconds);
        let grant = Grant {
            grant_id: grant_id.clone(),
            expires_at_unix,
            binding: request.binding,
            admitted_revision: request.expected_revision,
            capabilities: request.capabilities.clone(),
            source_approved: request.source_approved,
            expires_at: now + ttl,
            limits: request.limits,
            used_requests: 0,
            reserved_bytes: 0,
            in_flight: 0,
            revoked: false,
        };
        self.table
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(digest(&token), grant);
        Ok(Issued {
            grant_id,
            token,
            expires_at_unix,
            admitted_revision: request.expected_revision,
            capabilities: request.capabilities.into_iter().collect(),
            limits: request.limits,
        })
    }

    /// Remove a grant entirely.
    ///
    /// Used when a credential is discarded before its token reaches anyone: a revoked row would
    /// linger until expiry describing a grant no caller ever held.
    pub fn remove(&self, grant_id: &str) {
        let mut table = self.table.lock().unwrap_or_else(|e| e.into_inner());
        table.retain(|_, grant| grant.grant_id != grant_id);
    }

    /// Idempotent: revoking an unknown or already revoked grant is not an error.
    pub fn revoke(&self, grant_id: &str) {
        let mut table = self.table.lock().unwrap_or_else(|e| e.into_inner());
        for grant in table.values_mut() {
            if grant.grant_id == grant_id {
                grant.revoked = true;
            }
        }
    }

    /// Admit one request against a presented token, reserving its budget before any work starts.
    /// Identify the presented grant and reserve its budget.
    ///
    /// Deliberately does not check the capability. A forbidden call is authenticated ordinary error
    /// traffic and must be charged, so the capability is checked through the returned admission
    /// once accounting is in place. The same reservation serves a request naming a tool that does
    /// not exist.
    pub fn admit(
        &self,
        token: &str,
        lifetime: Option<&super::enrollment::Enrollment>,
        now: Instant,
    ) -> Result<Admission<'_>, McpError> {
        let key = digest(token);
        let mut table = self.table.lock().unwrap_or_else(|e| e.into_inner());
        let grant = table.get_mut(&key).ok_or_else(unauthorized)?;
        if grant.revoked || now >= grant.expires_at {
            return Err(unauthorized());
        }
        // An enrollment that ends rotates its generation, retiring every grant issued against the
        // old one. Reported as unusable rather than a binding mismatch: the caller's binding still
        // matches the grant's, so this is a lifetime end, not a wrong request.
        // Read from the enrollment here rather than taken as a caller-supplied string: a sampled
        // generation can already be stale when it arrives, which would let a grant outlive the
        // binding it was issued against. An ended enrollment is reported as unavailable rather
        // than merely unauthorized, so the caller is told to restart rather than to reissue.
        if let Some(lifetime) = lifetime {
            if !lifetime.is_available() {
                return Err(McpError::new(
                    ErrorCode::StoreUnavailable,
                    "The bound store is unavailable",
                ));
            }
            if grant.binding.store_generation != lifetime.store_generation() {
                return Err(unauthorized());
            }
        }
        if grant.in_flight >= MAX_CONCURRENT_READS {
            return Err(McpError::new(
                ErrorCode::TooManyRequests,
                "Too many concurrent requests for this grant",
            ));
        }
        if grant.used_requests >= grant.limits.max_requests {
            return Err(McpError::new(
                ErrorCode::BudgetExhausted,
                "The grant request budget is exhausted",
            ));
        }
        // Reserve up front so concurrent requests cannot jointly exceed the ceiling. When less than
        // a full response remains, admit with a correspondingly tighter cap rather than refusing:
        // reserving the worst case unconditionally would strand most of the request allowance.
        // A budget-denied error response needs no reservation, which is the fixed error allowance.
        let remaining = grant
            .limits
            .max_total_response_bytes
            .saturating_sub(grant.reserved_bytes);
        if remaining == 0 {
            return Err(McpError::new(
                ErrorCode::BudgetExhausted,
                "The grant response budget is exhausted",
            ));
        }
        let reserved = grant.limits.max_response_bytes.min(remaining);
        grant.used_requests += 1;
        grant.reserved_bytes += reserved;
        grant.in_flight += 1;
        Ok(Admission {
            grants: self,
            digest: key,
            grant_id: grant.grant_id.clone(),
            admitted_revision: grant.admitted_revision,
            max_response_bytes: reserved,
            binding: grant.binding.clone(),
            capabilities: grant.capabilities.clone(),
            source_approved: grant.source_approved,
            reserved,
            settled: false,
        })
    }

    fn settle(&self, key: &[u8; 32], reserved: u64, actual: u64) {
        let mut table = self.table.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(grant) = table.get_mut(key) {
            let actual = actual.min(reserved);
            grant.reserved_bytes = grant.reserved_bytes.saturating_sub(reserved - actual);
            grant.in_flight = grant.in_flight.saturating_sub(1);
        }
    }

    /// Non-secret policy of one live grant, for describe. Never exposes the token or its digest.
    pub fn summary(&self, grant_id: &str) -> Option<Summary> {
        let table = self.table.lock().unwrap_or_else(|e| e.into_inner());
        table
            .values()
            .find(|g| g.grant_id == grant_id)
            .map(|g| Summary {
                expires_at_unix: g.expires_at_unix,
                admitted_revision: g.admitted_revision,
                capabilities: g.capabilities.iter().copied().collect(),
                limits: g.limits,
            })
    }

    /// Re-check just before emitting a response. Expiry or revocation during a read discards it.
    pub fn still_valid(
        &self,
        grant_id: &str,
        lifetime: Option<&super::enrollment::Enrollment>,
        now: Instant,
    ) -> Result<(), McpError> {
        if let Some(lifetime) = lifetime
            && !lifetime.is_available()
        {
            return Err(McpError::new(
                ErrorCode::StoreUnavailable,
                "The bound store is unavailable",
            ));
        }
        let current_generation = lifetime.map(|l| l.store_generation()).unwrap_or_default();
        let table = self.table.lock().unwrap_or_else(|e| e.into_inner());
        // Generation-aware, like admission: work already in flight must not survive an enrollment
        // that ended while it ran.
        let live = table.values().any(|g| {
            g.grant_id == grant_id
                && !g.revoked
                && now < g.expires_at
                && g.binding.store_generation == current_generation
        });
        if live { Ok(()) } else { Err(unauthorized()) }
    }

    #[cfg(test)]
    pub fn issued_count(&self) -> usize {
        self.table.lock().unwrap_or_else(|e| e.into_inner()).len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn binding() -> Binding {
        Binding {
            daemon_instance_id: "daemon".into(),
            store_generation: "generation".into(),
        }
    }

    fn request(capabilities: &[Capability], source_approved: bool) -> IssueRequest {
        IssueRequest {
            binding: binding(),
            expected_revision: 7,
            capabilities: capabilities.iter().copied().collect(),
            ttl_seconds: 900,
            limits: Limits::default(),
            client_label: "terminal-pilot".into(),
            recipient: "approved local client".into(),
            source_approved,
        }
    }

    fn all() -> Vec<Capability> {
        vec![
            Capability::WorkspaceDescribe,
            Capability::FindSymbols,
            Capability::Inspect,
            Capability::ReadSource,
        ]
    }

    #[test]
    fn describe_is_required() {
        let grants = Grants::new();
        let err = grants
            .issue(request(&[Capability::FindSymbols], true), Instant::now())
            .unwrap_err();
        assert_eq!(err.code, ErrorCode::InvalidRequest);
        assert_eq!(grants.issued_count(), 0);
    }

    #[test]
    fn source_capabilities_require_approval() {
        let grants = Grants::new();
        for capability in [Capability::Inspect, Capability::ReadSource] {
            let err = grants
                .issue(
                    request(&[Capability::WorkspaceDescribe, capability], false),
                    Instant::now(),
                )
                .unwrap_err();
            assert_eq!(err.code, ErrorCode::Forbidden);
        }
        // Describe and find alone remain issuable without source approval.
        assert!(
            grants
                .issue(
                    request(
                        &[Capability::WorkspaceDescribe, Capability::FindSymbols],
                        false
                    ),
                    Instant::now()
                )
                .is_ok()
        );
    }

    #[test]
    fn limits_and_lifetime_are_rejected_not_clamped() {
        let grants = Grants::new();
        let mut over = request(&all(), true);
        over.limits.max_requests = MAX_REQUESTS + 1;
        assert_eq!(
            grants.issue(over, Instant::now()).unwrap_err().code,
            ErrorCode::InvalidRequest
        );
        let mut long = request(&all(), true);
        long.ttl_seconds = MAX_TTL_SECONDS + 1;
        assert_eq!(
            grants.issue(long, Instant::now()).unwrap_err().code,
            ErrorCode::InvalidRequest
        );
        let mut zero = request(&all(), true);
        zero.ttl_seconds = 0;
        assert_eq!(
            grants.issue(zero, Instant::now()).unwrap_err().code,
            ErrorCode::InvalidRequest
        );
    }

    #[test]
    fn backend_rechecks_source_approval_independently_of_issuance() {
        // An inconsistent policy cannot arise through issue(), so inject one directly: the
        // capability is granted while source approval is not.
        let grants = Grants::new();
        let token = "bgp_injected";
        grants.table.lock().unwrap().insert(
            digest(token),
            Grant {
                grant_id: "injected".into(),
                expires_at_unix: 0,
                binding: binding(),
                admitted_revision: 7,
                capabilities: all().into_iter().collect(),
                source_approved: false,
                expires_at: Instant::now() + Duration::from_secs(900),
                limits: Limits::default(),
                used_requests: 0,
                reserved_bytes: 0,
                in_flight: 0,
                revoked: false,
            },
        );
        let now = Instant::now();
        for capability in [Capability::Inspect, Capability::ReadSource] {
            let err = grants
                .admit(token, None, now)
                .unwrap()
                .check_capability(capability)
                .unwrap_err();
            assert_eq!(err.code, ErrorCode::Forbidden);
        }
        // Structural capabilities on the same grant still work.
        assert!(grants.admit(token, None, now).is_ok());
    }

    #[test]
    fn unknown_expired_and_revoked_grants_are_indistinguishable() {
        let grants = Grants::new();
        let issued = grants.issue(request(&all(), true), Instant::now()).unwrap();
        let now = Instant::now();
        assert_eq!(
            grants.admit("bgp_nope", None, now).unwrap_err().code,
            ErrorCode::Unauthorized
        );
        // Fake clock: past the monotonic deadline.
        assert_eq!(
            grants
                .admit(
                    &issued.token,
                    None,
                    now + Duration::from_secs(MAX_TTL_SECONDS + 1),
                )
                .unwrap_err()
                .code,
            ErrorCode::Unauthorized
        );
        grants.revoke(&issued.grant_id);
        assert_eq!(
            grants.admit(&issued.token, None, now).unwrap_err().code,
            ErrorCode::Unauthorized
        );
        // Revocation is idempotent and unknown identifiers are accepted silently.
        grants.revoke(&issued.grant_id);
        grants.revoke("no-such-grant");
        assert_eq!(
            grants
                .still_valid(&issued.grant_id, None, now)
                .unwrap_err()
                .code,
            ErrorCode::Unauthorized
        );
    }

    #[test]
    fn a_different_binding_is_refused_after_admission() {
        let grants = Grants::new();
        let issued = grants.issue(request(&all(), true), Instant::now()).unwrap();
        let other = Binding {
            daemon_instance_id: "daemon".into(),
            store_generation: "rotated".into(),
        };
        // Admission succeeds and reserves, so a mismatched request is still charged to its grant;
        // the binding is checked once the body has been parsed.
        let admission = grants.admit(&issued.token, None, Instant::now()).unwrap();
        assert_eq!(
            admission.check_binding(&other).unwrap_err().code,
            ErrorCode::BindingMismatch
        );
        assert!(admission.check_binding(&binding()).is_ok());
    }

    #[test]
    fn a_forbidden_capability_is_still_charged() {
        let grants = Grants::new();
        let mut small = request(&[Capability::WorkspaceDescribe], false);
        small.limits.max_requests = 1;
        let issued = grants.issue(small, Instant::now()).unwrap();
        let now = Instant::now();

        // Reservation happens first, so the forbidden call spends the budget.
        let admission = grants.admit(&issued.token, None, now).unwrap();
        assert_eq!(
            admission
                .check_capability(Capability::Inspect)
                .unwrap_err()
                .code,
            ErrorCode::Forbidden
        );
        drop(admission);
        assert_eq!(
            grants.admit(&issued.token, None, now).unwrap_err().code,
            ErrorCode::BudgetExhausted
        );
    }

    #[test]
    fn missing_capability_is_forbidden() {
        let grants = Grants::new();
        let issued = grants
            .issue(
                request(&[Capability::WorkspaceDescribe], false),
                Instant::now(),
            )
            .unwrap();
        assert_eq!(
            grants
                .admit(&issued.token, None, Instant::now())
                .unwrap()
                .check_capability(Capability::FindSymbols)
                .unwrap_err()
                .code,
            ErrorCode::Forbidden
        );
    }

    #[test]
    fn the_request_budget_is_spent_and_not_refunded_by_cancellation() {
        let grants = Grants::new();
        let mut small = request(&all(), true);
        small.limits.max_requests = 2;
        let issued = grants.issue(small, Instant::now()).unwrap();
        let now = Instant::now();
        for _ in 0..2 {
            // Dropped without settle, as a cancelled request would be.
            drop(grants.admit(&issued.token, None, now).unwrap());
        }
        assert_eq!(
            grants.admit(&issued.token, None, now).unwrap_err().code,
            ErrorCode::BudgetExhausted
        );
    }

    #[test]
    fn concurrent_admissions_cannot_exceed_the_byte_ceiling() {
        let grants = Grants::new();
        let mut small = request(&all(), true);
        small.limits.max_total_response_bytes = MAX_RESPONSE_BYTES;
        let issued = grants.issue(small, Instant::now()).unwrap();
        let now = Instant::now();
        let first = grants.admit(&issued.token, None, now).unwrap();
        // The whole ceiling is reserved by the in-flight request.
        assert_eq!(
            grants.admit(&issued.token, None, now).unwrap_err().code,
            ErrorCode::BudgetExhausted
        );
        // Settling the actual size returns the unused reservation, and the next admission is
        // capped at what remains rather than refused.
        first.settle(16);
        let next = grants.admit(&issued.token, None, now).unwrap();
        assert_eq!(next.max_response_bytes, MAX_RESPONSE_BYTES - 16);
    }

    #[test]
    fn concurrency_is_capped_per_grant() {
        let grants = Grants::new();
        let issued = grants.issue(request(&all(), true), Instant::now()).unwrap();
        let now = Instant::now();
        let mut held = Vec::new();
        for _ in 0..MAX_CONCURRENT_READS {
            held.push(grants.admit(&issued.token, None, now).unwrap());
        }
        assert_eq!(
            grants.admit(&issued.token, None, now).unwrap_err().code,
            ErrorCode::TooManyRequests
        );
        held.clear();
        assert!(grants.admit(&issued.token, None, now).is_ok());
    }

    #[test]
    fn an_unknown_tool_still_spends_budget() {
        let grants = Grants::new();
        let mut small = request(&[Capability::WorkspaceDescribe], false);
        small.limits.max_requests = 1;
        let issued = grants.issue(small, Instant::now()).unwrap();
        let now = Instant::now();

        // No capability is named, but the request is authenticated, so it is charged.
        drop(grants.admit(&issued.token, None, now).unwrap());
        assert_eq!(
            grants.admit(&issued.token, None, now).unwrap_err().code,
            ErrorCode::BudgetExhausted
        );
    }

    #[test]
    fn tokens_are_prefixed_opaque_secrets_and_are_not_recoverable() {
        let grants = Grants::new();
        let issued = grants.issue(request(&all(), true), Instant::now()).unwrap();
        assert!(issued.token.starts_with(TOKEN_PREFIX));
        assert_eq!(issued.token.len(), TOKEN_PREFIX.len() + 64);
        assert_ne!(issued.token, issued.grant_id);
        assert!(!issued.token.contains(&issued.grant_id));
        // The grant identifier is not a credential.
        assert_eq!(
            grants
                .admit(&issued.grant_id, None, Instant::now())
                .unwrap_err()
                .code,
            ErrorCode::Unauthorized
        );
    }
}

#[cfg(test)]
mod lifetime_tests {
    use super::*;
    use std::collections::BTreeSet;

    fn request() -> IssueRequest {
        IssueRequest {
            binding: Binding {
                daemon_instance_id: "daemon".into(),
                store_generation: "generation".into(),
            },
            expected_revision: 7,
            capabilities: BTreeSet::from([Capability::WorkspaceDescribe]),
            ttl_seconds: 900,
            limits: Limits::default(),
            client_label: "terminal-pilot".into(),
            recipient: "approved local client".into(),
            source_approved: false,
        }
    }

    #[test]
    fn a_discarded_grant_leaves_no_row_behind() {
        let grants = Grants::new();
        let issued = grants.issue(request(), Instant::now()).unwrap();
        assert_eq!(grants.issued_count(), 1);
        grants.remove(&issued.grant_id);
        assert_eq!(
            grants.issued_count(),
            0,
            "a credential discarded before its token was returned left a row"
        );
    }
}
