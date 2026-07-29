use std::{sync::Arc, time::Duration};

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use getrandom::fill as random_fill;
use hmac::{Hmac, KeyInit as _, Mac as _};
use serde::{Deserialize, Serialize};
use sha2::Sha256;
use thiserror::Error;
use zeroize::Zeroizing;

use crate::{AuthenticatedPrincipal, CredentialStore, credentials::valid_identifier};

pub const AUTH_PROTOCOL_VERSION: &str = "1";
const CLIENT_NONCE_BYTES: usize = 32;
const SERVER_NONCE_BYTES: usize = 32;
const CHALLENGE_ID_BYTES: usize = 16;
const PROOF_BYTES: usize = 32;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ChallengeConfig {
    ttl: Duration,
    max_attempts: u8,
}

impl ChallengeConfig {
    pub fn new(ttl: Duration, max_attempts: u8) -> Result<Self, AuthError> {
        if ttl < Duration::from_millis(100)
            || ttl > Duration::from_secs(30)
            || !(1..=5).contains(&max_attempts)
        {
            return Err(AuthError::InvalidConfiguration);
        }
        Ok(Self { ttl, max_attempts })
    }

    pub fn ttl(&self) -> Duration {
        self.ttl
    }
}

impl Default for ChallengeConfig {
    fn default() -> Self {
        Self {
            ttl: Duration::from_secs(10),
            max_attempts: 3,
        }
    }
}

#[derive(Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AuthChallengeRequest {
    pub auth_protocol_version: String,
    pub principal_id: String,
    pub key_id: String,
    pub client_nonce: String,
}

impl std::fmt::Debug for AuthChallengeRequest {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("AuthChallengeRequest")
            .field("auth_protocol_version", &self.auth_protocol_version)
            .field("principal_id", &self.principal_id)
            .field("key_id", &self.key_id)
            .field("client_nonce", &"[REDACTED]")
            .finish()
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AuthChallenge {
    pub auth_protocol_version: String,
    pub algorithm: String,
    pub challenge_id: String,
    pub server_nonce: String,
    pub expires_in_ms: u64,
}

#[derive(Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AuthProofRequest {
    pub auth_protocol_version: String,
    pub challenge_id: String,
    pub proof: String,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AuthSuccess {
    pub auth_protocol_version: String,
    pub authenticated: bool,
    pub principal_id: String,
    pub permissions: Vec<crate::Permission>,
}

impl AuthSuccess {
    pub fn from_principal(principal: &AuthenticatedPrincipal) -> Self {
        Self {
            auth_protocol_version: AUTH_PROTOCOL_VERSION.to_owned(),
            authenticated: true,
            principal_id: principal.id().to_owned(),
            permissions: principal.permissions().iter().copied().collect(),
        }
    }
}

impl std::fmt::Debug for AuthProofRequest {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("AuthProofRequest")
            .field("auth_protocol_version", &self.auth_protocol_version)
            .field("challenge_id", &"[REDACTED]")
            .field("proof", &"[REDACTED]")
            .finish()
    }
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum AuthError {
    #[error("authentication configuration is invalid")]
    InvalidConfiguration,
    #[error("authentication randomness is unavailable")]
    RandomUnavailable,
    #[error("authentication request is invalid")]
    InvalidRequest,
    #[error("an authentication challenge is already pending")]
    ChallengePending,
    #[error("authentication attempt limit exceeded")]
    AttemptsExceeded,
    #[error("authentication challenge is absent, expired, replayed, or invalid")]
    AuthenticationFailed,
}

impl AuthError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::InvalidConfiguration => "auth_configuration_invalid",
            Self::RandomUnavailable => "auth_random_unavailable",
            Self::InvalidRequest => "auth_request_invalid",
            Self::ChallengePending => "auth_challenge_pending",
            Self::AttemptsExceeded => "auth_attempt_limit",
            Self::AuthenticationFailed => "authentication_failed",
        }
    }
}

pub struct Authenticator {
    credentials: Arc<CredentialStore>,
    dummy_secret: Zeroizing<Vec<u8>>,
    config: ChallengeConfig,
}

impl std::fmt::Debug for Authenticator {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Authenticator")
            .field("credentials", &self.credentials)
            .field("dummy_secret", &"[REDACTED]")
            .field("config", &self.config)
            .finish()
    }
}

impl Authenticator {
    pub fn new(credentials: CredentialStore) -> Result<Self, AuthError> {
        Self::with_config(credentials, ChallengeConfig::default())
    }

    pub fn with_config(
        credentials: CredentialStore,
        config: ChallengeConfig,
    ) -> Result<Self, AuthError> {
        let mut dummy_secret = vec![0_u8; 32];
        random_fill(&mut dummy_secret).map_err(|_| AuthError::RandomUnavailable)?;
        Ok(Self {
            credentials: Arc::new(credentials),
            dummy_secret: Zeroizing::new(dummy_secret),
            config,
        })
    }

    pub fn session(self: &Arc<Self>) -> AuthSession {
        AuthSession {
            authenticator: Arc::clone(self),
            attempts: 0,
            pending: None,
        }
    }
}

pub struct AuthSession {
    authenticator: Arc<Authenticator>,
    attempts: u8,
    pending: Option<PendingChallenge>,
}

impl std::fmt::Debug for AuthSession {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("AuthSession")
            .field("attempts", &self.attempts)
            .field("has_pending_challenge", &self.pending.is_some())
            .finish()
    }
}

struct PendingChallenge {
    principal_id: String,
    key_id: String,
    client_nonce: [u8; CLIENT_NONCE_BYTES],
    server_nonce: [u8; SERVER_NONCE_BYTES],
    challenge_id: [u8; CHALLENGE_ID_BYTES],
    created_at: std::time::Instant,
}

impl AuthSession {
    pub fn begin(
        &mut self,
        request: AuthChallengeRequest,
        now: std::time::Instant,
    ) -> Result<AuthChallenge, AuthError> {
        if self.pending.is_some() {
            return Err(AuthError::ChallengePending);
        }
        if self.attempts >= self.authenticator.config.max_attempts {
            return Err(AuthError::AttemptsExceeded);
        }
        if request.auth_protocol_version != AUTH_PROTOCOL_VERSION
            || !valid_identifier(&request.principal_id)
            || !valid_identifier(&request.key_id)
        {
            return Err(AuthError::InvalidRequest);
        }
        let client_nonce = decode_canonical::<CLIENT_NONCE_BYTES>(&request.client_nonce)
            .ok_or(AuthError::InvalidRequest)?;
        self.attempts += 1;
        let mut server_nonce = [0_u8; SERVER_NONCE_BYTES];
        let mut challenge_id = [0_u8; CHALLENGE_ID_BYTES];
        random_fill(&mut server_nonce).map_err(|_| AuthError::RandomUnavailable)?;
        random_fill(&mut challenge_id).map_err(|_| AuthError::RandomUnavailable)?;
        self.pending = Some(PendingChallenge {
            principal_id: request.principal_id,
            key_id: request.key_id,
            client_nonce,
            server_nonce,
            challenge_id,
            created_at: now,
        });
        Ok(AuthChallenge {
            auth_protocol_version: AUTH_PROTOCOL_VERSION.to_owned(),
            algorithm: "HMAC-SHA256".to_owned(),
            challenge_id: URL_SAFE_NO_PAD.encode(challenge_id),
            server_nonce: URL_SAFE_NO_PAD.encode(server_nonce),
            expires_in_ms: self.authenticator.config.ttl.as_millis() as u64,
        })
    }

    pub fn finish(
        &mut self,
        request: AuthProofRequest,
        now: std::time::Instant,
    ) -> Result<AuthenticatedPrincipal, AuthError> {
        let pending = self.pending.take().ok_or(AuthError::AuthenticationFailed)?;
        let supplied_challenge = decode_canonical::<CHALLENGE_ID_BYTES>(&request.challenge_id)
            .unwrap_or([0_u8; CHALLENGE_ID_BYTES]);
        let supplied_proof =
            decode_canonical::<PROOF_BYTES>(&request.proof).unwrap_or([0_u8; PROOF_BYTES]);
        let credential = self
            .authenticator
            .credentials
            .lookup(&pending.principal_id, &pending.key_id);
        let secret = credential.map_or(self.authenticator.dummy_secret.as_slice(), |value| {
            value.secret.as_slice()
        });
        let input = proof_input(
            &pending.principal_id,
            &pending.key_id,
            &pending.client_nonce,
            &pending.server_nonce,
            &pending.challenge_id,
        );
        let mut mac = Hmac::<Sha256>::new_from_slice(secret)
            .expect("HMAC-SHA256 accepts the bounded credential lengths");
        mac.update(&input);
        let proof_matches = mac.verify_slice(&supplied_proof).is_ok();
        let challenge_matches = constant_time_eq(&supplied_challenge, &pending.challenge_id);
        let valid = request.auth_protocol_version == AUTH_PROTOCOL_VERSION
            && now.saturating_duration_since(pending.created_at) <= self.authenticator.config.ttl
            && proof_matches
            && challenge_matches;
        match (valid, credential) {
            (true, Some(credential)) => Ok(AuthenticatedPrincipal::new(
                credential.principal_id.clone(),
                credential.permissions.clone(),
            )),
            _ => Err(AuthError::AuthenticationFailed),
        }
    }
}

pub fn compute_proof(
    secret: &[u8],
    principal_id: &str,
    key_id: &str,
    client_nonce: &str,
    challenge: &AuthChallenge,
) -> Result<String, AuthError> {
    if secret.len() < 32
        || secret.len() > 64
        || !valid_identifier(principal_id)
        || !valid_identifier(key_id)
        || challenge.auth_protocol_version != AUTH_PROTOCOL_VERSION
        || challenge.algorithm != "HMAC-SHA256"
    {
        return Err(AuthError::InvalidRequest);
    }
    let client_nonce =
        decode_canonical::<CLIENT_NONCE_BYTES>(client_nonce).ok_or(AuthError::InvalidRequest)?;
    let server_nonce = decode_canonical::<SERVER_NONCE_BYTES>(&challenge.server_nonce)
        .ok_or(AuthError::InvalidRequest)?;
    let challenge_id = decode_canonical::<CHALLENGE_ID_BYTES>(&challenge.challenge_id)
        .ok_or(AuthError::InvalidRequest)?;
    let input = proof_input(
        principal_id,
        key_id,
        &client_nonce,
        &server_nonce,
        &challenge_id,
    );
    let mut mac = Hmac::<Sha256>::new_from_slice(secret).map_err(|_| AuthError::InvalidRequest)?;
    mac.update(&input);
    Ok(URL_SAFE_NO_PAD.encode(mac.finalize().into_bytes()))
}

fn proof_input(
    principal_id: &str,
    key_id: &str,
    client_nonce: &[u8; CLIENT_NONCE_BYTES],
    server_nonce: &[u8; SERVER_NONCE_BYTES],
    challenge_id: &[u8; CHALLENGE_ID_BYTES],
) -> Vec<u8> {
    let fields = [
        AUTH_PROTOCOL_VERSION.as_bytes(),
        principal_id.as_bytes(),
        key_id.as_bytes(),
        client_nonce,
        server_nonce,
        challenge_id,
    ];
    let mut input = b"devicerail.remote-auth.hmac-sha256.v1\0".to_vec();
    for field in fields {
        input.extend_from_slice(&(field.len() as u16).to_be_bytes());
        input.extend_from_slice(field);
    }
    input
}

fn decode_canonical<const N: usize>(value: &str) -> Option<[u8; N]> {
    let bytes = URL_SAFE_NO_PAD.decode(value.as_bytes()).ok()?;
    if bytes.len() != N || URL_SAFE_NO_PAD.encode(&bytes) != value {
        return None;
    }
    bytes.try_into().ok()
}

fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    if left.len() != right.len() {
        return false;
    }
    let difference = left
        .iter()
        .zip(right)
        .fold(0_u8, |difference, (left, right)| {
            difference | (left ^ right)
        });
    difference == 0
}
