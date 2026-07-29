use std::fs;
#[cfg(unix)]
use std::{
    path::Path,
    sync::Arc,
    time::{Duration, Instant},
};

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use devicerail_remote_auth::{
    AUDIT_RECORD_SCHEMA, AUTH_PROTOCOL_SCHEMA, AuditError, AuditLog, CREDENTIAL_STORE_SCHEMA,
    CredentialError, CredentialStore, Permission, compute_proof, required_permission,
};
#[cfg(unix)]
use devicerail_remote_auth::{
    AuditDecision, AuditEvent, AuditOutcome, AuditStage, AuthChallengeRequest, AuthError,
    AuthProofRequest, Authenticator, ChallengeConfig,
};
use serde_json::json;
use tempfile::TempDir;

const SECRET: [u8; 32] = [0x2a; 32];

fn credential_json(secret: &[u8], permission: &str) -> Vec<u8> {
    serde_json::to_vec(&json!({
        "schemaVersion": 1,
        "principals": [{
            "principalId": "test-principal",
            "keyId": "test-key-1",
            "secretBase64": URL_SAFE_NO_PAD.encode(secret),
            "permissions": [permission]
        }]
    }))
    .expect("credential JSON")
}

#[cfg(unix)]
fn owner_only(path: &Path) {
    use std::os::unix::fs::PermissionsExt as _;

    fs::set_permissions(path, fs::Permissions::from_mode(0o600)).expect("owner-only mode");
}

#[cfg(unix)]
fn owner_only_directory(path: &Path) {
    use std::os::unix::fs::PermissionsExt as _;

    fs::set_permissions(path, fs::Permissions::from_mode(0o700))
        .expect("owner-only directory mode");
}

#[cfg(unix)]
fn store(root: &Path, permission: &str) -> CredentialStore {
    let path = root.join(format!("{permission}.credentials.json"));
    fs::write(&path, credential_json(&SECRET, permission)).expect("write credentials");
    owner_only(&path);
    CredentialStore::load(path).expect("load credentials")
}

#[cfg(unix)]
fn challenge_request() -> AuthChallengeRequest {
    AuthChallengeRequest {
        auth_protocol_version: "1".to_owned(),
        principal_id: "test-principal".to_owned(),
        key_id: "test-key-1".to_owned(),
        client_nonce: URL_SAFE_NO_PAD.encode([7_u8; 32]),
    }
}

#[cfg(unix)]
#[test]
fn hmac_authentication_is_single_use_expiring_and_generic_on_failure() {
    let root = TempDir::new().expect("tempdir");
    let authenticator = Arc::new(Authenticator::new(store(root.path(), "control")).expect("auth"));
    let now = Instant::now();
    let mut session = authenticator.session();
    let request = challenge_request();
    let challenge = session.begin(request.clone(), now).expect("challenge");
    let proof = compute_proof(
        &SECRET,
        &request.principal_id,
        &request.key_id,
        &request.client_nonce,
        &challenge,
    )
    .expect("proof");
    let principal = session
        .finish(
            AuthProofRequest {
                auth_protocol_version: "1".into(),
                challenge_id: challenge.challenge_id.clone(),
                proof,
            },
            now + Duration::from_millis(1),
        )
        .expect("authenticate");
    assert_eq!(principal.id(), "test-principal");
    assert!(principal.allows(Permission::Read));
    assert!(principal.allows(Permission::Control));
    assert!(!principal.allows(Permission::Admin));
    assert_eq!(
        session
            .finish(
                AuthProofRequest {
                    auth_protocol_version: "1".into(),
                    challenge_id: challenge.challenge_id,
                    proof: URL_SAFE_NO_PAD.encode([0_u8; 32]),
                },
                now,
            )
            .expect_err("replay must fail"),
        AuthError::AuthenticationFailed
    );

    let mut expired = authenticator.session();
    let challenge = expired
        .begin(challenge_request(), now)
        .expect("expiring challenge");
    let proof = compute_proof(
        &SECRET,
        "test-principal",
        "test-key-1",
        &challenge_request().client_nonce,
        &challenge,
    )
    .expect("proof");
    assert_eq!(
        expired
            .finish(
                AuthProofRequest {
                    auth_protocol_version: "1".into(),
                    challenge_id: challenge.challenge_id,
                    proof,
                },
                now + Duration::from_secs(11),
            )
            .expect_err("expired challenge"),
        AuthError::AuthenticationFailed
    );

    let mut unknown = authenticator.session();
    let mut unknown_request = challenge_request();
    unknown_request.principal_id = "unknown-principal".into();
    let challenge = unknown
        .begin(unknown_request, now)
        .expect("generic challenge");
    let error = unknown
        .finish(
            AuthProofRequest {
                auth_protocol_version: "1".into(),
                challenge_id: challenge.challenge_id,
                proof: URL_SAFE_NO_PAD.encode([0_u8; 32]),
            },
            now,
        )
        .expect_err("unknown identity");
    assert_eq!(error, AuthError::AuthenticationFailed);
}

#[cfg(unix)]
#[test]
fn challenge_attempts_and_configuration_are_bounded() {
    let root = TempDir::new().expect("tempdir");
    assert!(ChallengeConfig::new(Duration::from_millis(99), 3).is_err());
    assert!(ChallengeConfig::new(Duration::from_secs(31), 3).is_err());
    assert!(ChallengeConfig::new(Duration::from_secs(1), 0).is_err());
    let authenticator = Arc::new(
        Authenticator::with_config(
            store(root.path(), "read"),
            ChallengeConfig::new(Duration::from_secs(1), 1).expect("config"),
        )
        .expect("auth"),
    );
    let mut session = authenticator.session();
    let challenge = session
        .begin(challenge_request(), Instant::now())
        .expect("challenge");
    assert_eq!(
        session
            .begin(challenge_request(), Instant::now())
            .expect_err("pending challenge"),
        AuthError::ChallengePending
    );
    session
        .finish(
            AuthProofRequest {
                auth_protocol_version: "1".into(),
                challenge_id: challenge.challenge_id,
                proof: URL_SAFE_NO_PAD.encode([0_u8; 32]),
            },
            Instant::now(),
        )
        .expect_err("wrong proof");
    assert_eq!(
        session
            .begin(challenge_request(), Instant::now())
            .expect_err("attempt limit"),
        AuthError::AttemptsExceeded
    );
}

#[cfg(not(unix))]
#[test]
fn credential_and_audit_storage_fail_closed_without_owner_only_verification() {
    let root = TempDir::new().expect("tempdir");
    let credential_path = root.path().join("credentials.json");
    fs::write(&credential_path, credential_json(&SECRET, "read")).expect("write credentials");
    assert_eq!(
        CredentialStore::load(credential_path).expect_err("permissions cannot be verified"),
        CredentialError::PermissionsUnsupported
    );
    assert_eq!(
        AuditLog::open(root.path().join("audit.jsonl"))
            .expect_err("permissions cannot be verified"),
        AuditError::PermissionsUnsupported
    );
}

#[cfg(unix)]
#[test]
fn credential_store_rejects_open_permissions_symlinks_and_secret_debug() {
    use std::os::unix::fs::{PermissionsExt as _, symlink};

    let root = TempDir::new().expect("tempdir");
    let path = root.path().join("credentials.json");
    fs::write(&path, credential_json(&SECRET, "read")).expect("write");
    fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).expect("mode");
    assert_eq!(
        CredentialStore::load(&path).expect_err("open permissions"),
        CredentialError::UnsafeFile
    );
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).expect("mode");
    let loaded = CredentialStore::load(&path).expect("owner-only credentials");
    let debug = format!("{loaded:?}");
    assert!(!debug.contains(&URL_SAFE_NO_PAD.encode(SECRET)));
    let link = root.path().join("credentials-link.json");
    symlink(&path, &link).expect("symlink");
    assert_eq!(
        CredentialStore::load(link).expect_err("symlink"),
        CredentialError::UnsafeFile
    );
}

#[cfg(target_os = "macos")]
#[test]
fn credential_store_rejects_extended_acl_despite_private_mode_bits() {
    use std::{os::unix::fs::PermissionsExt as _, process::Command};

    let root = TempDir::new().expect("tempdir");
    let path = root.path().join("credentials.json");
    fs::write(&path, credential_json(&SECRET, "read")).expect("write");
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).expect("private mode");
    let status = Command::new("chmod")
        .args(["+a", "everyone allow read"])
        .arg(&path)
        .status()
        .expect("chmod ACL");
    assert!(status.success());
    assert_eq!(
        CredentialStore::load(&path).expect_err("extended ACL"),
        CredentialError::UnsafeFile
    );
}

#[cfg(unix)]
#[test]
fn audit_chain_recovers_rejects_corruption_and_never_stores_params() {
    let root = TempDir::new().expect("tempdir");
    owner_only_directory(root.path());
    let path = root.path().join("audit.jsonl");
    let log = AuditLog::open(&path).expect("create audit");
    let first = log
        .append(AuditEvent {
            at_ms: 1,
            connection_id: "connection-1".into(),
            principal_id: Some("test-principal".into()),
            method: "system.hello".into(),
            required_permission: Some(Permission::Read),
            decision: AuditDecision::Allowed,
            outcome: AuditOutcome::Succeeded,
            error_code: None,
        })
        .expect("first record");
    let second = log
        .append(AuditEvent {
            at_ms: 2,
            connection_id: "connection-1".into(),
            principal_id: Some("test-principal".into()),
            method: "device.execute".into(),
            required_permission: Some(Permission::Control),
            decision: AuditDecision::Denied,
            outcome: AuditOutcome::Failed,
            error_code: Some("permission_denied".into()),
        })
        .expect("second record");
    assert_eq!(second.previous_hash, first.entry_hash);
    assert_eq!(first.stage, AuditStage::SecurityAdmission);
    assert_eq!(second.stage, AuditStage::SecurityAdmission);
    assert_eq!(
        AuditLog::open(&path).expect_err("exclusive writer lock"),
        AuditError::Busy
    );
    drop(log);
    assert_eq!(AuditLog::verify(&path).expect("verify").len(), 2);
    let reopened = AuditLog::open(&path).expect("recover chain");
    let third = reopened
        .append(AuditEvent {
            at_ms: 3,
            connection_id: "connection-2".into(),
            principal_id: None,
            method: "auth.respond".into(),
            required_permission: None,
            decision: AuditDecision::Denied,
            outcome: AuditOutcome::Failed,
            error_code: Some("authentication_failed".into()),
        })
        .expect("continued record");
    assert_eq!(third.sequence, 3);
    drop(reopened);
    let bytes = fs::read(&path).expect("audit bytes");
    assert!(!bytes.windows(6).any(|window| window == b"params"));
    assert!(!bytes.windows(6).any(|window| window == b"secret"));
    let mut text = String::from_utf8(bytes).expect("UTF-8 audit");
    text = text.replacen("test-principal", "evil-principal", 1);
    fs::write(&path, text).expect("tamper");
    owner_only(&path);
    assert_eq!(
        AuditLog::verify(&path).expect_err("tampered chain"),
        AuditError::Corrupt
    );
}

#[cfg(unix)]
#[test]
fn audit_rejects_a_group_or_world_accessible_parent_directory() {
    use std::os::unix::fs::PermissionsExt as _;

    let root = TempDir::new().expect("tempdir");
    let unsafe_parent = root.path().join("unsafe-audit-parent");
    fs::create_dir(&unsafe_parent).expect("create audit parent");
    fs::set_permissions(&unsafe_parent, fs::Permissions::from_mode(0o777))
        .expect("make audit parent unsafe");
    assert_eq!(
        AuditLog::open(unsafe_parent.join("audit.jsonl")).expect_err("unsafe parent"),
        AuditError::UnsafeFile
    );
}

#[cfg(target_os = "macos")]
#[test]
fn audit_rejects_extended_acl_on_private_parent_directory() {
    use std::{os::unix::fs::PermissionsExt as _, process::Command};

    let root = TempDir::new().expect("tempdir");
    let parent = root.path().join("audit-parent");
    fs::create_dir(&parent).expect("create audit parent");
    fs::set_permissions(&parent, fs::Permissions::from_mode(0o700)).expect("private mode");
    let status = Command::new("chmod")
        .args(["+a", "everyone allow read"])
        .arg(&parent)
        .status()
        .expect("chmod ACL");
    assert!(status.success());
    assert_eq!(
        AuditLog::open(parent.join("audit.jsonl")).expect_err("extended ACL"),
        AuditError::UnsafeFile
    );
}

#[cfg(target_os = "macos")]
#[test]
fn audit_verify_rejects_extended_acl_on_private_log_file() {
    use std::process::Command;

    let root = TempDir::new().expect("tempdir");
    owner_only_directory(root.path());
    let path = root.path().join("audit.jsonl");
    let log = AuditLog::open(&path).expect("create audit");
    drop(log);
    let status = Command::new("chmod")
        .args(["+a", "everyone allow read"])
        .arg(&path)
        .status()
        .expect("chmod ACL");
    assert!(status.success());
    assert_eq!(
        AuditLog::verify(&path).expect_err("extended ACL"),
        AuditError::UnsafeFile
    );
}

#[cfg(unix)]
#[test]
fn audit_rejects_an_owner_only_non_regular_path() {
    use std::os::unix::fs::PermissionsExt as _;

    let root = TempDir::new().expect("tempdir");
    owner_only_directory(root.path());
    let path = root.path().join("audit.directory");
    fs::create_dir(&path).expect("create non-regular audit path");
    fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).expect("owner-only directory");
    assert_eq!(
        AuditLog::open(&path).expect_err("non-regular audit path"),
        AuditError::UnsafeFile
    );
}

#[test]
fn checked_in_schemas_accept_fixtures_and_authorization_fails_closed() {
    let auth_schema: serde_json::Value =
        serde_json::from_str(AUTH_PROTOCOL_SCHEMA).expect("auth schema");
    let auth_validator = jsonschema::validator_for(&auth_schema).expect("auth validator");
    for fixture in [
        include_str!("../fixtures/challenge-request.json"),
        include_str!("../fixtures/challenge.json"),
        include_str!("../fixtures/proof-request.json"),
        include_str!("../fixtures/success.json"),
    ] {
        let value: serde_json::Value = serde_json::from_str(fixture).expect("auth fixture");
        assert!(
            auth_validator.is_valid(&value),
            "{:#?}",
            auth_validator.iter_errors(&value).collect::<Vec<_>>()
        );
    }
    let credential_schema: serde_json::Value =
        serde_json::from_str(CREDENTIAL_STORE_SCHEMA).expect("credential schema");
    let credential_validator =
        jsonschema::validator_for(&credential_schema).expect("credential validator");
    let credential_fixture: serde_json::Value =
        serde_json::from_str(include_str!("../fixtures/credential-store.json"))
            .expect("credential fixture");
    assert!(credential_validator.is_valid(&credential_fixture));
    let fixture_challenge: devicerail_remote_auth::AuthChallenge =
        serde_json::from_str(include_str!("../fixtures/challenge.json"))
            .expect("fixture challenge DTO");
    let fixture_request: devicerail_remote_auth::AuthChallengeRequest =
        serde_json::from_str(include_str!("../fixtures/challenge-request.json"))
            .expect("fixture request DTO");
    let fixture_proof: devicerail_remote_auth::AuthProofRequest =
        serde_json::from_str(include_str!("../fixtures/proof-request.json"))
            .expect("fixture proof DTO");
    assert_eq!(
        compute_proof(
            &[0_u8; 32],
            &fixture_request.principal_id,
            &fixture_request.key_id,
            &fixture_request.client_nonce,
            &fixture_challenge,
        )
        .expect("fixture HMAC"),
        fixture_proof.proof
    );
    let audit_schema: serde_json::Value =
        serde_json::from_str(AUDIT_RECORD_SCHEMA).expect("audit schema");
    let audit_validator = jsonschema::validator_for(&audit_schema).expect("audit validator");
    let audit_fixture: serde_json::Value =
        serde_json::from_str(include_str!("../fixtures/audit-record.jsonl"))
            .expect("audit fixture");
    assert!(audit_validator.is_valid(&audit_fixture));
    #[cfg(unix)]
    {
        let root = TempDir::new().expect("audit fixture tempdir");
        owner_only_directory(root.path());
        let path = root.path().join("fixture.jsonl");
        fs::write(&path, include_bytes!("../fixtures/audit-record.jsonl"))
            .expect("write audit fixture");
        owner_only(&path);
        assert_eq!(AuditLog::verify(path).expect("fixture hash chain").len(), 1);
    }

    assert_eq!(required_permission("events.clear"), Some(Permission::Admin));
    assert_eq!(
        required_permission("ui.snapshot.get"),
        Some(Permission::Read)
    );
    assert_eq!(
        required_permission("verdict.record"),
        Some(Permission::Control)
    );
    assert_eq!(required_permission("unknown.future"), None);
}
