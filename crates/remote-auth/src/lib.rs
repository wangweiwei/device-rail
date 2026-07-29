//! Optional authentication, authorization, and tamper-evident audit support
//! for DeviceRail's loopback RPC transport.
//!
//! This crate supplies no network encryption. A client on another host must
//! reach the loopback listener through an independently authenticated SSH or
//! mTLS tunnel.

mod audit;
mod auth;
mod credentials;
#[cfg(unix)]
mod owner_only;
mod permission;

pub use audit::{
    AuditDecision, AuditError, AuditEvent, AuditLog, AuditOutcome, AuditRecord, AuditStage,
};
pub use auth::{
    AUTH_PROTOCOL_VERSION, AuthChallenge, AuthChallengeRequest, AuthError, AuthProofRequest,
    AuthSession, AuthSuccess, Authenticator, ChallengeConfig, compute_proof,
};
pub use credentials::{CredentialError, CredentialStore};
pub use permission::{AuthenticatedPrincipal, Permission, required_permission};

pub const AUTH_PROTOCOL_SCHEMA: &str = include_str!("../protocol/auth-v1.schema.json");
pub const CREDENTIAL_STORE_SCHEMA: &str =
    include_str!("../protocol/credential-store-v1.schema.json");
pub const AUDIT_RECORD_SCHEMA: &str = include_str!("../protocol/audit-record-v1.schema.json");
