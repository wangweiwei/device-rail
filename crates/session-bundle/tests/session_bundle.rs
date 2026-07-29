use std::{
    fs,
    future::pending,
    io::Cursor,
    path::{Path, PathBuf},
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
    time::Duration,
};

use async_trait::async_trait;
use devicerail_core::{
    CancellationReason, EvidenceError, EvidenceStore, ExecutionControl, ExecutionController,
    GcPolicy, PutEvidence, Sha256Digest,
};
use devicerail_evidence_fs::{FileEvidenceStore, FileEvidenceStoreConfig};
use devicerail_protocol::{
    ActionOutcome, ActionResult, AssetRef, DeviceId, EventId, EventSequence, Observation,
    ProtocolVersion, RecordedActionCall, RpcId, SessionExport, SessionId, SessionInfo,
    SessionOutcome, SessionState, TestEvent, TestEventPayload, Verdict, VerdictStatus, Viewport,
};
use devicerail_session_bundle::{
    BundleError, BundleEvidence, BundleEvidenceSource, BundleLimits, BundleManifest, BundleSource,
    ModelError, export_directory, from_canonical_slice, to_canonical_bytes, validate_directory,
    validate_manifest_events,
};
use serde_json::{Map, json};
use sha2::{Digest as _, Sha256};
use tempfile::TempDir;
use tokio::io::{AsyncRead, ReadBuf};
use tokio::sync::Notify;
use uuid::Uuid;

const MEDIA_TYPE: &str = "image/png";
const ASSET_BYTES: &[u8] = b"DeviceRail bundle integration fixture";
const STAGING_PREFIX: &str = ".devicerail-session-bundle-staging-";

#[tokio::test]
async fn file_evidence_export_deduplicates_typed_references_and_outlives_store_gc() {
    let temporary = TempDir::new().expect("temporary root");
    let evidence_root = temporary.path().join("evidence");
    let bundle = temporary.path().join("bundle");
    let bundle_copy = temporary.path().join("bundle-copy");
    let store = FileEvidenceStore::new(&evidence_root, FileEvidenceStoreConfig::default())
        .expect("open Evidence Store");
    let session_id = fixed_session_id();
    let stored = store
        .put(
            PutEvidence::new(session_id.clone(), MEDIA_TYPE).expect("valid put request"),
            Box::pin(Cursor::new(ASSET_BYTES.to_vec())),
        )
        .await
        .expect("store Evidence");
    let reference = stored.asset_ref();
    let source = source_with_reference(session_id.clone(), reference.clone());

    let summary = export_directory(
        &source,
        &store,
        &bundle,
        &BundleLimits::default(),
        &ExecutionControl::unbounded(),
    )
    .await
    .expect("export Bundle");

    assert_eq!(summary.asset_count, 1);
    assert_eq!(summary.asset_bytes, ASSET_BYTES.len() as u64);
    let digest = reference.sha256.as_deref().expect("canonical digest");
    let asset = bundle.join("assets").join("sha256").join(digest);
    assert_eq!(fs::read(&asset).expect("bundled asset"), ASSET_BYTES);
    assert_eq!(
        fs::read_dir(bundle.join("assets").join("sha256"))
            .expect("asset directory")
            .count(),
        1,
        "all five typed references must share one content-addressed file"
    );

    export_directory(
        &source,
        &store,
        &bundle_copy,
        &BundleLimits::default(),
        &ExecutionControl::unbounded(),
    )
    .await
    .expect("repeat deterministic export");
    assert_eq!(
        fs::read(bundle.join("manifest.json")).expect("first manifest"),
        fs::read(bundle_copy.join("manifest.json")).expect("second manifest")
    );
    assert_eq!(
        fs::read(&asset).expect("first asset"),
        fs::read(asset_path(&bundle_copy, digest)).expect("second asset")
    );

    let validated = validate_directory(
        &bundle,
        &BundleLimits::default(),
        &ExecutionControl::unbounded(),
    )
    .await
    .expect("validate Bundle");
    assert_eq!(validated.event_protocol_version, ProtocolVersion::new(1, 2));
    assert_eq!(validated.session_export, source.session_export);
    assert_eq!(validated.assets.len(), 1);

    let released = store
        .release_session(&session_id, 10_000)
        .await
        .expect("release Session Evidence");
    assert_eq!(released.released_references, 1);
    let collected = store
        .gc(GcPolicy::delete(u64::MAX))
        .await
        .expect("collect original Evidence");
    assert_eq!(collected.deleted_assets, 1);
    drop(store);
    fs::remove_dir_all(&evidence_root).expect("remove original Evidence Store");

    let replay = validate_directory(
        &bundle,
        &BundleLimits::default(),
        &ExecutionControl::unbounded(),
    )
    .await
    .expect("Bundle remains independently valid");
    assert_eq!(replay.session_export, source.session_export);
    assert_eq!(
        fs::read(asset).expect("independent bundled asset"),
        ASSET_BYTES
    );
}

#[tokio::test]
async fn validator_rejects_asset_and_manifest_tampering_and_unexpected_nodes() {
    let temporary = TempDir::new().expect("temporary root");
    let evidence_root = temporary.path().join("evidence");
    let store = FileEvidenceStore::new(&evidence_root, FileEvidenceStoreConfig::default())
        .expect("open Evidence Store");
    let session_id = fixed_session_id();
    let stored = store
        .put(
            PutEvidence::new(session_id.clone(), MEDIA_TYPE).expect("valid put request"),
            Box::pin(Cursor::new(ASSET_BYTES.to_vec())),
        )
        .await
        .expect("store Evidence");
    let reference = stored.asset_ref();
    let source = source_with_reference(session_id, reference.clone());
    let digest = reference.sha256.as_deref().expect("canonical digest");

    let tampered = export_fixture(temporary.path(), "tampered", &source, &store).await;
    let tampered_asset = asset_path(&tampered, digest);
    let mut bytes = fs::read(&tampered_asset).expect("read asset");
    bytes[0] ^= 0xff;
    fs::write(&tampered_asset, bytes).expect("tamper asset");
    assert!(matches!(
        validate(&tampered).await,
        Err(BundleError::EvidenceDigestMismatch)
    ));

    let truncated = export_fixture(temporary.path(), "truncated", &source, &store).await;
    fs::write(
        asset_path(&truncated, digest),
        &ASSET_BYTES[..ASSET_BYTES.len() - 1],
    )
    .expect("truncate asset");
    assert!(matches!(
        validate(&truncated).await,
        Err(BundleError::EvidenceSizeMismatch)
    ));

    let missing = export_fixture(temporary.path(), "missing", &source, &store).await;
    fs::remove_file(asset_path(&missing, digest)).expect("remove asset");
    assert!(matches!(
        validate(&missing).await,
        Err(BundleError::Filesystem(_))
    ));

    let extra = export_fixture(temporary.path(), "extra", &source, &store).await;
    fs::write(extra.join("unexpected"), b"extra").expect("write unexpected node");
    assert!(matches!(
        validate(&extra).await,
        Err(BundleError::Filesystem(_))
    ));

    let noncanonical = export_fixture(temporary.path(), "noncanonical", &source, &store).await;
    let manifest_path = noncanonical.join("manifest.json");
    let mut manifest_bytes = fs::read(&manifest_path).expect("read manifest");
    assert_eq!(manifest_bytes.pop(), Some(b'\n'));
    fs::write(&manifest_path, manifest_bytes).expect("remove canonical line feed");
    assert!(matches!(
        validate(&noncanonical).await,
        Err(BundleError::CanonicalJson(_))
    ));

    let bad_path = export_fixture(temporary.path(), "bad-path", &source, &store).await;
    mutate_manifest(&bad_path, |manifest| {
        manifest.assets[0].path = "../escape".to_owned();
    });
    assert!(matches!(
        validate(&bad_path).await,
        Err(BundleError::Model(ModelError::InvalidAssetPath))
    ));

    let bad_media = export_fixture(temporary.path(), "bad-media", &source, &store).await;
    mutate_manifest(&bad_media, |manifest| {
        manifest.assets[0].media_type = "application/octet-stream".to_owned();
    });
    assert!(matches!(
        validate(&bad_media).await,
        Err(BundleError::Model(ModelError::AssetMediaTypeMismatch))
    ));

    let duplicate = export_fixture(temporary.path(), "duplicate-index", &source, &store).await;
    mutate_manifest(&duplicate, |manifest| {
        manifest.assets.push(manifest.assets[0].clone());
    });
    assert!(matches!(
        validate(&duplicate).await,
        Err(BundleError::Model(ModelError::AssetIndexNotSorted))
    ));

    #[cfg(unix)]
    {
        use std::os::unix::fs::symlink;

        let symlinked = export_fixture(temporary.path(), "symlinked", &source, &store).await;
        let bundled_asset = asset_path(&symlinked, digest);
        let outside = temporary.path().join("outside-asset");
        fs::write(&outside, ASSET_BYTES).expect("write symlink target");
        fs::remove_file(&bundled_asset).expect("remove bundled asset");
        symlink(&outside, &bundled_asset).expect("replace asset with symlink");
        assert!(matches!(
            validate(&symlinked).await,
            Err(BundleError::Filesystem(_))
        ));
    }
}

#[tokio::test]
async fn export_rejects_existing_target_and_untrusted_reader_metadata_or_bytes() {
    let temporary = TempDir::new().expect("temporary root");
    let reference = reference_for(ASSET_BYTES, MEDIA_TYPE);
    let source = source_with_reference(fixed_session_id(), reference);

    let existing = temporary.path().join("existing");
    fs::create_dir(&existing).expect("create existing target");
    fs::write(existing.join("marker"), b"preserve").expect("write marker");
    let correct = StaticEvidence::new(MEDIA_TYPE, ASSET_BYTES.len() as u64, ASSET_BYTES);
    assert!(matches!(
        export(&source, &correct, &existing).await,
        Err(BundleError::TargetExists)
    ));
    assert_eq!(
        fs::read(existing.join("marker")).expect("marker"),
        b"preserve"
    );

    let wrong_media_target = temporary.path().join("wrong-media");
    let wrong_media = StaticEvidence::new(
        "application/octet-stream",
        ASSET_BYTES.len() as u64,
        ASSET_BYTES,
    );
    assert!(matches!(
        export(&source, &wrong_media, &wrong_media_target).await,
        Err(BundleError::EvidenceMetadataMismatch)
    ));
    assert!(!wrong_media_target.exists());

    let wrong_size_target = temporary.path().join("wrong-size");
    let wrong_size = StaticEvidence::new(MEDIA_TYPE, ASSET_BYTES.len() as u64 + 1, ASSET_BYTES);
    assert!(matches!(
        export(&source, &wrong_size, &wrong_size_target).await,
        Err(BundleError::EvidenceSizeMismatch)
    ));
    assert!(!wrong_size_target.exists());

    let wrong_hash_target = temporary.path().join("wrong-hash");
    let mut wrong_bytes = ASSET_BYTES.to_vec();
    wrong_bytes[0] ^= 0xff;
    let wrong_hash = StaticEvidence::new(MEDIA_TYPE, wrong_bytes.len() as u64, &wrong_bytes);
    assert!(matches!(
        export(&source, &wrong_hash, &wrong_hash_target).await,
        Err(BundleError::EvidenceDigestMismatch)
    ));
    assert!(!wrong_hash_target.exists());
    assert_no_staging(temporary.path());
}

#[tokio::test]
async fn cancellation_during_evidence_open_removes_target_and_staging() {
    let temporary = TempDir::new().expect("temporary root");
    let target = temporary.path().join("cancelled");
    let source = source_with_reference(fixed_session_id(), reference_for(ASSET_BYTES, MEDIA_TYPE));
    let entered = Arc::new(Notify::new());
    let evidence = BlockingEvidence {
        entered: Arc::clone(&entered),
    };
    let (controller, control) = ExecutionController::new();
    let target_for_task = target.clone();
    let task = tokio::spawn(async move {
        export_directory(
            &source,
            &evidence,
            &target_for_task,
            &BundleLimits::default(),
            &control,
        )
        .await
    });

    entered.notified().await;
    assert!(controller.cancel(CancellationReason::Requested));
    let result = tokio::time::timeout(Duration::from_secs(5), task)
        .await
        .expect("cancelled export must finish")
        .expect("export task must not panic");
    assert!(matches!(result, Err(BundleError::Cancelled { .. })));
    assert!(!target.exists());
    assert_no_staging(temporary.path());
}

#[tokio::test]
async fn cancellation_and_timeout_during_asset_copy_publish_nothing() {
    for timed in [false, true] {
        let temporary = TempDir::new().expect("temporary root");
        let target = temporary.path().join("interrupted");
        let source =
            source_with_reference(fixed_session_id(), reference_for(ASSET_BYTES, MEDIA_TYPE));
        let blocked = Arc::new(Notify::new());
        let evidence = MidCopyEvidence {
            blocked: Arc::clone(&blocked),
        };
        let (controller, control) = if timed {
            ExecutionController::with_timeout(1_000, devicerail_core::TimeoutScope::Request)
        } else {
            ExecutionController::new()
        };
        let target_for_task = target.clone();
        let task = tokio::spawn(async move {
            export_directory(
                &source,
                &evidence,
                &target_for_task,
                &BundleLimits::default(),
                &control,
            )
            .await
        });

        tokio::time::timeout(Duration::from_secs(5), blocked.notified())
            .await
            .expect("copy must reach its controlled blocking read");
        if !timed {
            assert!(controller.cancel(CancellationReason::Requested));
        }
        let result = tokio::time::timeout(Duration::from_secs(5), task)
            .await
            .expect("interrupted copy must finish")
            .expect("export task must not panic");
        if timed {
            assert!(matches!(result, Err(BundleError::TimedOut { .. })));
        } else {
            assert!(matches!(result, Err(BundleError::Cancelled { .. })));
        }
        assert!(!target.exists());
        assert_no_staging(temporary.path());
    }
}

#[tokio::test]
async fn concurrent_writers_publish_exactly_one_bundle_without_staging_leaks() {
    let temporary = TempDir::new().expect("temporary root");
    let target = temporary.path().join("contended");
    let source = source_with_reference(fixed_session_id(), reference_for(ASSET_BYTES, MEDIA_TYPE));
    let evidence = StaticEvidence::new(MEDIA_TYPE, ASSET_BYTES.len() as u64, ASSET_BYTES);
    let limits = BundleLimits::default();
    let first_control = ExecutionControl::unbounded();
    let second_control = ExecutionControl::unbounded();

    let (first, second) = tokio::join!(
        export_directory(&source, &evidence, &target, &limits, &first_control),
        export_directory(&source, &evidence, &target, &limits, &second_control),
    );

    let successes = usize::from(first.is_ok()) + usize::from(second.is_ok());
    let target_exists = usize::from(matches!(first, Err(BundleError::TargetExists)))
        + usize::from(matches!(second, Err(BundleError::TargetExists)));
    assert_eq!(successes, 1);
    assert_eq!(target_exists, 1);
    validate(&target).await.expect("winning Bundle is valid");
    assert_no_staging(temporary.path());
}

#[test]
fn checked_in_protected_success_fixture_is_canonical_valid_and_asset_free() {
    let bytes = include_bytes!("fixtures/protected-omission/manifest.json");
    let manifest: BundleManifest = from_canonical_slice(bytes).expect("canonical fixture");
    let references = validate_manifest_events(&manifest, &BundleLimits::default())
        .expect("valid protected-success fixture");

    assert!(manifest.assets.is_empty());
    assert!(references.is_empty());
    let assets =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/protected-omission/assets");
    assert!(!assets.exists(), "zero-asset fixture must omit assets/");
}

struct StaticEvidence {
    media_type: String,
    byte_length: u64,
    bytes: Vec<u8>,
}

impl StaticEvidence {
    fn new(media_type: &str, byte_length: u64, bytes: &[u8]) -> Self {
        Self {
            media_type: media_type.to_owned(),
            byte_length,
            bytes: bytes.to_vec(),
        }
    }
}

#[async_trait]
impl BundleEvidenceSource for StaticEvidence {
    async fn open_asset(&self, _reference: &AssetRef) -> Result<BundleEvidence, EvidenceError> {
        Ok(BundleEvidence {
            media_type: self.media_type.clone(),
            byte_length: self.byte_length,
            reader: Box::pin(Cursor::new(self.bytes.clone())),
        })
    }
}

struct BlockingEvidence {
    entered: Arc<Notify>,
}

struct MidCopyEvidence {
    blocked: Arc<Notify>,
}

#[async_trait]
impl BundleEvidenceSource for MidCopyEvidence {
    async fn open_asset(&self, _reference: &AssetRef) -> Result<BundleEvidence, EvidenceError> {
        Ok(BundleEvidence {
            media_type: MEDIA_TYPE.to_owned(),
            byte_length: ASSET_BYTES.len() as u64,
            reader: Box::pin(BlockAfterChunk {
                first: Some(ASSET_BYTES[..8].to_vec()),
                blocked: Arc::clone(&self.blocked),
                notified: false,
            }),
        })
    }
}

struct BlockAfterChunk {
    first: Option<Vec<u8>>,
    blocked: Arc<Notify>,
    notified: bool,
}

impl AsyncRead for BlockAfterChunk {
    fn poll_read(
        mut self: Pin<&mut Self>,
        _context: &mut Context<'_>,
        buffer: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        if let Some(first) = self.first.take() {
            buffer.put_slice(&first);
            return Poll::Ready(Ok(()));
        }
        if !self.notified {
            self.notified = true;
            self.blocked.notify_one();
        }
        Poll::Pending
    }
}

#[async_trait]
impl BundleEvidenceSource for BlockingEvidence {
    async fn open_asset(&self, _reference: &AssetRef) -> Result<BundleEvidence, EvidenceError> {
        self.entered.notify_one();
        pending::<()>().await;
        unreachable!("blocking Evidence source only finishes through cancellation")
    }
}

fn fixed_session_id() -> SessionId {
    SessionId::from(Uuid::from_u128(0x11111111_1111_4111_8111_111111111111))
}

fn source_with_reference(session_id: SessionId, reference: AssetRef) -> BundleSource {
    let device_id = DeviceId::new("android-emulator-5554");
    let call_id = Uuid::from_u128(0x22222222_2222_4222_8222_222222222222);
    let request_id = Some(RpcId::String("bundle-action".to_owned()));
    let events = vec![
        event(
            &session_id,
            1,
            100,
            None,
            None,
            TestEventPayload::SessionStarted,
        ),
        event(
            &session_id,
            2,
            200,
            None,
            Some(device_id.clone()),
            TestEventPayload::ObservationCaptured {
                observation: Box::new(observation(&device_id, 200, Some(reference.clone()), 1)),
            },
        ),
        event(
            &session_id,
            3,
            300,
            request_id.clone(),
            Some(device_id.clone()),
            TestEventPayload::ActionStarted {
                call: RecordedActionCall {
                    id: call_id,
                    name: "tap".to_owned(),
                    arguments: json!({"x": 10, "y": 20}),
                    arguments_redacted: false,
                },
            },
        ),
        event(
            &session_id,
            4,
            400,
            request_id,
            Some(device_id.clone()),
            TestEventPayload::ActionCompleted {
                call_id,
                outcome: ActionOutcome::Succeeded {
                    result: Box::new(ActionResult {
                        call_id,
                        started_at_ms: 300,
                        finished_at_ms: 400,
                        output: json!({"tapped": true}),
                        before: Some(observation(&device_id, 300, Some(reference.clone()), 2)),
                        after: Some(observation(&device_id, 400, Some(reference.clone()), 3)),
                        evidence: vec![reference.clone()],
                        execution: None,
                    }),
                },
            },
        ),
        event(
            &session_id,
            5,
            500,
            None,
            Some(device_id),
            TestEventPayload::VerdictRecorded {
                verdict: Verdict {
                    status: VerdictStatus::Pass,
                    summary: "all checks passed".to_owned(),
                    evidence: vec![reference],
                },
            },
        ),
        event(
            &session_id,
            6,
            600,
            None,
            None,
            TestEventPayload::SessionEnded {
                outcome: SessionOutcome::Completed,
                reason: None,
            },
        ),
    ];
    let count = EventSequence::new(events.len() as u64).expect("nonzero event count");
    BundleSource {
        event_protocol_version: ProtocolVersion::new(1, 2),
        session_export: SessionExport {
            session: SessionInfo {
                id: session_id,
                state: SessionState::Ended,
                started_at_ms: 100,
                ended_at_ms: Some(600),
                event_count: count,
                last_sequence: count,
            },
            events,
        },
    }
}

fn observation(
    device_id: &DeviceId,
    captured_at_ms: u64,
    screenshot: Option<AssetRef>,
    id: u128,
) -> Observation {
    Observation {
        id: Uuid::from_u128(id),
        device_id: device_id.clone(),
        captured_at_ms,
        viewport: Viewport {
            width: 1080,
            height: 1920,
            scale_factor: 3.0,
        },
        screenshot,
        screenshot_omission: None,
        ui_snapshot: None,
        ui_snapshot_omission: None,
        metadata: Map::new(),
    }
}

fn event(
    session_id: &SessionId,
    sequence: u64,
    at_ms: u64,
    request_id: Option<RpcId>,
    device_id: Option<DeviceId>,
    payload: TestEventPayload,
) -> TestEvent {
    TestEvent {
        event_id: EventId::from(Uuid::from_u128(
            0x90000000_0000_4000_8000_000000000000 + sequence as u128,
        )),
        session_id: session_id.clone(),
        sequence: EventSequence::new(sequence).expect("nonzero sequence"),
        request_id,
        device_id,
        at_ms,
        payload,
    }
}

fn reference_for(bytes: &[u8], media_type: &str) -> AssetRef {
    let digest = hex::encode(Sha256::digest(bytes));
    let digest = Sha256Digest::parse(digest).expect("SHA-256 is canonical");
    AssetRef {
        id: digest.asset_id(),
        media_type: media_type.to_owned(),
        uri: digest.asset_uri(),
        sha256: Some(digest.to_string()),
    }
}

async fn export_fixture(
    parent: &Path,
    name: &str,
    source: &BundleSource,
    evidence: &dyn BundleEvidenceSource,
) -> PathBuf {
    let target = parent.join(name);
    export(source, evidence, &target)
        .await
        .expect("export mutation fixture");
    target
}

async fn export(
    source: &BundleSource,
    evidence: &dyn BundleEvidenceSource,
    target: &Path,
) -> Result<devicerail_session_bundle::BundleSummary, BundleError> {
    export_directory(
        source,
        evidence,
        target,
        &BundleLimits::default(),
        &ExecutionControl::unbounded(),
    )
    .await
}

async fn validate(root: &Path) -> Result<devicerail_session_bundle::ValidatedBundle, BundleError> {
    validate_directory(
        root,
        &BundleLimits::default(),
        &ExecutionControl::unbounded(),
    )
    .await
}

fn asset_path(bundle: &Path, digest: &str) -> PathBuf {
    bundle.join("assets").join("sha256").join(digest)
}

fn mutate_manifest(bundle: &Path, mutate: impl FnOnce(&mut BundleManifest)) {
    let path = bundle.join("manifest.json");
    let bytes = fs::read(&path).expect("read manifest");
    let mut manifest: BundleManifest =
        from_canonical_slice(&bytes).expect("parse canonical manifest");
    mutate(&mut manifest);
    fs::write(
        path,
        to_canonical_bytes(&manifest).expect("encode canonical manifest"),
    )
    .expect("write mutated manifest");
}

fn assert_no_staging(parent: &Path) {
    let staging = fs::read_dir(parent)
        .expect("read output parent")
        .filter_map(Result::ok)
        .filter(|entry| {
            entry
                .file_name()
                .to_string_lossy()
                .starts_with(STAGING_PREFIX)
        })
        .map(|entry| entry.path())
        .collect::<Vec<_>>();
    assert!(
        staging.is_empty(),
        "staging directories leaked: {staging:?}"
    );
}
