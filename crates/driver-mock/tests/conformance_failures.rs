use std::{
    io::Cursor,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::Duration,
};

use async_trait::async_trait;
use devicerail_core::{
    DeviceDriver, DeviceOperationResult, DriverError, DriverOperationContext, DriverResult,
    EvidenceInput, EvidenceMetadata, EvidenceOutput, EvidenceResult, EvidenceStore,
    ExecutionControl, GcPolicy, GcReport, PutEvidence, ReleaseReport, ScreenshotPolicy,
    Sha256Digest, StoredEvidence, UnavailableEvidenceStore,
    conformance::{
        ConformanceOptions, run_driver_conformance_with_evidence,
        run_driver_conformance_with_evidence_and_options, run_driver_conformance_with_options,
    },
};
use devicerail_driver_mock::MockDriver;
use devicerail_evidence_fs::{FileEvidenceStore, FileEvidenceStoreConfig};
use devicerail_protocol::{
    ActionCall, ActionDefinition, ActionProtection, ActionResult, AssetRef, DeviceId, DeviceInfo,
    Observation, SessionId,
};
use serde_json::json;
use tempfile::TempDir;
use uuid::Uuid;

#[derive(Clone, Copy)]
enum Fault {
    CompatibleActionSchemas,
    IgnoreUnexpectedArgument,
    MisclassifyProtection,
    IgnoreScreenshotOmit,
    HangWhileConnected,
    PanicWhileConnected,
}

#[derive(Default)]
struct SharedState {
    connected: AtomicBool,
    disconnect_count: AtomicUsize,
}

struct FaultyDriver {
    inner: MockDriver,
    fault: Fault,
    state: Arc<SharedState>,
}

struct RecordingEvidenceStore {
    inner: FileEvidenceStore,
    release_calls: Mutex<Vec<SessionId>>,
    successful_puts: AtomicUsize,
    fail_release: bool,
}

impl RecordingEvidenceStore {
    fn new(root: &TempDir, fail_release: bool) -> Self {
        Self {
            inner: FileEvidenceStore::new(root.path(), FileEvidenceStoreConfig::default())
                .expect("recording Evidence Store"),
            release_calls: Mutex::new(Vec::new()),
            successful_puts: AtomicUsize::new(0),
            fail_release,
        }
    }

    fn release_calls(&self) -> Vec<SessionId> {
        self.release_calls
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }
}

#[async_trait]
impl EvidenceStore for RecordingEvidenceStore {
    async fn put(
        &self,
        request: PutEvidence,
        input: EvidenceInput,
    ) -> EvidenceResult<StoredEvidence> {
        let stored = self.inner.put(request, input).await?;
        self.successful_puts.fetch_add(1, Ordering::SeqCst);
        Ok(stored)
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
        self.release_calls
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(session_id.clone());
        if self.fail_release {
            return Err(devicerail_core::EvidenceError::Internal(
                "fixture failed below /private/secret/evidence".to_owned(),
            ));
        }
        self.inner.release_session(session_id, released_at_ms).await
    }

    async fn gc(&self, policy: GcPolicy) -> EvidenceResult<GcReport> {
        self.inner.gc(policy).await
    }
}

#[derive(Clone, Copy)]
enum EvidenceFault {
    None,
    FailAfterReceipt,
    PanicAfterReceipt,
    HangAfterReceipt,
    HangDuringActionAfterReceipt,
}

struct EvidenceDriver {
    inner: MockDriver,
    fault: EvidenceFault,
    fail_cleanup_disconnect: bool,
    disconnect_count: AtomicUsize,
}

impl EvidenceDriver {
    fn new(id: impl Into<String>, fault: EvidenceFault, fail_cleanup_disconnect: bool) -> Self {
        Self {
            inner: MockDriver::new(id),
            fault,
            fail_cleanup_disconnect,
            disconnect_count: AtomicUsize::new(0),
        }
    }

    async fn persist_fixture(context: &DriverOperationContext) -> EvidenceResult<AssetRef> {
        context
            .evidence()
            .put(
                "image/png",
                Box::pin(Cursor::new(b"conformance evidence fixture".to_vec())),
            )
            .await
            .map(|stored| stored.asset_ref())
    }
}

#[async_trait]
impl DeviceDriver for EvidenceDriver {
    fn id(&self) -> &DeviceId {
        self.inner.id()
    }

    async fn connect(&self, control: &ExecutionControl) -> DriverResult<DeviceInfo> {
        self.inner.connect(control).await
    }

    async fn disconnect(&self, control: &ExecutionControl) -> DriverResult<()> {
        let previous = self.disconnect_count.fetch_add(1, Ordering::SeqCst);
        if self.fail_cleanup_disconnect && previous >= 3 {
            return Err(DriverError::Internal(
                "intentional cleanup disconnect failure".to_owned(),
            ));
        }
        self.inner.disconnect(control).await
    }

    async fn capabilities(
        &self,
        control: &ExecutionControl,
    ) -> DriverResult<Vec<ActionDefinition>> {
        self.inner.capabilities(control).await
    }

    fn action_protection(&self, name: &str) -> Option<ActionProtection> {
        self.inner.action_protection(name)
    }

    async fn observe(
        &self,
        context: &DriverOperationContext,
    ) -> DeviceOperationResult<Observation> {
        let mut observation = self.inner.observe(context).await?;
        if context.screenshot_policy() == ScreenshotPolicy::Capture {
            observation.screenshot = Some(Self::persist_fixture(context).await?);
        }
        match self.fault {
            EvidenceFault::None | EvidenceFault::HangDuringActionAfterReceipt => Ok(observation),
            EvidenceFault::FailAfterReceipt => {
                Err(DriverError::Protocol("intentional failure after receipt".to_owned()).into())
            }
            EvidenceFault::PanicAfterReceipt => {
                panic!("intentional panic after receipt")
            }
            EvidenceFault::HangAfterReceipt => std::future::pending().await,
        }
    }

    async fn execute(
        &self,
        context: &DriverOperationContext,
        call: ActionCall,
    ) -> DeviceOperationResult<ActionResult> {
        let mut result = self.inner.execute(context, call).await?;
        if context.screenshot_policy() == ScreenshotPolicy::Capture {
            let asset = Self::persist_fixture(context).await?;
            result
                .after
                .as_mut()
                .expect("MockDriver always returns an after observation")
                .screenshot = Some(asset.clone());
            result.evidence = vec![asset];
        }
        if matches!(self.fault, EvidenceFault::HangDuringActionAfterReceipt) {
            std::future::pending().await
        } else {
            Ok(result)
        }
    }
}

#[async_trait]
impl DeviceDriver for FaultyDriver {
    fn id(&self) -> &DeviceId {
        self.inner.id()
    }

    async fn connect(&self, control: &ExecutionControl) -> DriverResult<DeviceInfo> {
        let info = self.inner.connect(control).await?;
        self.state.connected.store(true, Ordering::SeqCst);
        Ok(info)
    }

    async fn disconnect(&self, control: &ExecutionControl) -> DriverResult<()> {
        self.state.connected.store(false, Ordering::SeqCst);
        self.state.disconnect_count.fetch_add(1, Ordering::SeqCst);
        self.inner.disconnect(control).await
    }

    async fn capabilities(
        &self,
        control: &ExecutionControl,
    ) -> DriverResult<Vec<ActionDefinition>> {
        let mut capabilities = self.inner.capabilities(control).await?;
        if matches!(self.fault, Fault::CompatibleActionSchemas) {
            for action in &mut capabilities {
                action.input_schema = match action.name.as_str() {
                    "tap" => json!({
                        "$schema": "http://json-schema.org/draft-07/schema#",
                        "$id": "https://schemas.devicerail.test/tap.json",
                        "type": "object",
                        "additionalProperties": false,
                        "required": ["x", "y"],
                        "definitions": {
                            "coordinate": {
                                "$id": "#coordinate",
                                "type": "number",
                                "minimum": 0
                            }
                        },
                        "properties": {
                            "x": { "$ref": "#coordinate" },
                            "y": { "$ref": "#coordinate" }
                        }
                    }),
                    "inputText" => json!({
                        "$schema": "http://json-schema.org/draft-06/schema#",
                        "type": "object",
                        "additionalProperties": false,
                        "required": ["text"],
                        "properties": {
                            "text": { "type": "string" }
                        }
                    }),
                    "scroll" => json!({
                        "$schema": "https://json-schema.org/draft/2020-12/schema",
                        "$id": "https://schemas.devicerail.test/scroll.json",
                        "type": "object",
                        "additionalProperties": false,
                        "required": ["deltaX", "deltaY"],
                        "$defs": {
                            "delta": {
                                "$id": "delta.json",
                                "$anchor": "value",
                                "type": "number"
                            }
                        },
                        "properties": {
                            "deltaX": { "$ref": "delta.json#value" },
                            "deltaY": { "$ref": "delta.json#value" }
                        }
                    }),
                    _ => action.input_schema.clone(),
                };
            }
        }
        if matches!(self.fault, Fault::MisclassifyProtection) {
            capabilities[0].protection = ActionProtection::Protected;
        }
        Ok(capabilities)
    }

    fn action_protection(&self, name: &str) -> Option<ActionProtection> {
        self.inner.action_protection(name)
    }

    async fn observe(
        &self,
        context: &DriverOperationContext,
    ) -> DeviceOperationResult<Observation> {
        if matches!(self.fault, Fault::HangWhileConnected)
            && self.state.connected.load(Ordering::SeqCst)
        {
            return std::future::pending().await;
        }
        assert!(
            !(matches!(self.fault, Fault::PanicWhileConnected)
                && self.state.connected.load(Ordering::SeqCst)),
            "intentional broken Driver panic"
        );
        let mut observation = self.inner.observe(context).await?;
        if matches!(self.fault, Fault::IgnoreScreenshotOmit)
            && context.screenshot_policy() == ScreenshotPolicy::Omit
        {
            observation.screenshot_omission = None;
        }
        Ok(observation)
    }

    async fn execute(
        &self,
        context: &DriverOperationContext,
        mut call: ActionCall,
    ) -> DeviceOperationResult<ActionResult> {
        if matches!(self.fault, Fault::IgnoreUnexpectedArgument) {
            if let Some(arguments) = call.arguments.as_object_mut() {
                arguments.remove("__devicerailUnexpected");
            }
        }
        self.inner.execute(context, call).await
    }
}

fn valid_call(action: &ActionDefinition) -> Result<ActionCall, String> {
    let arguments = match action.name.as_str() {
        "tap" => json!({ "x": 1, "y": 1 }),
        "inputText" => json!({ "text": "DeviceRail" }),
        "scroll" => json!({ "deltaX": 0, "deltaY": 1 }),
        name => return Err(format!("no fixture for `{name}`")),
    };
    Ok(ActionCall {
        id: Uuid::new_v4(),
        name: action.name.clone(),
        arguments,
    })
}

#[tokio::test]
async fn suite_accepts_declared_dialects_and_compound_resources() {
    let state = Arc::new(SharedState::default());
    let factory_state = Arc::clone(&state);
    let report = run_driver_conformance_with_options(
        move || FaultyDriver {
            inner: MockDriver::new("compatible-schema-driver"),
            fault: Fault::CompatibleActionSchemas,
            state: factory_state,
        },
        valid_call,
        ConformanceOptions::default(),
    )
    .await
    .expect("supported self-contained JSON Schema forms must remain compatible");

    assert!(report.full_json_schema_validation);
    assert_eq!(report.capability_count, 3);
    assert!(!state.connected.load(Ordering::SeqCst));
}

#[tokio::test]
async fn evidence_enabled_suite_enforces_operation_scoped_receipts() {
    let failure = run_driver_conformance_with_evidence(
        || MockDriver::new("external-evidence-driver"),
        valid_call,
        Arc::new(UnavailableEvidenceStore),
    )
    .await
    .expect_err("the evidence-enabled suite must reject Driver-owned references");

    assert_eq!(failure.check, "observe_after_connect");
    assert!(failure.message.contains("protocol error"));
    assert!(!failure.message.contains("mock://"));
}

async fn assert_harness_evidence_released(store: &RecordingEvidenceStore) {
    assert!(store.successful_puts.load(Ordering::SeqCst) > 0);
    assert_eq!(store.release_calls().len(), 1);
    assert!(
        store
            .referenced_sessions()
            .await
            .expect("list referenced Sessions")
            .is_empty(),
        "the conformance harness must not retain its Session in the caller's Store"
    );
}

#[tokio::test]
async fn evidence_enabled_suite_releases_its_session_after_success() {
    let root = tempfile::tempdir().expect("temporary Evidence Store");
    let store = Arc::new(RecordingEvidenceStore::new(&root, false));
    let evidence: Arc<dyn EvidenceStore> = store.clone();

    let report = run_driver_conformance_with_evidence(
        || EvidenceDriver::new("evidence-success", EvidenceFault::None, false),
        valid_call,
        evidence,
    )
    .await
    .expect("evidence-producing Driver must conform");

    assert_eq!(report.capability_count, 3);
    assert_harness_evidence_released(&store).await;
}

#[tokio::test]
async fn evidence_enabled_suite_releases_receipts_after_an_ordinary_failure() {
    let root = tempfile::tempdir().expect("temporary Evidence Store");
    let store = Arc::new(RecordingEvidenceStore::new(&root, false));
    let evidence: Arc<dyn EvidenceStore> = store.clone();

    let failure = run_driver_conformance_with_evidence(
        || EvidenceDriver::new("evidence-failure", EvidenceFault::FailAfterReceipt, false),
        valid_call,
        evidence,
    )
    .await
    .expect_err("intentional Driver failure");

    assert_eq!(failure.check, "observe_after_connect");
    assert_harness_evidence_released(&store).await;
}

#[tokio::test]
async fn evidence_enabled_suite_releases_receipts_after_a_driver_panic() {
    let root = tempfile::tempdir().expect("temporary Evidence Store");
    let store = Arc::new(RecordingEvidenceStore::new(&root, false));
    let evidence: Arc<dyn EvidenceStore> = store.clone();

    let failure = run_driver_conformance_with_evidence(
        || EvidenceDriver::new("evidence-panic", EvidenceFault::PanicAfterReceipt, false),
        valid_call,
        evidence,
    )
    .await
    .expect_err("intentional Driver panic");

    assert_eq!(failure.check, "suite_panic");
    assert_harness_evidence_released(&store).await;
}

#[tokio::test]
async fn evidence_enabled_suite_releases_receipts_after_timeout_abort() {
    let root = tempfile::tempdir().expect("temporary Evidence Store");
    let store = Arc::new(RecordingEvidenceStore::new(&root, false));
    let evidence: Arc<dyn EvidenceStore> = store.clone();

    let failure = run_driver_conformance_with_evidence_and_options(
        || EvidenceDriver::new("evidence-timeout", EvidenceFault::HangAfterReceipt, false),
        valid_call,
        evidence,
        ConformanceOptions {
            suite_timeout: Duration::from_millis(500),
            cleanup_timeout: Duration::from_secs(2),
        },
    )
    .await
    .expect_err("hung Driver must time out");

    assert_eq!(failure.check, "suite_timeout");
    assert_harness_evidence_released(&store).await;
}

#[tokio::test]
async fn evidence_enabled_suite_releases_action_receipts_after_timeout_abort() {
    let root = tempfile::tempdir().expect("temporary Evidence Store");
    let store = Arc::new(RecordingEvidenceStore::new(&root, false));
    let evidence: Arc<dyn EvidenceStore> = store.clone();

    let failure = run_driver_conformance_with_evidence_and_options(
        || {
            EvidenceDriver::new(
                "evidence-action-timeout",
                EvidenceFault::HangDuringActionAfterReceipt,
                false,
            )
        },
        valid_call,
        evidence,
        ConformanceOptions {
            suite_timeout: Duration::from_millis(500),
            cleanup_timeout: Duration::from_secs(2),
        },
    )
    .await
    .expect_err("hung Action must time out");

    assert_eq!(failure.check, "suite_timeout");
    assert_harness_evidence_released(&store).await;
}

#[tokio::test]
async fn cleanup_failures_fail_a_successful_suite_and_are_combined() {
    let root = tempfile::tempdir().expect("temporary Evidence Store");
    let store = Arc::new(RecordingEvidenceStore::new(&root, true));
    let evidence: Arc<dyn EvidenceStore> = store.clone();

    let failure = run_driver_conformance_with_evidence(
        || EvidenceDriver::new("cleanup-failures", EvidenceFault::None, true),
        valid_call,
        evidence,
    )
    .await
    .expect_err("cleanup failures must fail an otherwise successful suite");

    assert_eq!(failure.check, "cleanup_disconnect");
    assert!(failure.message.contains("cleanup also failed"));
    assert!(failure.message.contains("evidence store failed"));
    assert!(!failure.message.contains("/private/secret/evidence"));
    assert_eq!(store.release_calls().len(), 1);
}

#[tokio::test]
async fn store_cleanup_failure_is_attached_to_the_original_suite_failure() {
    let root = tempfile::tempdir().expect("temporary Evidence Store");
    let store = Arc::new(RecordingEvidenceStore::new(&root, true));
    let evidence: Arc<dyn EvidenceStore> = store.clone();

    let failure = run_driver_conformance_with_evidence(
        || {
            EvidenceDriver::new(
                "suite-and-cleanup-failure",
                EvidenceFault::FailAfterReceipt,
                false,
            )
        },
        valid_call,
        evidence,
    )
    .await
    .expect_err("suite and cleanup both fail");

    assert_eq!(failure.check, "observe_after_connect");
    assert!(failure.message.contains("cleanup also failed"));
    assert!(!failure.message.contains("/private/secret/evidence"));
    assert_eq!(store.release_calls().len(), 1);
}

#[tokio::test]
async fn suite_detects_schema_enforcement_gaps_and_still_disconnects() {
    let state = Arc::new(SharedState::default());
    let factory_state = Arc::clone(&state);
    let failure = run_driver_conformance_with_options(
        move || FaultyDriver {
            inner: MockDriver::new("broken-extra-argument"),
            fault: Fault::IgnoreUnexpectedArgument,
            state: factory_state,
        },
        valid_call,
        ConformanceOptions::default(),
    )
    .await
    .expect_err("a Driver that ignores additionalProperties must fail");

    assert_eq!(failure.check, "invalid_arguments");
    assert!(!state.connected.load(Ordering::SeqCst));
    assert!(state.disconnect_count.load(Ordering::SeqCst) >= 2);
}

#[tokio::test]
async fn suite_fails_closed_on_capability_protection_mismatches() {
    let state = Arc::new(SharedState::default());
    let factory_state = Arc::clone(&state);
    let failure = run_driver_conformance_with_options(
        move || FaultyDriver {
            inner: MockDriver::new("broken-protection-classifier"),
            fault: Fault::MisclassifyProtection,
            state: factory_state,
        },
        valid_call,
        ConformanceOptions::default(),
    )
    .await
    .expect_err("capability and classifier mismatch must fail closed");

    assert_eq!(failure.check, "disconnected_capabilities");
    assert!(!state.connected.load(Ordering::SeqCst));
}

#[tokio::test]
async fn suite_rejects_drivers_that_silently_ignore_global_screenshot_omission() {
    let state = Arc::new(SharedState::default());
    let factory_state = Arc::clone(&state);
    let failure = run_driver_conformance_with_options(
        move || FaultyDriver {
            inner: MockDriver::new("broken-screenshot-omit"),
            fault: Fault::IgnoreScreenshotOmit,
            state: factory_state,
        },
        valid_call,
        ConformanceOptions::default(),
    )
    .await
    .expect_err("screenshot omission must be explicit");

    assert_eq!(failure.check, "observe_omit_policy");
    assert!(!state.connected.load(Ordering::SeqCst));
}

#[tokio::test]
async fn suite_times_out_a_hung_driver_and_still_disconnects() {
    let state = Arc::new(SharedState::default());
    let factory_state = Arc::clone(&state);
    let failure = run_driver_conformance_with_options(
        move || FaultyDriver {
            inner: MockDriver::new("broken-hanging-observe"),
            fault: Fault::HangWhileConnected,
            state: factory_state,
        },
        valid_call,
        ConformanceOptions {
            suite_timeout: Duration::from_millis(25),
            cleanup_timeout: Duration::from_secs(1),
        },
    )
    .await
    .expect_err("a hung Driver must time out");

    assert_eq!(failure.check, "suite_timeout");
    assert!(!state.connected.load(Ordering::SeqCst));
    assert!(state.disconnect_count.load(Ordering::SeqCst) >= 2);
}

#[tokio::test]
async fn suite_converts_driver_panics_to_failures_and_still_disconnects() {
    let state = Arc::new(SharedState::default());
    let factory_state = Arc::clone(&state);
    let failure = run_driver_conformance_with_options(
        move || FaultyDriver {
            inner: MockDriver::new("broken-panicking-observe"),
            fault: Fault::PanicWhileConnected,
            state: factory_state,
        },
        valid_call,
        ConformanceOptions::default(),
    )
    .await
    .expect_err("a panicking Driver must fail the suite");

    assert_eq!(failure.check, "suite_panic");
    assert!(!state.connected.load(Ordering::SeqCst));
    assert!(state.disconnect_count.load(Ordering::SeqCst) >= 2);
}
