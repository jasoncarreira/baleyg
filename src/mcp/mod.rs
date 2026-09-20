//! Read-only MCP pilot: enrollment, grants, tool routes, stdio adapter and handoff.
//!
//! Implements [`docs/mcp-readonly-pilot-contract.md`]. The pilot deliberately does not reuse
//! [`crate::store::Store`] read entry points: they reopen the cache by path on every call and
//! their revision guards are optional or absent, neither of which can satisfy the contract's
//! binding. See [`enrollment`] for the connection this module owns instead.

pub mod enrollment;

/// Structured error vocabulary for the pilot's HTTP and tool surfaces.
///
/// Messages are `&'static str` by construction: an error may never carry SQL, token fragments,
/// absolute state paths or source text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorCode {
    InvalidRequest,
    RangeTooLarge,
    Unauthorized,
    Forbidden,
    BindingMismatch,
    RevisionConflict,
    NoPublishedIndex,
    NotFound,
    BodyTooLarge,
    BudgetExhausted,
    TooManyRequests,
    DeadlineExceeded,
    StoreUnavailable,
}

impl ErrorCode {
    /// Wire code, as written in the contract's error table.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::InvalidRequest => "invalid_request",
            Self::RangeTooLarge => "range_too_large",
            Self::Unauthorized => "unauthorized",
            Self::Forbidden => "forbidden",
            Self::BindingMismatch => "binding_mismatch",
            Self::RevisionConflict => "revision_conflict",
            Self::NoPublishedIndex => "no_published_index",
            Self::NotFound => "not_found",
            Self::BodyTooLarge => "body_too_large",
            Self::BudgetExhausted => "budget_exhausted",
            Self::TooManyRequests => "too_many_requests",
            Self::DeadlineExceeded => "deadline_exceeded",
            Self::StoreUnavailable => "store_unavailable",
        }
    }
    pub fn status(self) -> u16 {
        match self {
            Self::InvalidRequest | Self::RangeTooLarge => 400,
            Self::Unauthorized => 401,
            Self::Forbidden => 403,
            Self::NotFound => 404,
            Self::BindingMismatch | Self::RevisionConflict | Self::NoPublishedIndex => 409,
            Self::BodyTooLarge => 413,
            Self::BudgetExhausted | Self::TooManyRequests => 429,
            Self::StoreUnavailable => 503,
            Self::DeadlineExceeded => 504,
        }
    }
    /// Only transient conditions are retryable. A latched store and an exhausted lifetime budget
    /// are terminal: they need owner action, not another request.
    pub fn retryable(self) -> bool {
        matches!(self, Self::TooManyRequests)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpError {
    pub code: ErrorCode,
    pub message: &'static str,
}

impl McpError {
    pub fn new(code: ErrorCode, message: &'static str) -> Self {
        Self { code, message }
    }
}

impl std::fmt::Display for McpError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.code.as_str(), self.message)
    }
}

impl std::error::Error for McpError {}

/// Identifies exactly which snapshot a result came from. Returned on every tool response.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EvidenceBasis {
    pub daemon_instance_id: String,
    pub store_generation: String,
    pub index_revision: u64,
}
