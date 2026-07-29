use std::{
    collections::BTreeSet,
    fmt,
    io::Cursor,
    pin::Pin,
    str::FromStr,
    sync::{Arc, Mutex},
};

use async_trait::async_trait;
use devicerail_protocol::{
    AssetRef, ErrorInfo, MAX_UI_SNAPSHOT_BYTES, SessionId, UI_SNAPSHOT_MEDIA_TYPE, UiSnapshot,
};
use serde_json::json;
use thiserror::Error;
use tokio::io::AsyncRead;

const SHA256_HEX_LENGTH: usize = 64;
const ASSET_ID_PREFIX: &str = "sha256:";
const ASSET_URI_PREFIX: &str = "devicerail://assets/sha256/";
const MAX_MEDIA_TYPE_LENGTH: usize = 255;

/// A streaming evidence body supplied by a caller.
///
/// Keeping the stream owned and `'static` lets stores consume it after an
/// async suspension without tying the object-safe store trait to a caller
/// lifetime.
pub type EvidenceInput = Pin<Box<dyn AsyncRead + Send + 'static>>;

/// A verified evidence body returned by a store.
///
/// Implementations must validate the content against its digest before
/// returning this stream. Read failures after `open` use `std::io::Error`, as
/// required by `AsyncRead`; storage and integrity failures use
/// [`EvidenceError`] from `open` itself.
pub type EvidenceOutput = Pin<Box<dyn AsyncRead + Send + 'static>>;

pub type EvidenceResult<T> = Result<T, EvidenceError>;

/// A normalized SHA-256 digest.
///
/// The only accepted representation is exactly 64 lowercase hexadecimal
/// characters. This deliberately excludes uppercase and prefixed forms so
/// filesystem adapters and clients derive one canonical key.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Sha256Digest(String);

impl Sha256Digest {
    pub fn parse(value: impl Into<String>) -> EvidenceResult<Self> {
        let value = value.into();
        let valid = value.len() == SHA256_HEX_LENGTH
            && value
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte));
        if valid {
            Ok(Self(value))
        } else {
            Err(EvidenceError::InvalidDigest(value))
        }
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn asset_id(&self) -> String {
        format!("{ASSET_ID_PREFIX}{}", self.as_str())
    }

    pub fn asset_uri(&self) -> String {
        format!("{ASSET_URI_PREFIX}{}", self.as_str())
    }

    pub fn from_asset_ref(asset: &AssetRef) -> EvidenceResult<Self> {
        let raw_digest = asset
            .sha256
            .as_deref()
            .ok_or_else(|| EvidenceError::InvalidReference("sha256 is required".to_owned()))?;
        let digest = Self::parse(raw_digest.to_owned()).map_err(|_| {
            EvidenceError::InvalidReference("sha256 is not canonical lowercase hex".to_owned())
        })?;
        if asset.id != digest.asset_id() {
            return Err(EvidenceError::InvalidReference(
                "id does not match sha256".to_owned(),
            ));
        }
        if asset.uri != digest.asset_uri() {
            return Err(EvidenceError::InvalidReference(
                "uri is not the canonical DeviceRail asset URI".to_owned(),
            ));
        }
        validate_media_type(&asset.media_type)?;
        Ok(digest)
    }
}

impl fmt::Display for Sha256Digest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl FromStr for Sha256Digest {
    type Err = EvidenceError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::parse(value)
    }
}

impl TryFrom<String> for Sha256Digest {
    type Error = EvidenceError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::parse(value)
    }
}

impl TryFrom<&str> for Sha256Digest {
    type Error = EvidenceError;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        Self::parse(value)
    }
}

/// Metadata attached to one content-addressed evidence object.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EvidenceMetadata {
    digest: Sha256Digest,
    media_type: String,
    byte_length: u64,
    created_at_ms: u64,
    reference_count: u64,
}

impl EvidenceMetadata {
    pub fn new(
        digest: Sha256Digest,
        media_type: impl Into<String>,
        byte_length: u64,
        created_at_ms: u64,
        reference_count: u64,
    ) -> EvidenceResult<Self> {
        let media_type = media_type.into();
        validate_media_type(&media_type)?;
        Ok(Self {
            digest,
            media_type,
            byte_length,
            created_at_ms,
            reference_count,
        })
    }

    pub fn digest(&self) -> &Sha256Digest {
        &self.digest
    }

    pub fn media_type(&self) -> &str {
        &self.media_type
    }

    pub const fn byte_length(&self) -> u64 {
        self.byte_length
    }

    pub const fn created_at_ms(&self) -> u64 {
        self.created_at_ms
    }

    pub const fn reference_count(&self) -> u64 {
        self.reference_count
    }
}

/// Parameters for atomically storing one evidence stream and retaining it for
/// a Session.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PutEvidence {
    session_id: SessionId,
    media_type: String,
    expected_sha256: Option<Sha256Digest>,
    declared_size_bytes: Option<u64>,
}

impl PutEvidence {
    pub fn new(session_id: SessionId, media_type: impl Into<String>) -> EvidenceResult<Self> {
        let media_type = media_type.into();
        validate_media_type(&media_type)?;
        Ok(Self {
            session_id,
            media_type,
            expected_sha256: None,
            declared_size_bytes: None,
        })
    }

    pub fn with_expected_sha256(mut self, digest: Sha256Digest) -> Self {
        self.expected_sha256 = Some(digest);
        self
    }

    pub const fn with_declared_size_bytes(mut self, size: u64) -> Self {
        self.declared_size_bytes = Some(size);
        self
    }

    pub fn session_id(&self) -> &SessionId {
        &self.session_id
    }

    pub fn media_type(&self) -> &str {
        &self.media_type
    }

    pub fn expected_sha256(&self) -> Option<&Sha256Digest> {
        self.expected_sha256.as_ref()
    }

    pub const fn declared_size_bytes(&self) -> Option<u64> {
        self.declared_size_bytes
    }

    pub fn into_parts(self) -> (SessionId, String, Option<Sha256Digest>, Option<u64>) {
        (
            self.session_id,
            self.media_type,
            self.expected_sha256,
            self.declared_size_bytes,
        )
    }
}

/// Result of a successful `put`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StoredEvidence {
    metadata: EvidenceMetadata,
    deduplicated: bool,
}

impl StoredEvidence {
    pub const fn new(metadata: EvidenceMetadata, deduplicated: bool) -> Self {
        Self {
            metadata,
            deduplicated,
        }
    }

    pub fn metadata(&self) -> &EvidenceMetadata {
        &self.metadata
    }

    pub const fn deduplicated(&self) -> bool {
        self.deduplicated
    }

    /// Converts storage metadata to the canonical protocol-compatible
    /// reference. `AssetRef` remains the wire DTO; no storage-only type leaks
    /// into the protocol crate.
    pub fn asset_ref(&self) -> AssetRef {
        let digest = self.metadata.digest();
        AssetRef {
            id: digest.asset_id(),
            media_type: self.metadata.media_type().to_owned(),
            uri: digest.asset_uri(),
            sha256: Some(digest.to_string()),
        }
    }
}

/// Summary of dropping all references owned by one Session.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReleaseReport {
    pub session_id: SessionId,
    pub released_references: u64,
    pub newly_unreferenced_assets: u64,
    pub newly_unreferenced_bytes: u64,
}

/// Bounds one garbage-collection pass.
///
/// Only objects whose last reference was released at or before
/// `unreferenced_before_ms` are eligible. Limits apply in addition to that
/// cutoff. A dry run reports the same candidates without deleting them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GcPolicy {
    pub unreferenced_before_ms: u64,
    pub max_assets: Option<u64>,
    pub max_bytes: Option<u64>,
    pub dry_run: bool,
}

impl GcPolicy {
    pub const fn dry_run(unreferenced_before_ms: u64) -> Self {
        Self {
            unreferenced_before_ms,
            max_assets: None,
            max_bytes: None,
            dry_run: true,
        }
    }

    pub const fn delete(unreferenced_before_ms: u64) -> Self {
        Self {
            unreferenced_before_ms,
            max_assets: None,
            max_bytes: None,
            dry_run: false,
        }
    }
}

impl Default for GcPolicy {
    fn default() -> Self {
        Self::dry_run(0)
    }
}

/// Summary of one bounded garbage-collection pass.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct GcReport {
    pub examined_assets: u64,
    pub candidate_assets: u64,
    pub candidate_bytes: u64,
    pub deleted_assets: u64,
    pub deleted_bytes: u64,
    pub dry_run: bool,
}

#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum EvidenceError {
    #[error("evidence store is not configured for this runtime")]
    Unavailable,
    #[error("invalid Evidence Store configuration: {0}")]
    InvalidConfiguration(String),
    #[error("Evidence Store metadata is corrupt: {0}")]
    CorruptStore(String),
    #[error("invalid evidence reference: {0}")]
    InvalidReference(String),
    #[error("invalid SHA-256 digest: {0}")]
    InvalidDigest(String),
    #[error("invalid evidence media type: {0}")]
    InvalidMediaType(String),
    #[error("evidence content is empty")]
    EmptyContent,
    #[error("evidence declared {declared} bytes but received {actual} bytes")]
    DeclaredSizeMismatch { declared: u64, actual: u64 },
    #[error("evidence digest mismatch: expected {expected}, got {actual}")]
    DigestMismatch {
        expected: Sha256Digest,
        actual: Sha256Digest,
    },
    #[error("evidence not found: {0}")]
    NotFound(Sha256Digest),
    #[error("evidence {digest} is not attached to session {session_id}")]
    NotAttached {
        session_id: SessionId,
        digest: Sha256Digest,
    },
    #[error("evidence is corrupt: expected {expected}, got {actual}")]
    Corrupt {
        expected: Sha256Digest,
        actual: Sha256Digest,
    },
    #[error("evidence metadata is corrupt for {digest}: {reason}")]
    CorruptMetadata {
        digest: Sha256Digest,
        reason: String,
    },
    #[error(
        "evidence media type conflicts for {digest}: existing {existing}, requested {requested}"
    )]
    MediaTypeConflict {
        digest: Sha256Digest,
        existing: String,
        requested: String,
    },
    #[error("unsafe evidence storage path: {0}")]
    UnsafePath(String),
    #[error("evidence is too large: {actual} bytes exceeds {maximum} bytes")]
    TooLarge { actual: u64, maximum: u64 },
    #[error("session evidence reference limit exceeded: {maximum}")]
    ReferenceLimit { maximum: u64 },
    #[error("evidence session is already closed: {0}")]
    SessionClosed(SessionId),
    #[error("evidence store root is already locked by another process")]
    StoreBusy,
    #[error("unsupported evidence store version: {0}")]
    UnsupportedStoreVersion(u32),
    #[error("evidence I/O failed during {operation}: {message}")]
    Io { operation: String, message: String },
    #[error("evidence store failed: {0}")]
    Internal(String),
}

impl EvidenceError {
    pub fn io(operation: impl Into<String>, error: impl fmt::Display) -> Self {
        Self::Io {
            operation: operation.into(),
            message: error.to_string(),
        }
    }

    pub fn to_error_info(&self) -> ErrorInfo {
        let (code, message, retryable, details) = match self {
            Self::Unavailable => (
                "evidence_store_unavailable",
                "evidence store is not configured for this runtime",
                false,
                None,
            ),
            Self::InvalidConfiguration(_) => (
                "invalid_evidence_store_configuration",
                "evidence store configuration is invalid",
                false,
                None,
            ),
            Self::CorruptStore(_) => ("evidence_corrupt", "evidence store is corrupt", false, None),
            Self::InvalidReference(reason) => (
                "invalid_evidence_reference",
                "evidence reference is invalid",
                false,
                Some(json!({ "reason": reason })),
            ),
            Self::InvalidDigest(value) => (
                "invalid_evidence_digest",
                "evidence digest is invalid",
                false,
                Some(json!({ "digest": value })),
            ),
            Self::InvalidMediaType(media_type) => (
                "invalid_evidence_media_type",
                "evidence media type is invalid",
                false,
                Some(json!({ "mediaType": media_type })),
            ),
            Self::EmptyContent => ("empty_evidence", "evidence content is empty", false, None),
            Self::DeclaredSizeMismatch { declared, actual } => (
                "evidence_size_mismatch",
                "evidence size does not match its declaration",
                false,
                Some(json!({ "declaredBytes": declared, "actualBytes": actual })),
            ),
            Self::DigestMismatch { expected, actual } => (
                "evidence_digest_mismatch",
                "evidence digest does not match its declaration",
                false,
                Some(json!({ "expected": expected.as_str(), "actual": actual.as_str() })),
            ),
            Self::NotFound(digest) => (
                "evidence_not_found",
                "evidence was not found",
                false,
                Some(json!({ "digest": digest.as_str() })),
            ),
            Self::NotAttached { session_id, digest } => (
                "evidence_not_attached",
                "evidence is not attached to this session",
                false,
                Some(json!({
                    "sessionId": session_id,
                    "digest": digest.as_str()
                })),
            ),
            Self::Corrupt { expected, actual } => (
                "evidence_corrupt",
                "evidence content is corrupt",
                false,
                Some(json!({ "expected": expected.as_str(), "actual": actual.as_str() })),
            ),
            Self::CorruptMetadata { digest, .. } => (
                "evidence_corrupt",
                "evidence metadata is corrupt",
                false,
                Some(json!({ "digest": digest.as_str() })),
            ),
            Self::MediaTypeConflict {
                digest,
                existing,
                requested,
            } => (
                "evidence_media_type_conflict",
                "evidence media type conflicts with stored metadata",
                false,
                Some(json!({
                    "digest": digest.as_str(),
                    "existing": existing,
                    "requested": requested
                })),
            ),
            Self::UnsafePath(_) => (
                "unsafe_evidence_path",
                "evidence storage path is unsafe",
                false,
                None,
            ),
            Self::TooLarge { actual, maximum } => (
                "evidence_too_large",
                "evidence exceeds the configured size limit",
                false,
                Some(json!({ "actualBytes": actual, "maximumBytes": maximum })),
            ),
            Self::ReferenceLimit { maximum } => (
                "evidence_reference_limit",
                "session evidence reference limit exceeded",
                false,
                Some(json!({ "maximum": maximum })),
            ),
            Self::SessionClosed(session_id) => (
                "evidence_session_closed",
                "evidence session is already closed",
                false,
                Some(json!({ "sessionId": session_id })),
            ),
            Self::StoreBusy => ("evidence_store_busy", "evidence store is busy", true, None),
            Self::UnsupportedStoreVersion(version) => (
                "unsupported_evidence_store_version",
                "evidence store version is unsupported",
                false,
                Some(json!({ "version": version })),
            ),
            Self::Io { .. } => ("evidence_io_error", "evidence store I/O failed", true, None),
            Self::Internal(_) => ("evidence_store_error", "evidence store failed", true, None),
        };

        ErrorInfo {
            code: code.to_owned(),
            message: message.to_owned(),
            retryable,
            details,
        }
    }
}

/// Object-safe persistence boundary for content-addressed evidence.
///
/// Implementations compute SHA-256 while consuming `input`, enforce their
/// configured maximum asset size, compare optional declared size and digest,
/// and publish content atomically. Repeating the same `(Session, digest)` put
/// is idempotent; repeating content in another Session reuses the blob and
/// adds a reference. `release_session` removes references but never directly
/// deletes blobs, leaving bounded deletion to `gc`.
#[async_trait]
pub trait EvidenceStore: Send + Sync {
    async fn put(
        &self,
        request: PutEvidence,
        input: EvidenceInput,
    ) -> EvidenceResult<StoredEvidence>;

    /// Retains an existing canonical object for another Session without
    /// re-uploading its bytes.
    async fn attach(
        &self,
        session_id: &SessionId,
        asset: &AssetRef,
    ) -> EvidenceResult<StoredEvidence>;

    /// Verifies that `asset` is a canonical durable reference owned by
    /// `session_id` without creating or changing any ownership record.
    async fn verify_session_reference(
        &self,
        session_id: &SessionId,
        asset: &AssetRef,
    ) -> EvidenceResult<EvidenceMetadata>;

    /// Opens content only after verifying it against `digest`.
    async fn open(&self, digest: &Sha256Digest) -> EvidenceResult<EvidenceOutput>;

    async fn metadata(&self, digest: &Sha256Digest) -> EvidenceResult<EvidenceMetadata>;

    /// Lists Sessions that currently own at least one durable reference.
    /// This supports idempotent reconciliation after a Session log was
    /// deleted but the process stopped before `release_session` completed.
    async fn referenced_sessions(&self) -> EvidenceResult<Vec<SessionId>>;

    /// Idempotently drops all references for `session_id` and records when
    /// newly unreferenced blobs became eligible for retention policies.
    async fn release_session(
        &self,
        session_id: &SessionId,
        released_at_ms: u64,
    ) -> EvidenceResult<ReleaseReport>;

    async fn gc(&self, policy: GcPolicy) -> EvidenceResult<GcReport>;
}

/// Session-bound evidence capability supplied to one Driver operation.
///
/// Drivers can persist new bytes or retain an existing canonical asset, but
/// cannot choose a Session, inspect the backing Store, release references, or
/// run garbage collection. This keeps Session attribution under Core runtime
/// control even when device work is concurrent. The writer intentionally does
/// not implement `Clone`, so a Driver cannot retain the capability in a
/// detached `'static` task. Successful writes also create private,
/// operation-scoped receipts that Core reconciles with a successful Driver
/// result when the runtime was constructed with an injected Store.
pub struct SessionEvidenceWriter {
    session_id: SessionId,
    store: Arc<dyn EvidenceStore>,
    receipts: Mutex<BTreeSet<EvidenceReceipt>>,
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct EvidenceReceipt {
    id: String,
    media_type: String,
    uri: String,
    sha256: Option<String>,
}

impl From<&AssetRef> for EvidenceReceipt {
    fn from(asset: &AssetRef) -> Self {
        Self {
            id: asset.id.clone(),
            media_type: asset.media_type.clone(),
            uri: asset.uri.clone(),
            sha256: asset.sha256.clone(),
        }
    }
}

impl SessionEvidenceWriter {
    pub(crate) fn new(session_id: SessionId, store: Arc<dyn EvidenceStore>) -> Self {
        Self {
            session_id,
            store,
            receipts: Mutex::new(BTreeSet::new()),
        }
    }

    /// Stores one stream and pins the resulting object to this operation's
    /// Session. The Session cannot be overridden by the Driver.
    pub async fn put(
        &self,
        media_type: impl Into<String>,
        input: EvidenceInput,
    ) -> EvidenceResult<StoredEvidence> {
        let request = PutEvidence::new(self.session_id.clone(), media_type)?;
        let stored = self.store.put(request, input).await?;
        self.record(&stored);
        Ok(stored)
    }

    /// Stores one stream with an expected byte length while keeping Session
    /// selection private. Screenshot Drivers can use this to bind their
    /// bounded process output to the Store write contract.
    pub async fn put_with_declared_size(
        &self,
        media_type: impl Into<String>,
        declared_size_bytes: u64,
        input: EvidenceInput,
    ) -> EvidenceResult<StoredEvidence> {
        let request = PutEvidence::new(self.session_id.clone(), media_type)?
            .with_declared_size_bytes(declared_size_bytes);
        let stored = self.store.put(request, input).await?;
        self.record(&stored);
        Ok(stored)
    }

    /// Validates, serializes, bounds, and stores one canonical Protocol 1.5 UI
    /// Tree as a single operation-scoped Evidence object.
    ///
    /// Drivers use this typed path instead of serializing arbitrary JSON so a
    /// malformed or oversized tree cannot be referenced by an event before its
    /// complete body has passed the protocol contract.
    pub async fn put_ui_snapshot(
        &self,
        snapshot: &UiSnapshot,
    ) -> EvidenceResult<(StoredEvidence, u64)> {
        let bytes = encode_ui_snapshot(snapshot)?;
        let byte_length = u64::try_from(bytes.len()).map_err(|_| EvidenceError::TooLarge {
            actual: u64::MAX,
            maximum: MAX_UI_SNAPSHOT_BYTES,
        })?;
        let stored = self
            .put_with_declared_size(
                UI_SNAPSHOT_MEDIA_TYPE,
                byte_length,
                Box::pin(Cursor::new(bytes)),
            )
            .await?;
        Ok((stored, byte_length))
    }

    /// Pins an existing canonical object to this operation's Session.
    pub async fn attach(&self, asset: &AssetRef) -> EvidenceResult<StoredEvidence> {
        let stored = self.store.attach(&self.session_id, asset).await?;
        self.record(&stored);
        Ok(stored)
    }

    /// Compares the de-duplicated evidence returned by a successful Driver
    /// operation with the exact canonical references issued by this writer.
    /// Core keeps this check private so Drivers cannot forge ledger entries.
    pub(crate) fn receipts_match<'a>(
        &self,
        returned: impl IntoIterator<Item = &'a AssetRef>,
    ) -> bool {
        let returned = returned
            .into_iter()
            .map(EvidenceReceipt::from)
            .collect::<BTreeSet<_>>();
        let issued = self
            .receipts
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        *issued == returned
    }

    fn record(&self, stored: &StoredEvidence) {
        self.receipts
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(EvidenceReceipt::from(&stored.asset_ref()));
    }
}

fn encode_ui_snapshot(snapshot: &UiSnapshot) -> EvidenceResult<Vec<u8>> {
    snapshot
        .validate()
        .map_err(|error| EvidenceError::InvalidReference(error.to_string()))?;
    let bytes = serde_json::to_vec(snapshot)
        .map_err(|error| EvidenceError::Internal(format!("serialize UI snapshot: {error}")))?;
    let actual = u64::try_from(bytes.len()).unwrap_or(u64::MAX);
    if actual == 0 || actual > MAX_UI_SNAPSHOT_BYTES {
        return Err(EvidenceError::TooLarge {
            actual,
            maximum: MAX_UI_SNAPSHOT_BYTES,
        });
    }
    Ok(bytes)
}

impl fmt::Debug for SessionEvidenceWriter {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SessionEvidenceWriter")
            .finish_non_exhaustive()
    }
}

/// Explicit default used by runtimes that have not been configured with an
/// Evidence Store. Every operation fails; evidence is never silently dropped.
#[derive(Clone, Copy, Debug, Default)]
pub struct UnavailableEvidenceStore;

#[async_trait]
impl EvidenceStore for UnavailableEvidenceStore {
    async fn put(
        &self,
        _request: PutEvidence,
        _input: EvidenceInput,
    ) -> EvidenceResult<StoredEvidence> {
        Err(EvidenceError::Unavailable)
    }

    async fn attach(
        &self,
        _session_id: &SessionId,
        _asset: &AssetRef,
    ) -> EvidenceResult<StoredEvidence> {
        Err(EvidenceError::Unavailable)
    }

    async fn verify_session_reference(
        &self,
        _session_id: &SessionId,
        _asset: &AssetRef,
    ) -> EvidenceResult<EvidenceMetadata> {
        Err(EvidenceError::Unavailable)
    }

    async fn open(&self, _digest: &Sha256Digest) -> EvidenceResult<EvidenceOutput> {
        Err(EvidenceError::Unavailable)
    }

    async fn metadata(&self, _digest: &Sha256Digest) -> EvidenceResult<EvidenceMetadata> {
        Err(EvidenceError::Unavailable)
    }

    async fn referenced_sessions(&self) -> EvidenceResult<Vec<SessionId>> {
        Err(EvidenceError::Unavailable)
    }

    async fn release_session(
        &self,
        _session_id: &SessionId,
        _released_at_ms: u64,
    ) -> EvidenceResult<ReleaseReport> {
        Err(EvidenceError::Unavailable)
    }

    async fn gc(&self, _policy: GcPolicy) -> EvidenceResult<GcReport> {
        Err(EvidenceError::Unavailable)
    }
}

fn validate_media_type(media_type: &str) -> EvidenceResult<()> {
    if media_type.len() > MAX_MEDIA_TYPE_LENGTH {
        return Err(EvidenceError::InvalidMediaType(media_type.to_owned()));
    }
    let Some((kind, subtype)) = media_type.split_once('/') else {
        return Err(EvidenceError::InvalidMediaType(media_type.to_owned()));
    };
    let valid_part = |part: &str| {
        !part.is_empty()
            && part
                .bytes()
                .all(|byte| byte.is_ascii_graphic() && !byte.is_ascii_whitespace() && byte != b'/')
    };
    if valid_part(kind) && valid_part(subtype) {
        Ok(())
    } else {
        Err(EvidenceError::InvalidMediaType(media_type.to_owned()))
    }
}

#[cfg(test)]
mod tests {
    use super::{
        EvidenceError, EvidenceMetadata, EvidenceStore, Sha256Digest, StoredEvidence,
        encode_ui_snapshot,
    };
    use devicerail_protocol::{
        SessionId, UI_SNAPSHOT_FORMAT_VERSION, UiContextKind, UiContextRef, UiNode, UiSnapshot,
    };
    use uuid::Uuid;

    const DIGEST: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

    #[test]
    fn digest_accepts_only_canonical_lowercase_hex() {
        let digest = Sha256Digest::parse(DIGEST).expect("canonical digest");
        assert_eq!(digest.as_str(), DIGEST);
        assert_eq!(digest.to_string(), DIGEST);

        for invalid in [
            &DIGEST[..63],
            "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdeF",
            "g123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
            "sha256:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
        ] {
            assert!(matches!(
                Sha256Digest::parse(invalid),
                Err(EvidenceError::InvalidDigest(_))
            ));
        }
    }

    #[test]
    fn stored_evidence_emits_the_canonical_asset_ref_subset() {
        let digest = Sha256Digest::parse(DIGEST).expect("digest");
        let metadata = EvidenceMetadata::new(digest.clone(), "image/png", 42, 1_000, 1)
            .expect("valid metadata");
        let stored = StoredEvidence::new(metadata, false);

        let reference = stored.asset_ref();
        assert_eq!(reference.id, format!("sha256:{DIGEST}"));
        assert_eq!(reference.media_type, "image/png");
        assert_eq!(
            reference.uri,
            format!("devicerail://assets/sha256/{DIGEST}")
        );
        assert_eq!(reference.sha256.as_deref(), Some(DIGEST));
        assert_eq!(
            Sha256Digest::from_asset_ref(&reference).expect("parse canonical reference"),
            digest
        );

        let mut invalid = reference;
        invalid.uri.push_str("?download=1");
        assert!(matches!(
            Sha256Digest::from_asset_ref(&invalid),
            Err(EvidenceError::InvalidReference(_))
        ));
    }

    #[test]
    fn evidence_errors_have_stable_wire_classification() {
        let digest = Sha256Digest::parse(DIGEST).expect("digest");
        let cases = [
            (
                EvidenceError::Unavailable,
                "evidence_store_unavailable",
                false,
            ),
            (
                EvidenceError::NotFound(digest.clone()),
                "evidence_not_found",
                false,
            ),
            (
                EvidenceError::NotAttached {
                    session_id: SessionId::new(),
                    digest,
                },
                "evidence_not_attached",
                false,
            ),
            (
                EvidenceError::UnsafePath("../escape".to_owned()),
                "unsafe_evidence_path",
                false,
            ),
            (
                EvidenceError::io("read", std::io::Error::other("closed")),
                "evidence_io_error",
                true,
            ),
            (
                EvidenceError::Internal("database unavailable".to_owned()),
                "evidence_store_error",
                true,
            ),
        ];

        for (error, code, retryable) in cases {
            let info = error.to_error_info();
            assert_eq!(info.code, code);
            assert_eq!(info.retryable, retryable);
            assert!(!info.message.is_empty());
        }
    }

    #[test]
    fn internal_evidence_failures_are_sanitized_on_the_wire() {
        let digest = Sha256Digest::parse(DIGEST).expect("digest");
        const SECRET: &str = "/Users/private/evidence-root/token.txt";
        let cases = [
            EvidenceError::InvalidConfiguration(SECRET.to_owned()),
            EvidenceError::CorruptStore(SECRET.to_owned()),
            EvidenceError::CorruptMetadata {
                digest,
                reason: SECRET.to_owned(),
            },
            EvidenceError::UnsafePath(SECRET.to_owned()),
            EvidenceError::Io {
                operation: SECRET.to_owned(),
                message: format!("permission denied: {SECRET}"),
            },
            EvidenceError::Internal(format!("database failed at {SECRET}")),
        ];

        for error in cases {
            let local = error.to_string();
            assert!(local.contains(SECRET), "local diagnostics keep the source");

            let info = error.to_error_info();
            let wire = serde_json::to_string(&info).expect("serialize ErrorInfo");
            assert!(!wire.contains(SECRET), "wire ErrorInfo must be sanitized");
        }
    }

    #[test]
    fn trait_is_object_safe() {
        fn accepts_trait_object(_: Option<&dyn EvidenceStore>) {}
        accepts_trait_object(None);
    }

    #[test]
    fn typed_ui_snapshot_evidence_rejects_invalid_bodies_before_store_io() {
        let mut snapshot = UiSnapshot {
            format_version: UI_SNAPSHOT_FORMAT_VERSION,
            observation_id: Uuid::nil(),
            context: UiContextRef {
                context_kind: UiContextKind::Native,
                context_id: "NATIVE_APP".to_owned(),
                document_epoch: "epoch-1".to_owned(),
            },
            root_stable_node_ids: vec!["root".to_owned()],
            nodes: vec![UiNode {
                stable_node_id: "root".to_owned(),
                parent_stable_node_id: None,
                role: "application".to_owned(),
                name: None,
                value: None,
                identifier: None,
                text: None,
                bounds: None,
                enabled: Some(true),
                hittable: None,
            }],
        };

        let bytes = encode_ui_snapshot(&snapshot).expect("valid canonical UI Tree");
        assert!(!bytes.is_empty());

        snapshot.nodes[0].stable_node_id.clear();
        assert!(matches!(
            encode_ui_snapshot(&snapshot),
            Err(EvidenceError::InvalidReference(_))
        ));
    }
}
