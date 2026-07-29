use std::{
    fmt,
    fs::{self, OpenOptions},
    future::Future,
    io,
    path::{Path, PathBuf},
};

use async_trait::async_trait;
use devicerail_core::{
    CancellationReason, EvidenceError, EvidenceOutput, EvidenceStore, ExecutionControl,
    Sha256Digest, TimeoutScope,
};
use devicerail_protocol::AssetRef;
use serde::Serialize;
use sha2::{Digest as _, Sha256};
use thiserror::Error;
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

use crate::filesystem::{
    StagingDir, asset_path, inspect_bundle_tree, manifest_path, metadata_is_link_like,
    open_regular_file_nofollow, sync_directory,
};
use crate::{
    BUNDLE_MAGIC, BUNDLE_VERSION, BundleAsset, BundleLimits, BundleManifest, BundleSource,
    BundleSummary, CanonicalJsonError, FilesystemError, ModelError, ValidatedBundle,
    from_canonical_slice, to_canonical_bytes, validate_manifest_events, validate_source,
};

const COPY_BUFFER_BYTES: usize = 64 * 1024;

/// Bytes read from one asset in an already validated Bundle snapshot.
///
/// The filesystem path is deliberately not exposed: callers identify assets
/// by their canonical digest, while this module alone derives the on-disk
/// location.  All fields are owned so the result cannot retain a filesystem
/// handle or borrow mutable manifest state.
#[derive(PartialEq, Eq)]
pub struct ValidatedAssetBytes {
    pub sha256: String,
    pub media_type: String,
    pub byte_length: u64,
    pub bytes: Vec<u8>,
}

impl fmt::Debug for ValidatedAssetBytes {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ValidatedAssetBytes")
            .field("sha256", &self.sha256)
            .field("media_type", &self.media_type)
            .field("byte_length", &self.byte_length)
            .finish_non_exhaustive()
    }
}

/// A verified, read-only Evidence object supplied to the Bundle writer.
pub struct BundleEvidence {
    pub media_type: String,
    pub byte_length: u64,
    pub reader: EvidenceOutput,
}

/// Minimal Evidence capability required by a Session Bundle export.
///
/// Callers must serialize export with Session cleanup and Evidence GC. The
/// offline CLI obtains that guarantee by exclusively opening the stopped
/// daemon's filesystem Evidence Store.
#[async_trait]
pub trait BundleEvidenceSource: Send + Sync {
    async fn open_asset(&self, reference: &AssetRef) -> Result<BundleEvidence, EvidenceError>;
}

#[async_trait]
impl<T> BundleEvidenceSource for T
where
    T: EvidenceStore + ?Sized,
{
    async fn open_asset(&self, reference: &AssetRef) -> Result<BundleEvidence, EvidenceError> {
        let digest = Sha256Digest::from_asset_ref(reference)?;
        let metadata = self.metadata(&digest).await?;
        if metadata.digest() != &digest || metadata.media_type() != reference.media_type {
            return Err(EvidenceError::InvalidReference(
                "Evidence metadata does not match the typed reference".to_owned(),
            ));
        }
        let reader = self.open(&digest).await?;
        Ok(BundleEvidence {
            media_type: metadata.media_type().to_owned(),
            byte_length: metadata.byte_length(),
            reader,
        })
    }
}

#[derive(Debug, Error)]
pub enum BundleError {
    #[error(transparent)]
    Model(#[from] ModelError),
    #[error(transparent)]
    CanonicalJson(#[from] CanonicalJsonError),
    #[error("Evidence could not be read for the Bundle")]
    Evidence(#[source] EvidenceError),
    #[error("Bundle filesystem validation or publication failed")]
    Filesystem(#[source] FilesystemError),
    #[error("Bundle target already exists")]
    TargetExists,
    #[error("Bundle target is not a safe child of an existing real directory")]
    InvalidTarget,
    #[error("Bundle manifest exceeds its configured byte limit")]
    ManifestTooLarge,
    #[error("Evidence metadata conflicts with its typed reference")]
    EvidenceMetadataMismatch,
    #[error("Evidence stream length conflicts with its metadata")]
    EvidenceSizeMismatch,
    #[error("Evidence bytes do not match their canonical SHA-256")]
    EvidenceDigestMismatch,
    #[error("Bundle asset limits were exceeded")]
    AssetLimitExceeded,
    #[error("Bundle operation was cancelled: {reason:?}")]
    Cancelled { reason: CancellationReason },
    #[error("Bundle operation timed out after {timeout_ms} ms")]
    TimedOut {
        scope: TimeoutScope,
        timeout_ms: u64,
    },
    #[error("Bundle was published, but parent-directory durability is unknown")]
    PublishedDurabilityUnknown,
    #[error("Bundle I/O failed while {operation}")]
    Io {
        operation: &'static str,
        #[source]
        source: io::Error,
    },
}

impl From<EvidenceError> for BundleError {
    fn from(error: EvidenceError) -> Self {
        Self::Evidence(error)
    }
}

impl From<FilesystemError> for BundleError {
    fn from(error: FilesystemError) -> Self {
        match error {
            FilesystemError::DestinationExists(_) => Self::TargetExists,
            FilesystemError::InvalidTarget(_) => Self::InvalidTarget,
            FilesystemError::PublishedDurabilityUnknown { .. } => Self::PublishedDurabilityUnknown,
            other => Self::Filesystem(other),
        }
    }
}

impl From<io::Error> for BundleError {
    fn from(source: io::Error) -> Self {
        Self::Io {
            operation: "performing streamed Bundle I/O",
            source,
        }
    }
}

/// Export an ended Session into a canonical directory without replacing an
/// existing target.
pub async fn export_directory(
    source: &BundleSource,
    evidence: &dyn BundleEvidenceSource,
    target: &Path,
    limits: &BundleLimits,
    control: &ExecutionControl,
) -> Result<BundleSummary, BundleError> {
    let references = validate_source(source, limits)?;
    ensure_manifest_fits(source, &references, limits)?;
    check_control(control)?;
    let target = prepare_target(target)?;
    let staging = StagingDir::create(&target)?;

    let mut assets = Vec::with_capacity(references.len());
    let mut total_asset_bytes = 0_u64;
    if !references.is_empty() {
        create_private_directory(&staging.path().join("assets"))?;
        create_private_directory(&staging.path().join("assets").join("sha256"))?;
    }

    for (digest, reference) in references {
        check_control(control)?;
        let opened = wait_controlled(control, evidence.open_asset(&reference)).await??;
        if opened.media_type != reference.media_type {
            return Err(BundleError::EvidenceMetadataMismatch);
        }
        if opened.byte_length > limits.max_asset_bytes {
            return Err(BundleError::AssetLimitExceeded);
        }
        total_asset_bytes = total_asset_bytes
            .checked_add(opened.byte_length)
            .ok_or(BundleError::AssetLimitExceeded)?;
        if total_asset_bytes > limits.max_total_asset_bytes {
            return Err(BundleError::AssetLimitExceeded);
        }

        let output_path = asset_path(staging.path(), digest.as_str())?;
        copy_asset(
            opened.reader,
            &output_path,
            &digest,
            opened.byte_length,
            limits,
            control,
        )
        .await?;
        assets.push(BundleAsset {
            sha256: digest.to_string(),
            media_type: reference.media_type,
            byte_length: opened.byte_length,
            path: BundleAsset::canonical_path(digest.as_str()),
        });
    }

    let manifest = BundleManifest::from_source(source, assets);
    // Re-run the manifest/index validation against the exact data that will be
    // published, rather than assuming the construction code stayed correct.
    validate_manifest_events(&manifest, limits)?;
    let manifest_bytes = to_canonical_bytes(&manifest)?;
    if manifest_bytes.len() as u64 > limits.max_manifest_bytes {
        return Err(BundleError::ManifestTooLarge);
    }
    write_private_file(
        &manifest_path(staging.path()),
        &manifest_bytes,
        control,
        "writing the Bundle manifest",
    )
    .await?;

    if !manifest.assets.is_empty() {
        sync_directory(&staging.path().join("assets").join("sha256"))?;
        sync_directory(&staging.path().join("assets"))?;
    }
    sync_directory(staging.path())?;

    // A full read-back catches short writes, unexpected nodes, and any
    // construction/validation drift before the publication linearization
    // point.
    let validated = validate_directory(staging.path(), limits, control).await?;
    if validated.event_protocol_version != source.event_protocol_version
        || validated.session_export != source.session_export
    {
        return Err(BundleError::Filesystem(FilesystemError::UnexpectedNode {
            path: staging.path().to_path_buf(),
            expected: "the source Session replay",
        }));
    }

    check_control(control)?;
    let summary = BundleSummary::from_manifest(&manifest);
    // `commit` is the publication point. It intentionally performs no
    // cancellation check after rename and distinguishes a later fsync error.
    staging.commit()?;
    Ok(summary)
}

fn ensure_manifest_fits(
    source: &BundleSource,
    references: &std::collections::BTreeMap<Sha256Digest, AssetRef>,
    limits: &BundleLimits,
) -> Result<(), BundleError> {
    let assets = references
        .iter()
        .map(|(digest, reference)| BundleAsset {
            sha256: digest.to_string(),
            media_type: reference.media_type.clone(),
            // The real value cannot use more decimal digits than this upper
            // bound, so the serialized placeholder is a conservative size.
            byte_length: limits.max_asset_bytes,
            path: BundleAsset::canonical_path(digest.as_str()),
        })
        .collect::<Vec<_>>();
    #[derive(Serialize)]
    #[serde(rename_all = "camelCase")]
    struct BorrowedManifest<'a> {
        magic: &'static str,
        bundle_version: u16,
        event_protocol_version: devicerail_protocol::ProtocolVersion,
        session: &'a devicerail_protocol::SessionInfo,
        events: &'a [devicerail_protocol::TestEvent],
        assets: &'a [BundleAsset],
    }
    let manifest = BorrowedManifest {
        magic: BUNDLE_MAGIC,
        bundle_version: BUNDLE_VERSION,
        event_protocol_version: source.event_protocol_version,
        session: &source.session_export.session,
        events: &source.session_export.events,
        assets: &assets,
    };
    let mut writer = BoundedJsonWriter::new(limits.max_manifest_bytes.saturating_sub(1));
    if let Err(error) = serde_json::to_writer(&mut writer, &manifest) {
        if writer.exceeded {
            return Err(BundleError::ManifestTooLarge);
        }
        return Err(CanonicalJsonError::from(error).into());
    }
    if writer.written.saturating_add(1) > limits.max_manifest_bytes {
        return Err(BundleError::ManifestTooLarge);
    }
    Ok(())
}

struct BoundedJsonWriter {
    written: u64,
    limit: u64,
    exceeded: bool,
}

impl BoundedJsonWriter {
    const fn new(limit: u64) -> Self {
        Self {
            written: 0,
            limit,
            exceeded: false,
        }
    }
}

impl io::Write for BoundedJsonWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let length = u64::try_from(bytes.len()).unwrap_or(u64::MAX);
        let Some(next) = self.written.checked_add(length) else {
            self.exceeded = true;
            return Err(io::Error::other("Bundle manifest byte limit exceeded"));
        };
        if next > self.limit {
            self.exceeded = true;
            return Err(io::Error::other("Bundle manifest byte limit exceeded"));
        }
        self.written = next;
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// Validate a canonical Bundle directory and reconstruct its replay input.
pub async fn validate_directory(
    root: &Path,
    limits: &BundleLimits,
    control: &ExecutionControl,
) -> Result<ValidatedBundle, BundleError> {
    check_control(control)?;
    let tree = inspect_bundle_tree(root, limits.max_assets)?;
    let manifest_bytes = read_bounded_file(
        &tree.manifest,
        limits.max_manifest_bytes,
        control,
        "reading the Bundle manifest",
    )
    .await?;
    let manifest: BundleManifest = from_canonical_slice(&manifest_bytes)?;
    let references = validate_manifest_events(&manifest, limits)?;

    if tree.assets.len() != references.len()
        || tree
            .assets
            .keys()
            .zip(references.keys())
            .any(|(observed, expected)| observed != expected.as_str())
    {
        return Err(ModelError::AssetSetMismatch.into());
    }

    let mut total_asset_bytes = 0_u64;
    for asset in &manifest.assets {
        check_control(control)?;
        let digest = Sha256Digest::parse(asset.sha256.clone())
            .map_err(|_| ModelError::InvalidAssetIndexEntry)?;
        let path = tree
            .assets
            .get(digest.as_str())
            .ok_or(ModelError::AssetSetMismatch)?;
        let observed_bytes =
            hash_asset_file(path, &digest, asset.byte_length, limits, control).await?;
        total_asset_bytes = total_asset_bytes
            .checked_add(observed_bytes)
            .ok_or(BundleError::AssetLimitExceeded)?;
        if total_asset_bytes > limits.max_total_asset_bytes {
            return Err(BundleError::AssetLimitExceeded);
        }
    }

    Ok(ValidatedBundle::from_manifest(&manifest))
}

/// Read one asset selected from a [`ValidatedBundle`](crate::ValidatedBundle).
///
/// `asset` is expected to be an entry from the previously validated snapshot,
/// but none of its security-sensitive fields are trusted here.  The digest,
/// media type, canonical path, declared size, current file size, and content
/// digest are checked again.  The supplied path string is never resolved; the
/// file location is derived only from the canonical digest and opened with the
/// platform's no-follow regular-file primitive.
///
/// `max_bytes` is an additional per-read budget and may be lower than the
/// limit used for whole-Bundle validation.  As with Bundle validation, callers
/// must prevent same-authority mutation of intermediate directories while the
/// operation is in progress.
pub async fn read_validated_asset(
    root: &Path,
    asset: &BundleAsset,
    max_bytes: u64,
    control: &ExecutionControl,
) -> Result<ValidatedAssetBytes, BundleError> {
    check_control(control)?;

    let digest = Sha256Digest::parse(asset.sha256.clone())
        .map_err(|_| ModelError::InvalidAssetIndexEntry)?;
    let reference = AssetRef {
        id: digest.asset_id(),
        media_type: asset.media_type.clone(),
        uri: digest.asset_uri(),
        sha256: Some(asset.sha256.clone()),
    };
    Sha256Digest::from_asset_ref(&reference).map_err(|_| ModelError::InvalidAssetIndexEntry)?;
    if asset.path != BundleAsset::canonical_path(digest.as_str()) {
        return Err(ModelError::InvalidAssetPath.into());
    }
    if asset.byte_length > max_bytes {
        return Err(BundleError::AssetLimitExceeded);
    }

    let path = asset_path(root, digest.as_str())?;
    let file = open_regular_file_nofollow(&path)?;
    let metadata = file.metadata().map_err(|source| BundleError::Io {
        operation: "inspecting an opened Bundle asset",
        source,
    })?;
    if metadata.len() != asset.byte_length {
        return Err(BundleError::EvidenceSizeMismatch);
    }

    let mut file = tokio::fs::File::from_std(file);
    let mut buffer = vec![0_u8; COPY_BUFFER_BYTES];
    let mut hasher = Sha256::new();
    let mut observed = 0_u64;
    let initial_capacity = usize::try_from(asset.byte_length)
        .map_err(|_| BundleError::AssetLimitExceeded)?
        .min(COPY_BUFFER_BYTES);
    let mut bytes = Vec::with_capacity(initial_capacity);
    loop {
        let count = wait_controlled(control, file.read(&mut buffer)).await??;
        if count == 0 {
            break;
        }
        observed = observed
            .checked_add(count as u64)
            .ok_or(BundleError::AssetLimitExceeded)?;
        if observed > max_bytes {
            return Err(BundleError::AssetLimitExceeded);
        }
        if observed > asset.byte_length {
            return Err(BundleError::EvidenceSizeMismatch);
        }
        bytes
            .try_reserve(count)
            .map_err(|_| BundleError::AssetLimitExceeded)?;
        bytes.extend_from_slice(&buffer[..count]);
        hasher.update(&buffer[..count]);
    }
    check_control(control)?;
    if observed != asset.byte_length {
        return Err(BundleError::EvidenceSizeMismatch);
    }
    let actual = hex::encode(hasher.finalize());
    if actual != digest.as_str() {
        return Err(BundleError::EvidenceDigestMismatch);
    }

    Ok(ValidatedAssetBytes {
        sha256: digest.to_string(),
        media_type: asset.media_type.clone(),
        byte_length: observed,
        bytes,
    })
}

fn prepare_target(target: &Path) -> Result<PathBuf, BundleError> {
    let name = target.file_name().ok_or(BundleError::InvalidTarget)?;
    if name.is_empty() || name == "." || name == ".." {
        return Err(BundleError::InvalidTarget);
    }
    let parent = target
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let metadata = fs::symlink_metadata(parent).map_err(|source| BundleError::Io {
        operation: "inspecting the Bundle output parent",
        source,
    })?;
    if metadata_is_link_like(&metadata) || !metadata.is_dir() {
        return Err(BundleError::InvalidTarget);
    }
    let parent = parent.canonicalize().map_err(|source| BundleError::Io {
        operation: "resolving the Bundle output parent",
        source,
    })?;
    let target = parent.join(name);
    match fs::symlink_metadata(&target) {
        Ok(_) => Err(BundleError::TargetExists),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(target),
        Err(source) => Err(BundleError::Io {
            operation: "inspecting the Bundle target",
            source,
        }),
    }
}

#[cfg(unix)]
fn create_private_directory(path: &Path) -> Result<(), BundleError> {
    use std::os::unix::fs::DirBuilderExt as _;

    let mut builder = fs::DirBuilder::new();
    builder.mode(0o700);
    builder.create(path).map_err(|source| BundleError::Io {
        operation: "creating the Bundle asset directory",
        source,
    })
}

#[cfg(not(unix))]
fn create_private_directory(path: &Path) -> Result<(), BundleError> {
    fs::create_dir(path).map_err(|source| BundleError::Io {
        operation: "creating the Bundle asset directory",
        source,
    })
}

fn create_private_file(path: &Path) -> Result<tokio::fs::File, BundleError> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }
    let file = options.open(path).map_err(|source| BundleError::Io {
        operation: "creating a Bundle file",
        source,
    })?;
    Ok(tokio::fs::File::from_std(file))
}

async fn write_private_file(
    path: &Path,
    bytes: &[u8],
    control: &ExecutionControl,
    operation: &'static str,
) -> Result<(), BundleError> {
    let mut file = create_private_file(path)?;
    wait_controlled(control, file.write_all(bytes)).await??;
    wait_controlled(control, file.flush()).await??;
    wait_controlled(control, file.sync_all()).await??;
    check_control(control).map_err(|error| match error {
        BundleError::Cancelled { .. } | BundleError::TimedOut { .. } => error,
        _ => BundleError::Io {
            operation,
            source: io::Error::other("Bundle control failed"),
        },
    })
}

async fn copy_asset(
    mut reader: EvidenceOutput,
    output_path: &Path,
    expected_digest: &Sha256Digest,
    expected_bytes: u64,
    limits: &BundleLimits,
    control: &ExecutionControl,
) -> Result<(), BundleError> {
    let mut output = create_private_file(output_path)?;
    let mut buffer = vec![0_u8; COPY_BUFFER_BYTES];
    let mut hasher = Sha256::new();
    let mut observed = 0_u64;
    loop {
        let count = wait_controlled(control, reader.read(&mut buffer)).await??;
        if count == 0 {
            break;
        }
        observed = observed
            .checked_add(count as u64)
            .ok_or(BundleError::AssetLimitExceeded)?;
        if observed > expected_bytes || observed > limits.max_asset_bytes {
            return Err(BundleError::EvidenceSizeMismatch);
        }
        hasher.update(&buffer[..count]);
        wait_controlled(control, output.write_all(&buffer[..count])).await??;
    }
    if observed != expected_bytes {
        return Err(BundleError::EvidenceSizeMismatch);
    }
    let actual = hex::encode(hasher.finalize());
    if actual != expected_digest.as_str() {
        return Err(BundleError::EvidenceDigestMismatch);
    }
    wait_controlled(control, output.flush()).await??;
    wait_controlled(control, output.sync_all()).await??;
    Ok(())
}

async fn read_bounded_file(
    path: &Path,
    limit: u64,
    control: &ExecutionControl,
    operation: &'static str,
) -> Result<Vec<u8>, BundleError> {
    let file = open_regular_file_nofollow(path)?;
    let metadata = file.metadata().map_err(|source| BundleError::Io {
        operation: "inspecting a Bundle file",
        source,
    })?;
    if metadata.len() > limit {
        return Err(BundleError::ManifestTooLarge);
    }
    let mut file = tokio::fs::File::from_std(file).take(limit.saturating_add(1));
    let capacity = usize::try_from(metadata.len()).unwrap_or(usize::MAX);
    let mut bytes = Vec::with_capacity(capacity.min(1024 * 1024));
    wait_controlled(control, file.read_to_end(&mut bytes)).await??;
    if bytes.len() as u64 > limit {
        return Err(BundleError::ManifestTooLarge);
    }
    check_control(control).map_err(|error| match error {
        BundleError::Cancelled { .. } | BundleError::TimedOut { .. } => error,
        _ => BundleError::Io {
            operation,
            source: io::Error::other("Bundle control failed"),
        },
    })?;
    Ok(bytes)
}

async fn hash_asset_file(
    path: &Path,
    expected_digest: &Sha256Digest,
    expected_bytes: u64,
    limits: &BundleLimits,
    control: &ExecutionControl,
) -> Result<u64, BundleError> {
    if expected_bytes > limits.max_asset_bytes {
        return Err(BundleError::AssetLimitExceeded);
    }
    let file = open_regular_file_nofollow(path)?;
    let metadata = file.metadata().map_err(|source| BundleError::Io {
        operation: "inspecting a Bundle asset",
        source,
    })?;
    if metadata.len() != expected_bytes {
        return Err(BundleError::EvidenceSizeMismatch);
    }
    let mut file = tokio::fs::File::from_std(file);
    let mut buffer = vec![0_u8; COPY_BUFFER_BYTES];
    let mut hasher = Sha256::new();
    let mut observed = 0_u64;
    loop {
        let count = wait_controlled(control, file.read(&mut buffer)).await??;
        if count == 0 {
            break;
        }
        observed = observed
            .checked_add(count as u64)
            .ok_or(BundleError::AssetLimitExceeded)?;
        if observed > expected_bytes || observed > limits.max_asset_bytes {
            return Err(BundleError::EvidenceSizeMismatch);
        }
        hasher.update(&buffer[..count]);
    }
    if observed != expected_bytes {
        return Err(BundleError::EvidenceSizeMismatch);
    }
    let actual = hex::encode(hasher.finalize());
    if actual != expected_digest.as_str() {
        return Err(BundleError::EvidenceDigestMismatch);
    }
    Ok(observed)
}

fn check_control(control: &ExecutionControl) -> Result<(), BundleError> {
    if let Some(reason) = control.cancellation_reason() {
        return Err(BundleError::Cancelled { reason });
    }
    if control.is_expired() {
        return Err(timeout_error(control));
    }
    Ok(())
}

fn timeout_error(control: &ExecutionControl) -> BundleError {
    let (scope, timeout_ms) = control.timeout().unwrap_or((TimeoutScope::Request, 0));
    BundleError::TimedOut { scope, timeout_ms }
}

async fn wait_controlled<F, T>(control: &ExecutionControl, future: F) -> Result<T, BundleError>
where
    F: Future<Output = T>,
{
    check_control(control)?;
    tokio::pin!(future);
    match control.remaining() {
        Some(remaining) => {
            let deadline = tokio::time::sleep(remaining);
            tokio::pin!(deadline);
            tokio::select! {
                biased;
                output = &mut future => Ok(output),
                reason = control.cancelled() => Err(BundleError::Cancelled { reason }),
                () = &mut deadline => Err(timeout_error(control)),
            }
        }
        None => {
            tokio::select! {
                biased;
                output = &mut future => Ok(output),
                reason = control.cancelled() => Err(BundleError::Cancelled { reason }),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use devicerail_protocol::{
        EventId, EventSequence, ProtocolVersion, SessionExport, SessionId, SessionInfo,
        SessionState, TestEvent, TestEventPayload,
    };
    use uuid::Uuid;

    use super::*;

    struct MemoryEvidence {
        bytes: BTreeMap<String, Vec<u8>>,
    }

    #[async_trait]
    impl BundleEvidenceSource for MemoryEvidence {
        async fn open_asset(&self, reference: &AssetRef) -> Result<BundleEvidence, EvidenceError> {
            let digest = Sha256Digest::from_asset_ref(reference)?;
            let bytes = self
                .bytes
                .get(digest.as_str())
                .cloned()
                .ok_or_else(|| EvidenceError::NotFound(digest.clone()))?;
            Ok(BundleEvidence {
                media_type: reference.media_type.clone(),
                byte_length: bytes.len() as u64,
                reader: Box::pin(std::io::Cursor::new(bytes)),
            })
        }
    }

    fn ended_source() -> BundleSource {
        let session_id = SessionId::from(
            Uuid::parse_str("33333333-3333-4333-8333-333333333333").expect("session UUID"),
        );
        let started = TestEvent {
            event_id: EventId::from(
                Uuid::parse_str("66666666-6666-4666-8666-666666666661").expect("event UUID"),
            ),
            session_id: session_id.clone(),
            sequence: EventSequence::FIRST,
            request_id: None,
            device_id: None,
            at_ms: 100,
            payload: TestEventPayload::SessionStarted,
        };
        let ended = TestEvent {
            event_id: EventId::from(
                Uuid::parse_str("66666666-6666-4666-8666-666666666662").expect("event UUID"),
            ),
            session_id: session_id.clone(),
            sequence: EventSequence::new(2).expect("sequence"),
            request_id: None,
            device_id: None,
            at_ms: 200,
            payload: TestEventPayload::SessionEnded {
                outcome: devicerail_protocol::SessionOutcome::Completed,
                reason: None,
            },
        };
        BundleSource {
            event_protocol_version: ProtocolVersion::new(1, 2),
            session_export: SessionExport {
                session: SessionInfo {
                    id: session_id,
                    state: SessionState::Ended,
                    started_at_ms: 100,
                    ended_at_ms: Some(200),
                    event_count: EventSequence::new(2).expect("count"),
                    last_sequence: EventSequence::new(2).expect("last sequence"),
                },
                events: vec![started, ended],
            },
        }
    }

    #[tokio::test]
    async fn zero_asset_export_is_deterministic_and_replayable() {
        let temporary = tempfile::TempDir::new().expect("temporary directory");
        let first = temporary.path().join("first");
        let second = temporary.path().join("second");
        let source = ended_source();
        let evidence = MemoryEvidence {
            bytes: BTreeMap::new(),
        };

        let first_summary = export_directory(
            &source,
            &evidence,
            &first,
            &BundleLimits::default(),
            &ExecutionControl::unbounded(),
        )
        .await
        .expect("first export");
        export_directory(
            &source,
            &evidence,
            &second,
            &BundleLimits::default(),
            &ExecutionControl::unbounded(),
        )
        .await
        .expect("second export");

        assert_eq!(first_summary.asset_count, 0);
        assert!(!first.join("assets").exists());
        assert_eq!(
            fs::read(first.join("manifest.json")).expect("first manifest"),
            fs::read(second.join("manifest.json")).expect("second manifest")
        );
        let validated = validate_directory(
            &first,
            &BundleLimits::default(),
            &ExecutionControl::unbounded(),
        )
        .await
        .expect("validate");
        assert_eq!(validated.session_export, source.session_export);
    }

    #[tokio::test]
    async fn pre_cancelled_export_publishes_nothing() {
        let temporary = tempfile::TempDir::new().expect("temporary directory");
        let target = temporary.path().join("bundle");
        let source = ended_source();
        let evidence = MemoryEvidence {
            bytes: BTreeMap::new(),
        };
        let (controller, control) = devicerail_core::ExecutionController::new();
        controller.cancel(CancellationReason::Requested);

        assert!(matches!(
            export_directory(
                &source,
                &evidence,
                &target,
                &BundleLimits::default(),
                &control,
            )
            .await,
            Err(BundleError::Cancelled { .. })
        ));
        assert!(!target.exists());
    }

    #[tokio::test]
    async fn manifest_limit_fails_before_staging_is_created() {
        let temporary = tempfile::TempDir::new().expect("temporary directory");
        let target = temporary.path().join("bundle");
        let source = ended_source();
        let evidence = MemoryEvidence {
            bytes: BTreeMap::new(),
        };
        let limits = BundleLimits {
            max_manifest_bytes: 1,
            ..BundleLimits::default()
        };

        assert!(matches!(
            export_directory(
                &source,
                &evidence,
                &target,
                &limits,
                &ExecutionControl::unbounded(),
            )
            .await,
            Err(BundleError::ManifestTooLarge)
        ));
        assert!(!target.exists());
        assert_eq!(
            fs::read_dir(temporary.path())
                .expect("output parent")
                .count(),
            0
        );
    }
}
