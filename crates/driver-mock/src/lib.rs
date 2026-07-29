use std::{
    fmt,
    io::Cursor,
    sync::atomic::{AtomicBool, AtomicU64, Ordering},
    time::Duration,
};

use async_trait::async_trait;
use devicerail_core::{
    DeviceDriver, DeviceOperationResult, DriverError, DriverOperationContext, DriverResult,
    ExecutionControl, ScreenshotPolicy, now_ms,
};
use devicerail_protocol::{
    ActionCall, ActionDefinition, ActionProtection, ActionResult, AssetRef, DeviceId, DeviceInfo,
    Observation, Platform, ScreenshotOmissionReason, Viewport,
};
use serde_json::{Map, Value, json};
use uuid::Uuid;

// A deterministic, valid 1x1 RGBA PNG. Store-owned Mock observations use the
// same content for every frame so frame identity stays in metadata instead of
// being encoded as trailing, non-PNG bytes.
const MOCK_SCREENSHOT_PNG: [u8; 70] = [
    0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x48, 0x44, 0x52,
    0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00, 0x00, 0x1f, 0x15, 0xc4,
    0x89, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x44, 0x41, 0x54, 0x78, 0xda, 0x63, 0x08, 0x9a, 0xf3, 0xea,
    0x3f, 0x00, 0x05, 0xf4, 0x02, 0xd8, 0x8d, 0xe4, 0x08, 0x21, 0x00, 0x00, 0x00, 0x00, 0x49, 0x45,
    0x4e, 0x44, 0xae, 0x42, 0x60, 0x82,
];
const MOCK_SCREENSHOT_PNG_SIZE: u64 = 70;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum EvidenceMode {
    #[default]
    DriverOwned,
    SessionEvidence,
}

pub struct MockDriver {
    id: DeviceId,
    connected: AtomicBool,
    frame_sequence: AtomicU64,
    action_delay: Duration,
    evidence_mode: EvidenceMode,
}

impl MockDriver {
    pub fn new(id: impl Into<String>) -> Self {
        Self {
            id: DeviceId::new(id),
            connected: AtomicBool::new(false),
            frame_sequence: AtomicU64::new(0),
            action_delay: Duration::ZERO,
            evidence_mode: EvidenceMode::DriverOwned,
        }
    }

    /// Adds deterministic latency before every Action for cancellation and
    /// timeout integration tests. The default Mock remains immediate.
    pub fn with_action_delay(mut self, delay: Duration) -> Self {
        self.action_delay = delay;
        self
    }

    /// Persists each Mock screenshot through the operation's Session-bound
    /// Evidence writer and returns only the resulting canonical `AssetRef`.
    ///
    /// This mode is intended for runtimes created with
    /// `DeviceRuntime::with_evidence`. The default mode deliberately retains
    /// the historical `mock://` references for lightweight tests.
    pub fn with_session_evidence(mut self) -> Self {
        self.evidence_mode = EvidenceMode::SessionEvidence;
        self
    }

    /// Returns the current public metadata snapshot used for Registry
    /// registration and device listing.
    pub fn device_info(&self) -> DeviceInfo {
        self.info()
    }

    fn ensure_connected(&self) -> DriverResult<()> {
        if self.connected.load(Ordering::SeqCst) {
            Ok(())
        } else {
            Err(DriverError::NotConnected(self.id.clone()))
        }
    }

    fn ensure_active(control: &ExecutionControl) -> DriverResult<()> {
        if control.is_cancelled() {
            Err(DriverError::Cancelled)
        } else if control.is_expired() {
            Err(DriverError::TimedOut)
        } else {
            Ok(())
        }
    }

    fn info(&self) -> DeviceInfo {
        DeviceInfo {
            id: self.id.clone(),
            name: "DeviceRail Mock Device".to_owned(),
            platform: Platform::Mock,
            os_version: Some("0.1".to_owned()),
            connected: self.connected.load(Ordering::SeqCst),
        }
    }

    fn validate_number(arguments: &Value, action: &str, field: &str) -> DriverResult<f64> {
        arguments
            .get(field)
            .and_then(Value::as_f64)
            .ok_or_else(|| DriverError::InvalidArguments {
                action: action.to_owned(),
                message: format!("{field} must be a number"),
            })
    }

    fn validate_fields(arguments: &Value, action: &str, allowed: &[&str]) -> DriverResult<()> {
        let fields = arguments
            .as_object()
            .ok_or_else(|| DriverError::InvalidArguments {
                action: action.to_owned(),
                message: "arguments must be an object".to_owned(),
            })?;
        if let Some(unexpected) = fields
            .keys()
            .find(|field| !allowed.contains(&field.as_str()))
        {
            return Err(DriverError::InvalidArguments {
                action: action.to_owned(),
                message: format!("unexpected argument: {unexpected}"),
            });
        }
        Ok(())
    }

    fn validate_non_negative(value: f64, action: &str, field: &str) -> DriverResult<f64> {
        if value >= 0.0 {
            Ok(value)
        } else {
            Err(DriverError::InvalidArguments {
                action: action.to_owned(),
                message: format!("{field} must be non-negative"),
            })
        }
    }

    async fn screenshot(
        &self,
        context: &DriverOperationContext,
        sequence: u64,
    ) -> DeviceOperationResult<AssetRef> {
        match self.evidence_mode {
            EvidenceMode::DriverOwned => Ok(AssetRef {
                id: format!("mock-frame-{sequence}"),
                media_type: "image/png".to_owned(),
                uri: format!("mock://screenshots/{sequence}.png"),
                sha256: None,
            }),
            EvidenceMode::SessionEvidence => {
                let stored = context
                    .evidence()
                    .put_with_declared_size(
                        "image/png",
                        MOCK_SCREENSHOT_PNG_SIZE,
                        Box::pin(Cursor::new(MOCK_SCREENSHOT_PNG.to_vec())),
                    )
                    .await?;
                Ok(stored.asset_ref())
            }
        }
    }
}

impl fmt::Debug for MockDriver {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("MockDriver")
            .field("id", &self.id)
            .field("action_delay", &self.action_delay)
            .field("evidence_mode", &self.evidence_mode)
            .finish_non_exhaustive()
    }
}

#[async_trait]
impl DeviceDriver for MockDriver {
    fn id(&self) -> &DeviceId {
        &self.id
    }

    async fn connect(&self, control: &ExecutionControl) -> DriverResult<DeviceInfo> {
        Self::ensure_active(control)?;
        self.connected.store(true, Ordering::SeqCst);
        Ok(self.info())
    }

    async fn disconnect(&self, control: &ExecutionControl) -> DriverResult<()> {
        Self::ensure_active(control)?;
        self.connected.store(false, Ordering::SeqCst);
        Ok(())
    }

    async fn capabilities(
        &self,
        control: &ExecutionControl,
    ) -> DriverResult<Vec<ActionDefinition>> {
        Self::ensure_active(control)?;
        Ok(vec![
            ActionDefinition {
                name: "tap".to_owned(),
                description: "Tap a point in device coordinates".to_owned(),
                input_schema: json!({
                    "type": "object",
                    "additionalProperties": false,
                    "required": ["x", "y"],
                    "properties": {
                        "x": { "type": "number", "minimum": 0 },
                        "y": { "type": "number", "minimum": 0 }
                    }
                }),
                protection: ActionProtection::Standard,
            },
            ActionDefinition {
                name: "inputText".to_owned(),
                description: "Type text into the focused control".to_owned(),
                input_schema: json!({
                    "type": "object",
                    "additionalProperties": false,
                    "required": ["text"],
                    "properties": {
                        "text": { "type": "string" }
                    }
                }),
                protection: ActionProtection::Standard,
            },
            ActionDefinition {
                name: "scroll".to_owned(),
                description: "Scroll by a device-coordinate delta".to_owned(),
                input_schema: json!({
                    "type": "object",
                    "additionalProperties": false,
                    "required": ["deltaX", "deltaY"],
                    "properties": {
                        "deltaX": { "type": "number" },
                        "deltaY": { "type": "number" }
                    }
                }),
                protection: ActionProtection::Standard,
            },
        ])
    }

    fn action_protection(&self, name: &str) -> Option<ActionProtection> {
        matches!(name, "tap" | "inputText" | "scroll").then_some(ActionProtection::Standard)
    }

    async fn observe(
        &self,
        context: &DriverOperationContext,
    ) -> DeviceOperationResult<Observation> {
        let control = context.control();
        Self::ensure_active(control)?;
        self.ensure_connected()?;
        let sequence = self.frame_sequence.fetch_add(1, Ordering::SeqCst) + 1;
        let (screenshot, screenshot_omission) = match context.screenshot_policy() {
            ScreenshotPolicy::Capture => (Some(self.screenshot(context, sequence).await?), None),
            ScreenshotPolicy::Omit => (None, Some(ScreenshotOmissionReason::Policy)),
        };

        Ok(Observation {
            id: Uuid::new_v4(),
            device_id: self.id.clone(),
            captured_at_ms: now_ms(),
            viewport: Viewport {
                width: 1280,
                height: 720,
                scale_factor: 1.0,
            },
            screenshot,
            screenshot_omission,
            ui_snapshot: None,
            ui_snapshot_omission: None,
            metadata: Map::from_iter([("frameSequence".to_owned(), json!(sequence))]),
        })
    }

    async fn execute(
        &self,
        context: &DriverOperationContext,
        call: ActionCall,
    ) -> DeviceOperationResult<ActionResult> {
        let control = context.control();
        Self::ensure_active(control)?;
        self.ensure_connected()?;
        if !self.action_delay.is_zero() {
            let wait = control.remaining().map_or(self.action_delay, |remaining| {
                remaining.min(self.action_delay)
            });
            tokio::select! {
                biased;
                _ = control.cancelled() => return Err(DriverError::Cancelled.into()),
                () = tokio::time::sleep(wait) => Self::ensure_active(control)?,
            }
        }
        let started_at_ms = now_ms();

        let output = match call.name.as_str() {
            "tap" => {
                Self::validate_fields(&call.arguments, "tap", &["x", "y"])?;
                let x = Self::validate_non_negative(
                    Self::validate_number(&call.arguments, "tap", "x")?,
                    "tap",
                    "x",
                )?;
                let y = Self::validate_non_negative(
                    Self::validate_number(&call.arguments, "tap", "y")?,
                    "tap",
                    "y",
                )?;
                json!({ "accepted": true, "x": x, "y": y })
            }
            "inputText" => {
                Self::validate_fields(&call.arguments, "inputText", &["text"])?;
                let text = call
                    .arguments
                    .get("text")
                    .and_then(Value::as_str)
                    .ok_or_else(|| DriverError::InvalidArguments {
                        action: "inputText".to_owned(),
                        message: "text must be a string".to_owned(),
                    })?;
                json!({ "accepted": true, "characters": text.chars().count() })
            }
            "scroll" => {
                Self::validate_fields(&call.arguments, "scroll", &["deltaX", "deltaY"])?;
                let delta_x = Self::validate_number(&call.arguments, "scroll", "deltaX")?;
                let delta_y = Self::validate_number(&call.arguments, "scroll", "deltaY")?;
                json!({ "accepted": true, "deltaX": delta_x, "deltaY": delta_y })
            }
            name => return Err(DriverError::UnknownAction(name.to_owned()).into()),
        };

        let after = self.observe(context).await?;
        let finished_at_ms = now_ms().max(started_at_ms);
        Ok(ActionResult {
            call_id: call.id,
            started_at_ms,
            finished_at_ms,
            output,
            before: None,
            evidence: after.screenshot.clone().into_iter().collect(),
            after: Some(after),
            execution: None,
        })
    }
}

#[cfg(test)]
mod tests {
    use std::{io::Cursor, sync::Arc, time::Duration};

    use devicerail_core::{
        DeviceDriver, DeviceRuntime, DriverError, EvidenceError, EvidenceInput, EvidenceMetadata,
        EvidenceOutput, EvidenceResult, EvidenceStore, ExecutionControl, GcPolicy, GcReport,
        MemoryEventStore, OperationContext, PutEvidence, ReleaseReport, RuntimeError,
        SessionEventStore, Sha256Digest, StartSession, StoredEvidence, TimeoutScope, now_ms,
    };
    use devicerail_evidence_fs::{FileEvidenceStore, FileEvidenceStoreConfig};
    use devicerail_protocol::{ActionCall, ActionDefinition, AssetRef, SessionId};
    use serde_json::json;
    use tempfile::TempDir;
    use tokio::io::AsyncReadExt as _;
    use uuid::Uuid;

    use super::{MOCK_SCREENSHOT_PNG, MockDriver};

    /// Keeps a disposable filesystem root alive for the full conformance run.
    /// `inner` is declared first so its file handles close before TempDir
    /// removes the tree.
    struct IsolatedFileEvidenceStore {
        inner: FileEvidenceStore,
        _root: TempDir,
    }

    impl IsolatedFileEvidenceStore {
        fn new() -> Self {
            let root = tempfile::tempdir().expect("temporary Evidence Store root");
            let inner = FileEvidenceStore::new(root.path(), FileEvidenceStoreConfig::default())
                .expect("isolated Evidence Store");
            Self { inner, _root: root }
        }
    }

    #[async_trait::async_trait]
    impl EvidenceStore for IsolatedFileEvidenceStore {
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

    fn isolated_evidence_store() -> Arc<dyn EvidenceStore> {
        Arc::new(IsolatedFileEvidenceStore::new())
    }

    async fn operation_context(events: &MemoryEventStore, driver: &MockDriver) -> OperationContext {
        let start = StartSession::new(None, Some(driver.id().clone()), now_ms());
        let context = OperationContext::new(start.session_id.clone(), None);
        events.start_session(start).await.expect("start session");
        context
    }

    fn valid_call(action: &ActionDefinition) -> Result<ActionCall, String> {
        let arguments = match action.name.as_str() {
            "tap" => json!({ "x": 120, "y": 80 }),
            "inputText" => json!({ "text": "DeviceRail" }),
            "scroll" => json!({ "deltaX": 0, "deltaY": 240 }),
            name => return Err(format!("no fixture for `{name}`")),
        };
        Ok(ActionCall {
            id: Uuid::new_v4(),
            name: action.name.clone(),
            arguments,
        })
    }

    devicerail_core::driver_conformance_test!(
        conforms_to_device_driver_contract,
        || MockDriver::new(format!("mock-conformance-{}", Uuid::new_v4())),
        valid_call,
    );

    devicerail_core::driver_conformance_test!(
        session_evidence_mode_conforms_to_device_driver_contract,
        || MockDriver::new(format!("mock-evidence-conformance-{}", Uuid::new_v4()))
            .with_session_evidence(),
        valid_call,
        isolated_evidence_store(),
    );

    #[tokio::test]
    async fn session_evidence_mode_fails_explicitly_without_a_store() {
        let driver = Arc::new(MockDriver::new("mock-no-store").with_session_evidence());
        driver
            .connect(&ExecutionControl::unbounded())
            .await
            .expect("connect");
        let events = Arc::new(MemoryEventStore::default());
        let runtime = DeviceRuntime::new(Arc::clone(&driver), Arc::clone(&events));
        let context = operation_context(&events, &driver).await;

        let error = runtime
            .observe(&context)
            .await
            .expect_err("missing Store must not produce an Observation");
        assert!(matches!(
            error,
            RuntimeError::Evidence(EvidenceError::Unavailable)
        ));
    }

    #[tokio::test]
    async fn session_evidence_is_canonical_pinned_to_each_session_and_decodable() {
        let root = tempfile::tempdir().expect("temporary Evidence Store root");
        let evidence = Arc::new(
            FileEvidenceStore::new(root.path(), FileEvidenceStoreConfig::default())
                .expect("Evidence Store"),
        );
        let runtime_evidence: Arc<dyn EvidenceStore> = evidence.clone();
        let driver = Arc::new(MockDriver::new("mock-store").with_session_evidence());
        driver
            .connect(&ExecutionControl::unbounded())
            .await
            .expect("connect");
        let events = Arc::new(MemoryEventStore::default());
        let runtime = DeviceRuntime::with_evidence(
            Arc::clone(&driver),
            Arc::clone(&events),
            runtime_evidence,
        );
        let first_context = operation_context(&events, &driver).await;
        let second_context = operation_context(&events, &driver).await;

        let first = runtime
            .observe(&first_context)
            .await
            .expect("first Observation");
        assert_eq!(
            evidence
                .referenced_sessions()
                .await
                .expect("referenced Sessions"),
            vec![first_context.session_id.clone()]
        );

        let second = runtime
            .observe(&second_context)
            .await
            .expect("second Observation");
        let mut expected_sessions = vec![
            first_context.session_id.clone(),
            second_context.session_id.clone(),
        ];
        expected_sessions.sort();
        assert_eq!(
            evidence
                .referenced_sessions()
                .await
                .expect("referenced Sessions"),
            expected_sessions
        );

        let first_asset = first.screenshot.expect("first screenshot");
        let second_asset = second.screenshot.expect("second screenshot");
        assert_eq!(
            first_asset, second_asset,
            "fixed PNG content is de-duplicated"
        );
        assert_eq!(first.metadata["frameSequence"], 1);
        assert_eq!(second.metadata["frameSequence"], 2);

        let digest = FileEvidenceStore::validate_asset_ref(&first_asset)
            .expect("canonical Store-owned AssetRef");
        let mut stream = evidence.open(&digest).await.expect("open screenshot bytes");
        let mut bytes = Vec::new();
        stream
            .read_to_end(&mut bytes)
            .await
            .expect("read screenshot bytes");
        assert_eq!(bytes, MOCK_SCREENSHOT_PNG);

        let decoder = png::Decoder::new(Cursor::new(bytes));
        let mut reader = decoder.read_info().expect("decode PNG header");
        let mut pixels = vec![0; reader.output_buffer_size().expect("bounded PNG output")];
        let info = reader.next_frame(&mut pixels).expect("decode PNG pixels");
        assert_eq!((info.width, info.height), (1, 1));
        assert_eq!(info.color_type, png::ColorType::Rgba);
        assert_eq!(info.bit_depth, png::BitDepth::Eight);
        assert_eq!(&pixels[..info.buffer_size()], &[0x52, 0x9c, 0xea, 0xff]);
    }

    #[tokio::test]
    async fn requires_connection_before_observe() {
        let driver = Arc::new(MockDriver::new("mock-1"));
        let events = Arc::new(MemoryEventStore::default());
        let runtime = DeviceRuntime::new(Arc::clone(&driver), Arc::clone(&events));
        let context = operation_context(&events, &driver).await;
        let error = runtime
            .observe(&context)
            .await
            .expect_err("must reject observe");
        assert!(matches!(
            error,
            RuntimeError::Driver(DriverError::NotConnected(_))
        ));
    }

    #[tokio::test]
    async fn tap_result_preserves_mock_specific_output_and_frame_sequence() {
        let driver = Arc::new(MockDriver::new("mock-1"));
        let control = ExecutionControl::unbounded();
        driver.connect(&control).await.expect("connect");
        let events = Arc::new(MemoryEventStore::default());
        let runtime = DeviceRuntime::new(Arc::clone(&driver), Arc::clone(&events));
        let context = operation_context(&events, &driver).await;

        let actions = driver.capabilities(&control).await.expect("capabilities");
        assert!(actions.iter().any(|action| action.name == "tap"));

        let result = runtime
            .execute(
                &context,
                ActionCall {
                    id: Uuid::nil(),
                    name: "tap".to_owned(),
                    arguments: json!({ "x": 120, "y": 80 }),
                },
            )
            .await
            .expect("execute tap");

        assert_eq!(result.output["accepted"], true);
        assert!(result.after.is_some());
        assert_eq!(result.evidence.len(), 1);
        assert_eq!(result.evidence[0].uri, "mock://screenshots/1.png");

        let next = runtime.observe(&context).await.expect("next frame");
        assert_eq!(next.metadata["frameSequence"], 2);
        assert_eq!(
            next.screenshot.expect("screenshot").uri,
            "mock://screenshots/2.png"
        );
    }

    #[tokio::test]
    async fn delayed_mock_observes_a_direct_driver_deadline() {
        let driver =
            Arc::new(MockDriver::new("mock-delay").with_action_delay(Duration::from_secs(30)));
        let connect = ExecutionControl::unbounded();
        driver.connect(&connect).await.expect("connect");
        let events = Arc::new(MemoryEventStore::default());
        let runtime = DeviceRuntime::new(Arc::clone(&driver), Arc::clone(&events));
        let context = operation_context(&events, &driver)
            .await
            .with_action_timeout_ms(10);

        let error = tokio::time::timeout(
            Duration::from_secs(1),
            runtime.execute(
                &context,
                ActionCall {
                    id: Uuid::new_v4(),
                    name: "tap".to_owned(),
                    arguments: json!({ "x": 1, "y": 1 }),
                },
            ),
        )
        .await
        .expect("driver deadline is prompt")
        .expect_err("driver timeout");
        assert!(matches!(
            error,
            RuntimeError::TimedOut {
                scope: TimeoutScope::Action,
                timeout_ms: 10
            }
        ));
    }
}
