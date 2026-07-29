use devicerail_remote_auth::{AuthenticatedPrincipal, required_permission};
use thiserror::Error;

use crate::model::valid_identifier;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SecurityKind {
    RemoteAuth,
    ExternalSshOrMtlsTunnel,
}

#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum SecurityError {
    #[error("peer authentication is required")]
    AuthenticationRequired,
    #[error("authenticated peer lacks control permission")]
    ControlPermissionRequired,
    #[error("external tunnel attestation is invalid")]
    InvalidTunnelAttestation,
}

/// Non-secret attestation attached to an already established peer stream.
///
/// `RemoteAuth` values can only be constructed from a principal returned by
/// `devicerail-remote-auth`. `ExternalSshOrMtlsTunnel` records the bounded
/// local tunnel identifier configured by the operator; it does not claim that
/// this crate implemented or verified SSH/TLS itself.
#[derive(Clone, PartialEq, Eq)]
pub struct PeerSecurity {
    kind: SecurityKind,
    subject: String,
}

impl std::fmt::Debug for PeerSecurity {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("PeerSecurity")
            .field("kind", &self.kind)
            .field("subject", &self.subject)
            .finish()
    }
}

impl PeerSecurity {
    pub fn remote_auth(principal: &AuthenticatedPrincipal) -> Result<Self, SecurityError> {
        let required =
            required_permission("device.execute").ok_or(SecurityError::AuthenticationRequired)?;
        if !principal.allows(required) {
            return Err(SecurityError::ControlPermissionRequired);
        }
        Ok(Self {
            kind: SecurityKind::RemoteAuth,
            subject: principal.id().to_owned(),
        })
    }

    pub fn external_tunnel(tunnel_id: impl Into<String>) -> Result<Self, SecurityError> {
        let tunnel_id = tunnel_id.into();
        if !valid_identifier(&tunnel_id, 64) {
            return Err(SecurityError::InvalidTunnelAttestation);
        }
        Ok(Self {
            kind: SecurityKind::ExternalSshOrMtlsTunnel,
            subject: tunnel_id,
        })
    }

    pub fn kind(&self) -> SecurityKind {
        self.kind
    }

    pub fn subject(&self) -> &str {
        &self.subject
    }
}

#[cfg(test)]
mod tests {
    #[cfg(unix)]
    #[test]
    fn remote_auth_principal_must_have_control_permission() {
        use std::{fs, os::unix::fs::PermissionsExt as _, sync::Arc, time::Instant};

        use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
        use devicerail_remote_auth::{
            AUTH_PROTOCOL_VERSION, AuthChallengeRequest, AuthProofRequest, Authenticator,
            CredentialStore, compute_proof,
        };

        fn authenticate(
            authenticator: Arc<Authenticator>,
            principal_id: &str,
            secret: &[u8],
        ) -> devicerail_remote_auth::AuthenticatedPrincipal {
            let client_nonce = URL_SAFE_NO_PAD.encode([3_u8; 32]);
            let now = Instant::now();
            let mut session = authenticator.session();
            let challenge = session
                .begin(
                    AuthChallengeRequest {
                        auth_protocol_version: AUTH_PROTOCOL_VERSION.into(),
                        principal_id: principal_id.into(),
                        key_id: "key-1".into(),
                        client_nonce: client_nonce.clone(),
                    },
                    now,
                )
                .expect("challenge");
            let proof = compute_proof(secret, principal_id, "key-1", &client_nonce, &challenge)
                .expect("proof");
            session
                .finish(
                    AuthProofRequest {
                        auth_protocol_version: AUTH_PROTOCOL_VERSION.into(),
                        challenge_id: challenge.challenge_id,
                        proof,
                    },
                    now,
                )
                .expect("authenticated")
        }

        let secret = [9_u8; 32];
        let root = tempfile::tempdir().expect("tempdir");
        let path = root.path().join("credentials.json");
        fs::write(
            &path,
            serde_json::json!({
                "schemaVersion": 1,
                "principals": [
                    {
                        "principalId": "controller",
                        "keyId": "key-1",
                        "secretBase64": URL_SAFE_NO_PAD.encode(secret),
                        "permissions": ["control"]
                    },
                    {
                        "principalId": "reader",
                        "keyId": "key-1",
                        "secretBase64": URL_SAFE_NO_PAD.encode(secret),
                        "permissions": ["read"]
                    }
                ]
            })
            .to_string(),
        )
        .expect("credentials");
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).expect("permissions");
        let authenticator = Arc::new(
            Authenticator::new(CredentialStore::load(&path).expect("store"))
                .expect("authenticator"),
        );
        let controller = authenticate(Arc::clone(&authenticator), "controller", &secret);
        assert_eq!(
            super::PeerSecurity::remote_auth(&controller)
                .expect("control principal")
                .kind(),
            super::SecurityKind::RemoteAuth
        );
        let reader = authenticate(authenticator, "reader", &secret);
        assert_eq!(
            super::PeerSecurity::remote_auth(&reader),
            Err(super::SecurityError::ControlPermissionRequired)
        );
    }
}
