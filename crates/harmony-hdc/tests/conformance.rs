use std::sync::{Arc, Mutex as StdMutex};

use async_trait::async_trait;
use devicerail_core::{
    DeviceDriver, DeviceRuntime, DriverError, EvidenceInput, EvidenceMetadata, EvidenceOutput,
    EvidenceResult, EvidenceStore, ExecutionControl, GcPolicy, GcReport, MemoryEventStore,
    OperationContext, PutEvidence, ReleaseReport, RuntimeError, SessionEventStore, Sha256Digest,
    StartSession, StoredEvidence, now_ms,
};
use devicerail_evidence_fs::{FileEvidenceStore, FileEvidenceStoreConfig};
use devicerail_harmony_hdc::{
    DiscoveredHarmonyDevice, HarmonyHdc, HarmonyHdcResult, HdcCommand, HdcCommandOutput,
    HdcCommandRunner, HdcOperation, HdcTarget, HdcTargetState,
};
use devicerail_protocol::{ActionCall, ActionDefinition, AssetRef, SessionId};
use serde_json::json;
use tempfile::TempDir;
use uuid::Uuid;

struct FakeHdcRunner {
    operations: StdMutex<Vec<&'static str>>,
}

impl FakeHdcRunner {
    fn new() -> Self {
        Self {
            operations: StdMutex::new(Vec::new()),
        }
    }

    fn operations(&self) -> Vec<&'static str> {
        self.operations.lock().expect("fake operation log").clone()
    }
}

#[async_trait]
impl HdcCommandRunner for FakeHdcRunner {
    async fn run(
        &self,
        command: HdcCommand,
        _control: &devicerail_core::ExecutionControl,
    ) -> HarmonyHdcResult<HdcCommandOutput> {
        self.operations
            .lock()
            .expect("fake operation log")
            .push(command.operation().name());
        let stdout = match command.operation() {
            HdcOperation::ListTargetsVerbose => b"FMR022 connected devName:Conformance\n".to_vec(),
            HdcOperation::Probe => b"devicerail\n".to_vec(),
            HdcOperation::GetProperty(devicerail_harmony_hdc::HdcProperty::ProductModel) => {
                b"Harmony Conformance Device\n".to_vec()
            }
            HdcOperation::GetProperty(devicerail_harmony_hdc::HdcProperty::SoftwareVersion) => {
                b"5.0.0\n".to_vec()
            }
            HdcOperation::CaptureScreenshot => fixture_png(),
            HdcOperation::DumpLayout => {
                br#"{"root":{"type":"Window","bounds":"[0,0][2,2]","children":[]}}"#.to_vec()
            }
            HdcOperation::Tap { .. }
            | HdcOperation::Swipe { .. }
            | HdcOperation::InputText(_)
            | HdcOperation::KeyPress(_)
            | HdcOperation::Launch { .. } => Vec::new(),
        };
        Ok(HdcCommandOutput::new(stdout, Vec::new()))
    }
}

fn fixture_png() -> Vec<u8> {
    let mut bytes = Vec::new();
    let mut encoder = png::Encoder::new(&mut bytes, 2, 2);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    let mut writer = encoder.write_header().expect("PNG header");
    writer
        .write_image_data(&[
            0x22, 0x44, 0x66, 0xff, 0x22, 0x44, 0x66, 0xff, 0x22, 0x44, 0x66, 0xff, 0x22, 0x44,
            0x66, 0xff,
        ])
        .expect("PNG image");
    writer.finish().expect("PNG finish");
    bytes
}

fn descriptor() -> DiscoveredHarmonyDevice {
    DiscoveredHarmonyDevice {
        target: HdcTarget::parse(format!("FMR{}", Uuid::new_v4().simple())).expect("target"),
        state: HdcTargetState::Ready,
        name: Some("Harmony Conformance Device".to_owned()),
        os_version: None,
        extensions: Default::default(),
    }
}

fn fixture_driver() -> devicerail_harmony_hdc::HarmonyHdcDriver {
    HarmonyHdc::with_runner(Arc::new(FakeHdcRunner::new())).driver(descriptor())
}

fn conformance_call(action: &ActionDefinition) -> Result<ActionCall, String> {
    let arguments = match action.name.as_str() {
        "tap" => json!({ "x": 1, "y": 1 }),
        "swipe" => json!({
            "startX": 0, "startY": 0, "endX": 1, "endY": 1, "durationMs": 100
        }),
        "inputText" => json!({ "text": "DeviceRail 31" }),
        "keyPress" => json!({ "key": "enter" }),
        "launch" => json!({
            "bundleName": "com.example.app", "abilityName": "EntryAbility"
        }),
        name => return Err(format!("no HarmonyOS conformance fixture for `{name}`")),
    };
    Ok(ActionCall {
        id: Uuid::new_v4(),
        name: action.name.clone(),
        arguments,
    })
}

struct TemporaryEvidenceStore {
    inner: FileEvidenceStore,
    _root: TempDir,
}

impl TemporaryEvidenceStore {
    fn create() -> Arc<dyn EvidenceStore> {
        let root = tempfile::tempdir().expect("temporary Evidence Store root");
        let inner = FileEvidenceStore::new(root.path(), FileEvidenceStoreConfig::default())
            .expect("temporary Evidence Store");
        Arc::new(Self { inner, _root: root })
    }
}

#[async_trait]
impl EvidenceStore for TemporaryEvidenceStore {
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

devicerail_core::driver_conformance_test!(
    conforms_to_shared_driver_contract,
    fixture_driver,
    conformance_call,
    TemporaryEvidenceStore::create(),
);

#[tokio::test]
async fn injectable_runner_drives_typed_discovery() {
    let hdc = HarmonyHdc::with_runner(Arc::new(FakeHdcRunner::new()));
    let report = hdc
        .discover(&devicerail_core::ExecutionControl::unbounded())
        .await
        .expect("discovery");
    assert_eq!(report.devices.len(), 1);
    assert_eq!(report.devices[0].target.as_str(), "FMR022");
    assert!(matches!(report.devices[0].state, HdcTargetState::Ready));
}

#[tokio::test]
async fn coordinate_actions_use_the_real_before_observation_viewport() {
    let runner = Arc::new(FakeHdcRunner::new());
    let driver = Arc::new(
        HarmonyHdc::with_runner(Arc::clone(&runner) as Arc<dyn HdcCommandRunner>)
            .driver(descriptor()),
    );
    driver
        .connect(&ExecutionControl::unbounded())
        .await
        .expect("connect HarmonyOS Driver");
    let events = Arc::new(MemoryEventStore::default());
    let session = events
        .start_session(StartSession::new(None, Some(driver.id().clone()), now_ms()))
        .await
        .expect("start Session");
    let context = OperationContext::new(session.id, None);
    let runtime = DeviceRuntime::with_evidence(
        Arc::clone(&driver),
        events,
        TemporaryEvidenceStore::create(),
    );

    for (expected_action, arguments) in [
        ("tap", json!({ "x": 2, "y": 0 })),
        (
            "swipe",
            json!({
                "startX": 0,
                "startY": 0,
                "endX": 1,
                "endY": 2,
                "durationMs": 100
            }),
        ),
    ] {
        let error = runtime
            .execute(
                &context,
                ActionCall {
                    id: Uuid::new_v4(),
                    name: expected_action.to_owned(),
                    arguments,
                },
            )
            .await
            .expect_err("coordinate outside the before viewport must be rejected");
        assert!(matches!(
            error,
            RuntimeError::Driver(DriverError::InvalidArguments { ref action, .. })
                if action == expected_action
        ));
    }

    let operations = runner.operations();
    assert!(!operations.contains(&"tap"));
    assert!(!operations.contains(&"swipe"));
    assert_eq!(
        operations
            .iter()
            .filter(|operation| **operation == "dump_layout")
            .count(),
        2,
        "each rejected action still uses a real before observation"
    );
}
