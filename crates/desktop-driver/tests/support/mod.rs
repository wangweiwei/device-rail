#![allow(dead_code)]

use std::sync::{
    Arc, Mutex,
    atomic::{AtomicUsize, Ordering},
};

use async_trait::async_trait;
use devicerail_core::{
    EvidenceInput, EvidenceMetadata, EvidenceOutput, EvidenceResult, EvidenceStore,
    ExecutionControl, GcPolicy, GcReport, PutEvidence, ReleaseReport, Sha256Digest, StoredEvidence,
};
use devicerail_evidence_fs::{FileEvidenceStore, FileEvidenceStoreConfig};
use devicerail_protocol::{ActionCall, ActionDefinition, AssetRef, SessionId, Viewport};
use serde_json::json;
use tempfile::TempDir;
use uuid::Uuid;

use devicerail_desktop_driver::{
    DesktopAction, DesktopBackend, DesktopCapture, DesktopIdentity, DesktopProbe, DesktopProfile,
    DesktopResult,
};

pub const TEST_PNG: [u8; 70] = [
    0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x48, 0x44, 0x52,
    0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00, 0x00, 0x1f, 0x15, 0xc4,
    0x89, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x44, 0x41, 0x54, 0x78, 0xda, 0x63, 0x08, 0x9a, 0xf3, 0xea,
    0x3f, 0x00, 0x05, 0xf4, 0x02, 0xd8, 0x8d, 0xe4, 0x08, 0x21, 0x00, 0x00, 0x00, 0x00, 0x49, 0x45,
    0x4e, 0x44, 0xae, 0x42, 0x60, 0x82,
];

pub fn test_viewport() -> Viewport {
    Viewport {
        width: 1,
        height: 1,
        scale_factor: 1.0,
    }
}

pub struct FakeBackend {
    profile: DesktopProfile,
    probe_viewports: Vec<Viewport>,
    captures: AtomicUsize,
    probes: AtomicUsize,
    actions: Mutex<Vec<DesktopAction>>,
}

impl FakeBackend {
    pub fn new(profile: DesktopProfile) -> Self {
        Self {
            profile,
            probe_viewports: vec![test_viewport()],
            captures: AtomicUsize::new(0),
            probes: AtomicUsize::new(0),
            actions: Mutex::new(Vec::new()),
        }
    }

    pub fn with_probe_viewports(profile: DesktopProfile, probe_viewports: Vec<Viewport>) -> Self {
        assert!(!probe_viewports.is_empty(), "at least one probe viewport");
        Self {
            profile,
            probe_viewports,
            captures: AtomicUsize::new(0),
            probes: AtomicUsize::new(0),
            actions: Mutex::new(Vec::new()),
        }
    }

    pub fn capture_count(&self) -> usize {
        self.captures.load(Ordering::SeqCst)
    }

    pub fn probe_count(&self) -> usize {
        self.probes.load(Ordering::SeqCst)
    }

    pub fn actions(&self) -> Vec<DesktopAction> {
        self.actions.lock().expect("fake actions").clone()
    }
}

#[async_trait]
impl DesktopBackend for FakeBackend {
    fn profile(&self) -> &DesktopProfile {
        &self.profile
    }

    async fn probe(&self, _control: &ExecutionControl) -> DesktopResult<DesktopProbe> {
        let index = self.probes.fetch_add(1, Ordering::SeqCst);
        let viewport = self
            .probe_viewports
            .get(index)
            .or_else(|| self.probe_viewports.last())
            .expect("fake backend has a viewport")
            .clone();
        DesktopProbe::new(self.profile.clone(), viewport)
    }

    async fn capture(&self, _control: &ExecutionControl) -> DesktopResult<DesktopCapture> {
        self.captures.fetch_add(1, Ordering::SeqCst);
        DesktopCapture::new(TEST_PNG.to_vec(), test_viewport())
    }

    async fn execute(
        &self,
        action: DesktopAction,
        _control: &ExecutionControl,
    ) -> DesktopResult<()> {
        self.actions.lock().expect("fake actions").push(action);
        Ok(())
    }
}

pub fn identity(platform: &str) -> DesktopIdentity {
    DesktopIdentity::new(
        format!("desktop-{platform}-{}", Uuid::new_v4()),
        format!("DeviceRail {platform} test desktop"),
        Some("test-1".to_owned()),
    )
}

pub fn valid_call(action: &ActionDefinition) -> Result<ActionCall, String> {
    let arguments = match action.name.as_str() {
        "tap" => json!({ "x": 0, "y": 0 }),
        "inputText" => json!({ "text": "DeviceRail" }),
        "keyPress" => json!({ "key": "enter" }),
        "scroll" => json!({ "deltaX": 0, "deltaY": 120 }),
        name => return Err(format!("no desktop action fixture for `{name}`")),
    };
    Ok(ActionCall {
        id: Uuid::new_v4(),
        name: action.name.clone(),
        arguments,
    })
}

pub struct IsolatedEvidenceStore {
    inner: FileEvidenceStore,
    _root: TempDir,
}

impl IsolatedEvidenceStore {
    fn new() -> Self {
        let root = tempfile::tempdir().expect("temporary desktop evidence root");
        let inner = FileEvidenceStore::new(root.path(), FileEvidenceStoreConfig::default())
            .expect("desktop Evidence Store");
        Self { inner, _root: root }
    }
}

#[async_trait]
impl EvidenceStore for IsolatedEvidenceStore {
    async fn put(
        &self,
        request: PutEvidence,
        input: EvidenceInput,
    ) -> EvidenceResult<StoredEvidence> {
        self.inner.put(request, input).await
    }

    async fn attach(
        &self,
        session_id: &SessionId,
        asset: &AssetRef,
    ) -> EvidenceResult<StoredEvidence> {
        self.inner.attach(session_id, asset).await
    }

    async fn verify_session_reference(
        &self,
        session_id: &SessionId,
        asset: &AssetRef,
    ) -> EvidenceResult<EvidenceMetadata> {
        self.inner.verify_session_reference(session_id, asset).await
    }

    async fn open(&self, digest: &Sha256Digest) -> EvidenceResult<EvidenceOutput> {
        self.inner.open(digest).await
    }

    async fn metadata(&self, digest: &Sha256Digest) -> EvidenceResult<EvidenceMetadata> {
        self.inner.metadata(digest).await
    }

    async fn referenced_sessions(&self) -> EvidenceResult<Vec<SessionId>> {
        self.inner.referenced_sessions().await
    }

    async fn release_session(
        &self,
        session_id: &SessionId,
        released_at_ms: u64,
    ) -> EvidenceResult<ReleaseReport> {
        self.inner.release_session(session_id, released_at_ms).await
    }

    async fn gc(&self, policy: GcPolicy) -> EvidenceResult<GcReport> {
        self.inner.gc(policy).await
    }
}

pub fn isolated_evidence_store() -> Arc<dyn EvidenceStore> {
    Arc::new(IsolatedEvidenceStore::new())
}

pub async fn assert_png_is_readable(store: &dyn EvidenceStore, asset: &AssetRef) {
    let digest = Sha256Digest::parse(asset.sha256.as_deref().expect("screenshot digest"))
        .expect("valid screenshot digest");
    let mut output = store.open(&digest).await.expect("open screenshot");
    let mut bytes = Vec::new();
    tokio::io::AsyncReadExt::read_to_end(&mut output, &mut bytes)
        .await
        .expect("read screenshot");
    assert_eq!(bytes, TEST_PNG);
}
