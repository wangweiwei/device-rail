use std::{
    fs::{File, OpenOptions},
    io::{BufRead as _, BufReader, Read as _, Seek as _, SeekFrom, Write as _},
    path::Path,
    sync::Mutex,
};

#[cfg(unix)]
use fs2::FileExt as _;
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};
use thiserror::Error;

use crate::{Permission, credentials::valid_identifier};

const MAX_AUDIT_BYTES: u64 = 64 * 1024 * 1024;
const MAX_RECORD_BYTES: usize = 16 * 1024;
const ZERO_HASH: &str = "0000000000000000000000000000000000000000000000000000000000000000";
const MAX_SAFE_INTEGER: u64 = 9_007_199_254_740_991;

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum AuditDecision {
    Allowed,
    Denied,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum AuditOutcome {
    Succeeded,
    Failed,
}

/// The audit v1 record is deliberately an admission log, not an application
/// terminal-result log. Keeping the stage fixed on the wire prevents an
/// `allowed/succeeded` authorization decision from being mistaken for proof
/// that the subsequently dispatched RPC also succeeded.
#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum AuditStage {
    SecurityAdmission,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AuditEvent {
    pub at_ms: u64,
    pub connection_id: String,
    pub principal_id: Option<String>,
    pub method: String,
    pub required_permission: Option<Permission>,
    pub decision: AuditDecision,
    pub outcome: AuditOutcome,
    pub error_code: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AuditRecord {
    pub schema_version: u8,
    pub sequence: u64,
    pub at_ms: u64,
    pub connection_id: String,
    pub principal_id: Option<String>,
    pub stage: AuditStage,
    pub method: String,
    pub required_permission: Option<Permission>,
    pub decision: AuditDecision,
    pub outcome: AuditOutcome,
    pub error_code: Option<String>,
    pub previous_hash: String,
    pub entry_hash: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct UnsignedRecord<'a> {
    schema_version: u8,
    sequence: u64,
    at_ms: u64,
    connection_id: &'a str,
    principal_id: &'a Option<String>,
    stage: AuditStage,
    method: &'a str,
    required_permission: Option<Permission>,
    decision: AuditDecision,
    outcome: AuditOutcome,
    error_code: &'a Option<String>,
    previous_hash: &'a str,
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum AuditError {
    #[error("audit log parent or file is unsafe")]
    UnsafeFile,
    #[error("owner-only audit permissions cannot be verified on this platform")]
    PermissionsUnsupported,
    #[error("audit log is already in use")]
    Busy,
    #[error("audit log exceeds its size limit")]
    TooLarge,
    #[error("audit log hash chain is corrupt")]
    Corrupt,
    #[error("audit event is invalid")]
    InvalidEvent,
    #[error("audit log I/O failed")]
    Io,
}

impl AuditError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::UnsafeFile => "audit_file_unsafe",
            Self::PermissionsUnsupported => "audit_permissions_unsupported",
            Self::Busy => "audit_store_busy",
            Self::TooLarge => "audit_store_limit",
            Self::Corrupt => "audit_chain_corrupt",
            Self::InvalidEvent => "audit_event_invalid",
            Self::Io => "audit_io_failed",
        }
    }
}

struct AuditState {
    file: File,
    next_sequence: u64,
    previous_hash: String,
    bytes: u64,
    poisoned: bool,
}

pub struct AuditLog {
    state: Mutex<AuditState>,
}

impl std::fmt::Debug for AuditLog {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        formatter
            .debug_struct("AuditLog")
            .field("next_sequence", &state.next_sequence)
            .field("bytes", &state.bytes)
            .finish()
    }
}

impl AuditLog {
    pub fn open(path: impl AsRef<Path>) -> Result<Self, AuditError> {
        let path = path.as_ref();
        let parent = path.parent().ok_or(AuditError::UnsafeFile)?;
        let parent_metadata = parent
            .symlink_metadata()
            .map_err(|_| AuditError::UnsafeFile)?;
        if parent_metadata.file_type().is_symlink() || !parent_metadata.is_dir() {
            return Err(AuditError::UnsafeFile);
        }
        #[cfg(not(unix))]
        {
            let _ = path;
            Err(AuditError::PermissionsUnsupported)
        }
        #[cfg(unix)]
        {
            require_owner_only(&parent_metadata)?;
            require_acl_path(parent)?;
            let before = match path.symlink_metadata() {
                Ok(metadata) => {
                    if metadata.file_type().is_symlink() || !metadata.is_file() {
                        return Err(AuditError::UnsafeFile);
                    }
                    require_owner_only(&metadata)?;
                    require_acl_path(path)?;
                    Some(metadata)
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
                Err(_) => return Err(AuditError::UnsafeFile),
            };
            let mut file = open_owner_only(path)?;
            file.try_lock_exclusive()
                .map_err(|error| match error.kind() {
                    std::io::ErrorKind::WouldBlock => AuditError::Busy,
                    _ => AuditError::Io,
                })?;
            let metadata = file.metadata().map_err(|_| AuditError::UnsafeFile)?;
            if !metadata.is_file() {
                return Err(AuditError::UnsafeFile);
            }
            require_owner_only(&metadata)?;
            require_acl_file(&file)?;
            if let Some(before) = &before {
                require_same_file(before, &file)?;
            }
            if metadata.len() > MAX_AUDIT_BYTES {
                return Err(AuditError::TooLarge);
            }
            let (next_sequence, previous_hash) = verify_reader(&mut file, metadata.len())?;
            file.seek(SeekFrom::End(0)).map_err(|_| AuditError::Io)?;
            Ok(Self {
                state: Mutex::new(AuditState {
                    file,
                    next_sequence,
                    previous_hash,
                    bytes: metadata.len(),
                    poisoned: false,
                }),
            })
        }
    }

    pub fn append(&self, event: AuditEvent) -> Result<AuditRecord, AuditError> {
        validate_event(&event)?;
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if state.poisoned {
            return Err(AuditError::Io);
        }
        let sequence = state.next_sequence;
        if sequence > MAX_SAFE_INTEGER {
            return Err(AuditError::TooLarge);
        }
        let entry_hash = hash_unsigned(sequence, &event, &state.previous_hash)?;
        let record = AuditRecord {
            schema_version: 1,
            sequence,
            at_ms: event.at_ms,
            connection_id: event.connection_id,
            principal_id: event.principal_id,
            stage: AuditStage::SecurityAdmission,
            method: event.method,
            required_permission: event.required_permission,
            decision: event.decision,
            outcome: event.outcome,
            error_code: event.error_code,
            previous_hash: state.previous_hash.clone(),
            entry_hash: entry_hash.clone(),
        };
        let mut bytes = canonical_json(&record)?;
        bytes.push(b'\n');
        if bytes.len() > MAX_RECORD_BYTES
            || state.bytes.saturating_add(bytes.len() as u64) > MAX_AUDIT_BYTES
        {
            return Err(AuditError::TooLarge);
        }
        if state.file.write_all(&bytes).is_err() || state.file.sync_data().is_err() {
            state.poisoned = true;
            return Err(AuditError::Io);
        }
        state.bytes += bytes.len() as u64;
        state.next_sequence += 1;
        state.previous_hash = entry_hash;
        Ok(record)
    }

    pub fn verify(path: impl AsRef<Path>) -> Result<Vec<AuditRecord>, AuditError> {
        let path = path.as_ref();
        let parent = path.parent().ok_or(AuditError::UnsafeFile)?;
        let parent_metadata = parent
            .symlink_metadata()
            .map_err(|_| AuditError::UnsafeFile)?;
        if parent_metadata.file_type().is_symlink() || !parent_metadata.is_dir() {
            return Err(AuditError::UnsafeFile);
        }
        require_owner_only(&parent_metadata)?;
        require_acl_path(parent)?;
        let metadata = path
            .symlink_metadata()
            .map_err(|_| AuditError::UnsafeFile)?;
        if metadata.file_type().is_symlink() || !metadata.is_file() {
            return Err(AuditError::UnsafeFile);
        }
        require_owner_only(&metadata)?;
        require_acl_path(path)?;
        if metadata.len() > MAX_AUDIT_BYTES {
            return Err(AuditError::TooLarge);
        }
        let mut options = OpenOptions::new();
        options.read(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt as _;
            options.custom_flags(rustix::fs::OFlags::NOFOLLOW.bits() as i32);
        }
        let mut file = options.open(path).map_err(|_| AuditError::UnsafeFile)?;
        require_same_file(&metadata, &file)?;
        let (_, _, records) = verify_reader_collect(&mut file, metadata.len())?;
        Ok(records)
    }
}

fn validate_event(event: &AuditEvent) -> Result<(), AuditError> {
    if event.at_ms > MAX_SAFE_INTEGER
        || !valid_connection_id(&event.connection_id)
        || event
            .principal_id
            .as_deref()
            .is_some_and(|value| !valid_identifier(value))
        || !valid_method(&event.method)
        || event
            .error_code
            .as_deref()
            .is_some_and(|value| !valid_error_code(value))
        || (event.decision == AuditDecision::Denied && event.outcome != AuditOutcome::Failed)
    {
        return Err(AuditError::InvalidEvent);
    }
    Ok(())
}

fn valid_connection_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
}

fn valid_method(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 96
        && value.bytes().enumerate().all(|(index, byte)| {
            byte.is_ascii_lowercase()
                || (index > 0 && byte.is_ascii_digit())
                || (index > 0 && matches!(byte, b'.' | b'_'))
        })
}

fn valid_error_code(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 64
        && value.bytes().enumerate().all(|(index, byte)| {
            byte.is_ascii_lowercase()
                || (index > 0 && byte.is_ascii_digit())
                || (index > 0 && byte == b'_')
        })
}

fn canonical_json<T: Serialize>(value: &T) -> Result<Vec<u8>, AuditError> {
    serde_json::to_vec(value).map_err(|_| AuditError::Io)
}

fn hash_unsigned(
    sequence: u64,
    event: &AuditEvent,
    previous_hash: &str,
) -> Result<String, AuditError> {
    let unsigned = UnsignedRecord {
        schema_version: 1,
        sequence,
        at_ms: event.at_ms,
        connection_id: &event.connection_id,
        principal_id: &event.principal_id,
        stage: AuditStage::SecurityAdmission,
        method: &event.method,
        required_permission: event.required_permission,
        decision: event.decision,
        outcome: event.outcome,
        error_code: &event.error_code,
        previous_hash,
    };
    let mut hasher = Sha256::new();
    hasher.update(b"devicerail.audit.v1\0");
    hasher.update(canonical_json(&unsigned)?);
    Ok(hex::encode(hasher.finalize()))
}

#[cfg(unix)]
fn verify_reader(file: &mut File, bytes: u64) -> Result<(u64, String), AuditError> {
    let (sequence, hash, _) = verify_reader_collect(file, bytes)?;
    Ok((sequence, hash))
}

fn verify_reader_collect(
    file: &mut File,
    bytes: u64,
) -> Result<(u64, String, Vec<AuditRecord>), AuditError> {
    file.seek(SeekFrom::Start(0)).map_err(|_| AuditError::Io)?;
    let mut reader = BufReader::new(file);
    let mut consumed = 0_u64;
    let mut expected_sequence = 1_u64;
    let mut previous_hash = ZERO_HASH.to_owned();
    let mut records = Vec::new();
    loop {
        let mut line = Vec::new();
        let read = reader
            .by_ref()
            .take((MAX_RECORD_BYTES + 1) as u64)
            .read_until(b'\n', &mut line)
            .map_err(|_| AuditError::Io)?;
        if read == 0 {
            break;
        }
        consumed += read as u64;
        if read > MAX_RECORD_BYTES || line.last() != Some(&b'\n') {
            return Err(AuditError::Corrupt);
        }
        line.pop();
        let record: AuditRecord = serde_json::from_slice(&line).map_err(|_| AuditError::Corrupt)?;
        if record.schema_version != 1
            || record.sequence != expected_sequence
            || record.previous_hash != previous_hash
            || !is_hash(&record.entry_hash)
        {
            return Err(AuditError::Corrupt);
        }
        let event = AuditEvent {
            at_ms: record.at_ms,
            connection_id: record.connection_id.clone(),
            principal_id: record.principal_id.clone(),
            method: record.method.clone(),
            required_permission: record.required_permission,
            decision: record.decision,
            outcome: record.outcome,
            error_code: record.error_code.clone(),
        };
        validate_event(&event).map_err(|_| AuditError::Corrupt)?;
        if hash_unsigned(record.sequence, &event, &record.previous_hash)? != record.entry_hash {
            return Err(AuditError::Corrupt);
        }
        previous_hash = record.entry_hash.clone();
        expected_sequence += 1;
        records.push(record);
    }
    if consumed != bytes {
        return Err(AuditError::Corrupt);
    }
    Ok((expected_sequence, previous_hash, records))
}

fn is_hash(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
}

#[cfg(unix)]
fn open_owner_only(path: &Path) -> Result<File, AuditError> {
    use std::os::unix::fs::OpenOptionsExt as _;

    let mut options = OpenOptions::new();
    options
        .read(true)
        .append(true)
        .create(true)
        .mode(0o600)
        .custom_flags(rustix::fs::OFlags::NOFOLLOW.bits() as i32);
    options.open(path).map_err(|_| AuditError::UnsafeFile)
}

#[cfg(unix)]
fn require_owner_only(metadata: &std::fs::Metadata) -> Result<(), AuditError> {
    use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};

    if metadata.permissions().mode() & 0o077 != 0
        || metadata.uid() != rustix::process::geteuid().as_raw()
    {
        return Err(AuditError::UnsafeFile);
    }
    Ok(())
}

#[cfg(unix)]
fn require_same_file(before: &std::fs::Metadata, file: &File) -> Result<(), AuditError> {
    use std::os::unix::fs::MetadataExt as _;

    let opened = file.metadata().map_err(|_| AuditError::UnsafeFile)?;
    require_owner_only(&opened)?;
    require_acl_file(file)?;
    if opened.dev() != before.dev()
        || opened.ino() != before.ino()
        || opened.len() != before.len()
        || opened.mtime() != before.mtime()
        || opened.mtime_nsec() != before.mtime_nsec()
    {
        return Err(AuditError::UnsafeFile);
    }
    Ok(())
}

#[cfg(unix)]
fn require_acl_path(path: &Path) -> Result<(), AuditError> {
    crate::owner_only::require_no_extended_acl_path(path).map_err(|error| match error {
        #[cfg(target_os = "macos")]
        crate::owner_only::ExtendedAclError::Present => AuditError::UnsafeFile,
        #[cfg(target_os = "macos")]
        crate::owner_only::ExtendedAclError::Unavailable => AuditError::PermissionsUnsupported,
    })
}

#[cfg(not(unix))]
fn require_acl_path(_path: &Path) -> Result<(), AuditError> {
    Err(AuditError::PermissionsUnsupported)
}

#[cfg(unix)]
fn require_acl_file(file: &File) -> Result<(), AuditError> {
    crate::owner_only::require_no_extended_acl_file(file).map_err(|error| match error {
        #[cfg(target_os = "macos")]
        crate::owner_only::ExtendedAclError::Present => AuditError::UnsafeFile,
        #[cfg(target_os = "macos")]
        crate::owner_only::ExtendedAclError::Unavailable => AuditError::PermissionsUnsupported,
    })
}

#[cfg(not(unix))]
fn require_owner_only(_metadata: &std::fs::Metadata) -> Result<(), AuditError> {
    Err(AuditError::PermissionsUnsupported)
}

#[cfg(not(unix))]
fn require_same_file(_before: &std::fs::Metadata, _file: &File) -> Result<(), AuditError> {
    Err(AuditError::PermissionsUnsupported)
}
