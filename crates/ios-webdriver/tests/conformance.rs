use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};

use async_trait::async_trait;
use devicerail_core::{
    DeviceDriver, DeviceRuntime, DriverError, EvidenceInput, EvidenceMetadata, EvidenceOutput,
    EvidenceResult, EvidenceStore, ExecutionControl, GcPolicy, GcReport, MemoryEventStore,
    OperationContext, PutEvidence, ReleaseReport, RuntimeError, SessionEventStore, Sha256Digest,
    StartSession, StoredEvidence, now_ms,
};
use devicerail_evidence_fs::{FileEvidenceStore, FileEvidenceStoreConfig};
use devicerail_ios_webdriver::{
    IosDeviceConfig, IosDriver, WdaAction, WdaPage, WdaSession, WdaStatus, WdaTransport,
};
use devicerail_protocol::{ActionCall, ActionDefinition, AssetRef, SessionId, Viewport};
use serde_json::json;
use tempfile::TempDir;
use uuid::Uuid;

struct FakeWda {
    sequence: Mutex<u64>,
    inspections: AtomicUsize,
    actions: Mutex<Vec<WdaAction>>,
    generation: AtomicUsize,
    create_attempts: AtomicUsize,
    probe_attempts: AtomicUsize,
    delete_attempts: AtomicUsize,
    perform_attempts: AtomicUsize,
    active_session: Mutex<Option<String>>,
    next_create_failure: Mutex<Option<&'static str>>,
    next_delete_failure: Mutex<Option<&'static str>>,
    next_perform_failure: Mutex<Option<&'static str>>,
    delete_returns_invalid_session: AtomicBool,
}

impl FakeWda {
    fn new() -> Self {
        Self {
            sequence: Mutex::new(0),
            inspections: AtomicUsize::new(0),
            actions: Mutex::new(Vec::new()),
            generation: AtomicUsize::new(1),
            create_attempts: AtomicUsize::new(0),
            probe_attempts: AtomicUsize::new(0),
            delete_attempts: AtomicUsize::new(0),
            perform_attempts: AtomicUsize::new(0),
            active_session: Mutex::new(None),
            next_create_failure: Mutex::new(None),
            next_delete_failure: Mutex::new(None),
            next_perform_failure: Mutex::new(None),
            delete_returns_invalid_session: AtomicBool::new(false),
        }
    }

    fn actions(&self) -> Vec<WdaAction> {
        self.actions.lock().expect("actions lock").clone()
    }

    fn inspection_count(&self) -> usize {
        self.inspections.load(Ordering::SeqCst)
    }

    fn restart(&self) {
        self.generation.fetch_add(1, Ordering::SeqCst);
        *self.active_session.lock().expect("active Session lock") = None;
    }

    fn create_count(&self) -> usize {
        self.create_attempts.load(Ordering::SeqCst)
    }

    fn probe_count(&self) -> usize {
        self.probe_attempts.load(Ordering::SeqCst)
    }

    fn delete_count(&self) -> usize {
        self.delete_attempts.load(Ordering::SeqCst)
    }

    fn perform_count(&self) -> usize {
        self.perform_attempts.load(Ordering::SeqCst)
    }

    fn fail_next_create(&self, code: &'static str) {
        *self
            .next_create_failure
            .lock()
            .expect("create failure lock") = Some(code);
    }

    fn fail_next_perform(&self, code: &'static str) {
        *self
            .next_perform_failure
            .lock()
            .expect("perform failure lock") = Some(code);
    }

    fn fail_next_delete(&self, code: &'static str) {
        *self
            .next_delete_failure
            .lock()
            .expect("delete failure lock") = Some(code);
    }

    fn return_invalid_session_on_delete(&self) {
        self.delete_returns_invalid_session
            .store(true, Ordering::SeqCst);
    }

    fn invalid_session() -> DriverError {
        DriverError::Platform {
            code: "wda_invalid_session".to_owned(),
            retryable: false,
        }
    }

    fn assert_live_session(&self, session: &WdaSession) -> devicerail_core::DriverResult<()> {
        if self
            .active_session
            .lock()
            .expect("active Session lock")
            .as_deref()
            == Some(session.as_str())
        {
            Ok(())
        } else {
            Err(Self::invalid_session())
        }
    }
}

#[async_trait]
impl WdaTransport for FakeWda {
    async fn status(
        &self,
        _control: &ExecutionControl,
    ) -> devicerail_core::DriverResult<WdaStatus> {
        Ok(WdaStatus {
            ready: true,
            os_version: Some("18.0".to_owned()),
        })
    }

    async fn create_session(
        &self,
        _control: &ExecutionControl,
    ) -> devicerail_core::DriverResult<WdaSession> {
        self.create_attempts.fetch_add(1, Ordering::SeqCst);
        if let Some(code) = self
            .next_create_failure
            .lock()
            .expect("create failure lock")
            .take()
        {
            return Err(DriverError::Platform {
                code: code.to_owned(),
                retryable: false,
            });
        }
        let generation = self.generation.load(Ordering::SeqCst);
        let session = WdaSession::parse(format!("fake-session-{generation}"))?;
        *self.active_session.lock().expect("active Session lock") =
            Some(session.as_str().to_owned());
        Ok(session)
    }

    async fn delete_session(
        &self,
        session: &WdaSession,
        _control: &ExecutionControl,
    ) -> devicerail_core::DriverResult<()> {
        self.delete_attempts.fetch_add(1, Ordering::SeqCst);
        if let Some(code) = self
            .next_delete_failure
            .lock()
            .expect("delete failure lock")
            .take()
        {
            return Err(DriverError::Platform {
                code: code.to_owned(),
                retryable: false,
            });
        }
        if self
            .delete_returns_invalid_session
            .swap(false, Ordering::SeqCst)
        {
            *self.active_session.lock().expect("active Session lock") = None;
            return Err(Self::invalid_session());
        }
        self.assert_live_session(session)?;
        *self.active_session.lock().expect("active Session lock") = None;
        Ok(())
    }

    async fn probe_session(
        &self,
        session: &WdaSession,
        _control: &ExecutionControl,
    ) -> devicerail_core::DriverResult<()> {
        self.probe_attempts.fetch_add(1, Ordering::SeqCst);
        self.assert_live_session(session)
    }

    async fn inspect(
        &self,
        session: &WdaSession,
        _control: &ExecutionControl,
    ) -> devicerail_core::DriverResult<WdaPage> {
        self.assert_live_session(session)?;
        self.inspections.fetch_add(1, Ordering::SeqCst);
        let sequence = *self.sequence.lock().expect("sequence lock");
        Ok(WdaPage {
            source: format!("<App sequence=\"{sequence}\"/>"),
            viewport: Viewport {
                width: 1,
                height: 1,
                scale_factor: 1.0,
            },
        })
    }

    async fn screenshot_png(
        &self,
        session: &WdaSession,
        _control: &ExecutionControl,
    ) -> devicerail_core::DriverResult<Vec<u8>> {
        self.assert_live_session(session)?;
        Ok(fixture_png())
    }

    async fn perform(
        &self,
        session: &WdaSession,
        action: WdaAction,
        _control: &ExecutionControl,
    ) -> devicerail_core::DriverResult<()> {
        self.perform_attempts.fetch_add(1, Ordering::SeqCst);
        self.assert_live_session(session)?;
        if let Some(code) = self
            .next_perform_failure
            .lock()
            .expect("perform failure lock")
            .take()
        {
            return Err(DriverError::Platform {
                code: code.to_owned(),
                retryable: false,
            });
        }
        self.actions.lock().expect("actions lock").push(action);
        *self.sequence.lock().expect("sequence lock") += 1;
        Ok(())
    }
}

fn fixture_png() -> Vec<u8> {
    let mut bytes = Vec::new();
    let mut encoder = png::Encoder::new(&mut bytes, 1, 1);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    let mut writer = encoder.write_header().expect("PNG header");
    writer
        .write_image_data(&[0x33, 0x66, 0x99, 0xff])
        .expect("PNG image");
    writer.finish().expect("PNG finish");
    bytes
}

fn fixture_driver() -> IosDriver {
    IosDriver::new(
        IosDeviceConfig::new(
            format!("conformance-{}", Uuid::new_v4()),
            "iOS conformance device",
            None,
        )
        .expect("device config"),
        Arc::new(FakeWda::new()),
    )
}

fn conformance_call(action: &ActionDefinition) -> Result<ActionCall, String> {
    let arguments = match action.name.as_str() {
        "tap" => json!({ "x": 0, "y": 0 }),
        "inputText" => json!({ "text": "DeviceRail" }),
        "keyPress" => json!({ "key": "enter" }),
        "swipe" => json!({
            "startX": 0, "startY": 0, "endX": 0, "endY": 0, "durationMs": 100
        }),
        "scroll" => json!({ "deltaX": 0, "deltaY": 1 }),
        name => return Err(format!("no iOS conformance fixture for `{name}`")),
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
        let root = tempfile::tempdir().expect("temporary Evidence root");
        let inner = FileEvidenceStore::new(root.path(), FileEvidenceStoreConfig::default())
            .expect("Evidence Store");
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

async fn operation_runtime(
    driver: Arc<IosDriver>,
) -> (DeviceRuntime<IosDriver, MemoryEventStore>, OperationContext) {
    let events = Arc::new(MemoryEventStore::default());
    let session = events
        .start_session(StartSession::new(None, Some(driver.id().clone()), now_ms()))
        .await
        .expect("start Session");
    let context = OperationContext::new(session.id, None);
    let runtime = DeviceRuntime::with_evidence(driver, events, TemporaryEvidenceStore::create());
    (runtime, context)
}

devicerail_core::driver_conformance_test!(
    conforms_to_shared_driver_contract,
    fixture_driver,
    conformance_call,
    TemporaryEvidenceStore::create(),
);

#[tokio::test]
async fn connect_and_health_rebuild_only_definitively_stale_sessions() {
    let transport = Arc::new(FakeWda::new());
    let driver = IosDriver::new(
        IosDeviceConfig::new(
            format!("restart-lifecycle-{}", Uuid::new_v4()),
            "iOS restart lifecycle device",
            None,
        )
        .expect("device config"),
        Arc::clone(&transport) as Arc<dyn WdaTransport>,
    );
    driver
        .connect(&ExecutionControl::unbounded())
        .await
        .expect("initial connect");
    assert_eq!(transport.create_count(), 1);

    transport.restart();
    let reconnected = driver
        .connect(&ExecutionControl::unbounded())
        .await
        .expect("connect recovers stale Session");
    assert!(reconnected.connected);
    assert_eq!(transport.create_count(), 2);
    assert_eq!(transport.probe_count(), 1);

    transport.restart();
    driver
        .health_check(&ExecutionControl::unbounded())
        .await
        .expect("health recovers stale Session");
    assert!(driver.device_info().await.connected);
    assert_eq!(transport.create_count(), 3);
    assert_eq!(transport.probe_count(), 2);
}

#[tokio::test]
async fn observe_recovers_after_wda_restart_without_caller_reconnect() {
    let transport = Arc::new(FakeWda::new());
    let driver = Arc::new(IosDriver::new(
        IosDeviceConfig::new(
            format!("restart-observe-{}", Uuid::new_v4()),
            "iOS restart observation device",
            None,
        )
        .expect("device config"),
        Arc::clone(&transport) as Arc<dyn WdaTransport>,
    ));
    driver
        .connect(&ExecutionControl::unbounded())
        .await
        .expect("initial connect");
    let (runtime, context) = operation_runtime(Arc::clone(&driver)).await;

    transport.restart();
    let observation = runtime
        .observe(&context)
        .await
        .expect("observe recovers stale Session");

    assert_eq!(observation.device_id, *driver.id());
    assert!(observation.screenshot.is_some());
    assert_eq!(transport.create_count(), 2);
    assert_eq!(transport.inspection_count(), 1);
    assert!(driver.device_info().await.connected);
}

#[tokio::test]
async fn uncertain_mutation_is_returned_once_without_blind_replay() {
    let transport = Arc::new(FakeWda::new());
    let driver = Arc::new(IosDriver::new(
        IosDeviceConfig::new(
            format!("mutation-boundary-{}", Uuid::new_v4()),
            "iOS mutation boundary device",
            None,
        )
        .expect("device config"),
        Arc::clone(&transport) as Arc<dyn WdaTransport>,
    ));
    driver
        .connect(&ExecutionControl::unbounded())
        .await
        .expect("connect Driver");
    let (runtime, context) = operation_runtime(Arc::clone(&driver)).await;
    transport.fail_next_perform("wda_command_outcome_unknown");

    let error = runtime
        .execute(
            &context,
            ActionCall {
                id: Uuid::new_v4(),
                name: "tap".to_owned(),
                arguments: json!({ "x": 0, "y": 0 }),
            },
        )
        .await
        .expect_err("ambiguous mutation must fail closed");

    assert!(matches!(
        error,
        RuntimeError::Driver(DriverError::Platform { code, retryable: false })
            if code == "wda_command_outcome_unknown"
    ));
    assert_eq!(transport.perform_count(), 1);
    assert!(transport.actions().is_empty());
    assert_eq!(transport.create_count(), 1);
    assert!(driver.device_info().await.connected);
}

#[tokio::test]
async fn disconnect_clears_explicit_invalid_session_but_retains_ambiguous_ownership() {
    let transport = Arc::new(FakeWda::new());
    let driver = IosDriver::new(
        IosDeviceConfig::new(
            format!("disconnect-boundary-{}", Uuid::new_v4()),
            "iOS disconnect boundary device",
            None,
        )
        .expect("device config"),
        Arc::clone(&transport) as Arc<dyn WdaTransport>,
    );
    driver
        .connect(&ExecutionControl::unbounded())
        .await
        .expect("connect Driver");

    transport.fail_next_delete("wda_command_outcome_unknown");
    let error = driver
        .disconnect(&ExecutionControl::unbounded())
        .await
        .expect_err("ambiguous delete must retain ownership");
    assert!(matches!(
        error,
        DriverError::Platform { code, retryable: false }
            if code == "wda_command_outcome_unknown"
    ));
    assert!(driver.device_info().await.connected);

    transport.return_invalid_session_on_delete();
    driver
        .disconnect(&ExecutionControl::unbounded())
        .await
        .expect("explicit invalid Session means already disconnected");
    assert!(!driver.device_info().await.connected);
    assert_eq!(transport.delete_count(), 2);
    driver
        .disconnect(&ExecutionControl::unbounded())
        .await
        .expect("second disconnect is idempotent");
    assert_eq!(transport.delete_count(), 2);
}

#[tokio::test]
async fn ambiguous_create_poisoned_ownership_is_not_retried() {
    let transport = Arc::new(FakeWda::new());
    transport.fail_next_create("wda_command_outcome_unknown");
    let driver = IosDriver::new(
        IosDeviceConfig::new(
            format!("create-boundary-{}", Uuid::new_v4()),
            "iOS create boundary device",
            None,
        )
        .expect("device config"),
        Arc::clone(&transport) as Arc<dyn WdaTransport>,
    );

    driver
        .connect(&ExecutionControl::unbounded())
        .await
        .expect_err("ambiguous create must fail");
    let error = driver
        .connect(&ExecutionControl::unbounded())
        .await
        .expect_err("ambiguous ownership must fail closed");

    assert!(matches!(
        error,
        DriverError::Platform { code, retryable: false }
            if code == "wda_session_ownership_unknown"
    ));
    assert_eq!(transport.create_count(), 1);
}

#[tokio::test]
async fn out_of_bounds_coordinates_never_reach_wda_transport() {
    let transport = Arc::new(FakeWda::new());
    let driver = Arc::new(IosDriver::new(
        IosDeviceConfig::new(
            format!("viewport-{}", Uuid::new_v4()),
            "iOS viewport validation device",
            None,
        )
        .expect("device config"),
        Arc::clone(&transport) as Arc<dyn WdaTransport>,
    ));
    driver
        .connect(&ExecutionControl::unbounded())
        .await
        .expect("connect Driver");

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

    for call in [
        ActionCall {
            id: Uuid::new_v4(),
            name: "tap".to_owned(),
            arguments: json!({ "x": 1.0, "y": 0e0 }),
        },
        ActionCall {
            id: Uuid::new_v4(),
            name: "swipe".to_owned(),
            arguments: serde_json::from_str(
                r#"{"startX":0.0,"startY":0e0,"endX":1.0,"endY":0,"durationMs":100.0}"#,
            )
            .expect("swipe JSON"),
        },
    ] {
        runtime
            .execute(&context, call)
            .await
            .expect_err("out-of-bounds action must fail");
    }

    assert!(
        transport.actions().is_empty(),
        "viewport-invalid actions reached WDA transport"
    );
    assert_eq!(
        transport.inspection_count(),
        2,
        "each failed Action must validate against a freshly captured viewport"
    );
}
