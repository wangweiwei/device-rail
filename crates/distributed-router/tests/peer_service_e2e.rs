use std::{
    future::pending,
    io::Cursor,
    pin::Pin,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    task::{Context, Poll},
    time::Duration,
};

use async_trait::async_trait;
use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
use devicerail_core::{
    CancellationReason, DeviceDriver, DeviceOperationResult, DeviceRuntime, DriverError,
    DriverOperationContext, DriverRegistry, DriverResult, EndSession, EvidenceInput,
    EvidenceMetadata, EvidenceOutput, EvidenceResult, EvidenceStore, ExecutionControl,
    ExecutionController, GcPolicy, GcReport, LeaseOwnerId, MemoryEventStore, OperationContext,
    PutEvidence, ReleaseReport, ScreenshotPolicy, SessionEventStore, Sha256Digest, StartSession,
    StoredEvidence, TimeoutScope, cleanup_ended_session, now_ms,
};
use devicerail_distributed_router::{
    MemoryTelemetry, NdjsonPeerTransport, NodeId, OperationMethod, OperationOutcome, PeerLease,
    PeerOperation, PeerRequest, PeerResponse, PeerResult, PeerSecurity, PeerServerError,
    PeerTransport, RegistryPeerService, RemoteDriverConfig, RemoteNode, RouterConfig,
    TelemetrySink, serve_peer_stream, serve_peer_stream_until_cancelled,
};
use devicerail_driver_mock::MockDriver;
use devicerail_evidence_fs::{FileEvidenceStore, FileEvidenceStoreConfig};
use devicerail_protocol::{
    ActionCall, ActionDefinition, ActionProtection, ActionResult, AssetRef, DeviceId, DeviceInfo,
    Observation, ScreenshotOmissionReason, SessionId, SessionOutcome, Viewport,
};
use serde_json::json;
use sha2::{Digest as _, Sha256};
use tokio::io::{
    AsyncBufReadExt as _, AsyncRead, AsyncReadExt as _, AsyncWriteExt as _, BufStream,
    DuplexStream, ReadBuf,
};
use uuid::Uuid;

const NODE_EPOCH: u64 = 17;
const LARGE_EVIDENCE_BYTES: usize = 16 * 1024 * 1024;

struct LargeEvidenceDriver {
    inner: MockDriver,
}

impl LargeEvidenceDriver {
    fn new() -> Self {
        Self {
            inner: MockDriver::new("large-evidence-device"),
        }
    }

    fn device_info(&self) -> DeviceInfo {
        self.inner.device_info()
    }
}

#[async_trait]
impl DeviceDriver for LargeEvidenceDriver {
    fn id(&self) -> &DeviceId {
        self.inner.id()
    }

    async fn connect(&self, control: &ExecutionControl) -> DriverResult<DeviceInfo> {
        self.inner.connect(control).await
    }

    async fn disconnect(&self, control: &ExecutionControl) -> DriverResult<()> {
        self.inner.disconnect(control).await
    }

    async fn capabilities(
        &self,
        control: &ExecutionControl,
    ) -> DriverResult<Vec<ActionDefinition>> {
        self.inner.capabilities(control).await
    }

    async fn health_check(&self, control: &ExecutionControl) -> DriverResult<()> {
        self.inner.health_check(control).await
    }

    fn action_protection(&self, name: &str) -> Option<ActionProtection> {
        self.inner.action_protection(name)
    }

    async fn observe(
        &self,
        context: &DriverOperationContext,
    ) -> DeviceOperationResult<Observation> {
        let (screenshot, screenshot_omission) = match context.screenshot_policy() {
            ScreenshotPolicy::Capture => {
                let stored = context
                    .evidence()
                    .put_with_declared_size(
                        "application/octet-stream",
                        LARGE_EVIDENCE_BYTES as u64,
                        Box::pin(Cursor::new(vec![0x5a; LARGE_EVIDENCE_BYTES])),
                    )
                    .await?;
                (Some(stored.asset_ref()), None)
            }
            ScreenshotPolicy::Omit => (None, Some(ScreenshotOmissionReason::Policy)),
        };
        Ok(Observation {
            id: Uuid::new_v4(),
            device_id: self.id().clone(),
            captured_at_ms: now_ms(),
            viewport: Viewport {
                width: 1,
                height: 1,
                scale_factor: 1.0,
            },
            screenshot,
            screenshot_omission,
            ui_snapshot: None,
            ui_snapshot_omission: context
                .ui_snapshots_enabled()
                .then_some(devicerail_protocol::UiSnapshotOmissionReason::DriverUnsupported),
            metadata: Default::default(),
        })
    }

    async fn execute(
        &self,
        context: &DriverOperationContext,
        call: ActionCall,
    ) -> DeviceOperationResult<ActionResult> {
        self.inner.execute(context, call).await
    }
}

struct CountingEvidenceReader {
    inner: EvidenceOutput,
    read_bytes: Arc<AtomicUsize>,
}

impl AsyncRead for CountingEvidenceReader {
    fn poll_read(
        mut self: Pin<&mut Self>,
        context: &mut Context<'_>,
        buffer: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        let before = buffer.filled().len();
        let result = self.inner.as_mut().poll_read(context, buffer);
        if matches!(result, Poll::Ready(Ok(()))) {
            self.read_bytes
                .fetch_add(buffer.filled().len() - before, Ordering::SeqCst);
        }
        result
    }
}

struct CountingEvidenceStore {
    inner: Arc<FileEvidenceStore>,
    open_calls: AtomicUsize,
    read_bytes: Arc<AtomicUsize>,
}

impl CountingEvidenceStore {
    fn new(inner: Arc<FileEvidenceStore>) -> Self {
        Self {
            inner,
            open_calls: AtomicUsize::new(0),
            read_bytes: Arc::new(AtomicUsize::new(0)),
        }
    }
}

#[async_trait]
impl EvidenceStore for CountingEvidenceStore {
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
        self.open_calls.fetch_add(1, Ordering::SeqCst);
        Ok(Box::pin(CountingEvidenceReader {
            inner: self.inner.open(digest).await?,
            read_bytes: Arc::clone(&self.read_bytes),
        }))
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

struct CountingDriver {
    inner: MockDriver,
    execute_calls: AtomicUsize,
    observe_feature_flags: AtomicUsize,
    execute_feature_flags: AtomicUsize,
    health_calls: AtomicUsize,
    disconnect_calls: AtomicUsize,
    disconnect_failures: AtomicUsize,
    connect_failures: AtomicUsize,
    connect_blocks: AtomicUsize,
    panic_capabilities: bool,
    connect_started: tokio::sync::Notify,
}

impl CountingDriver {
    fn new() -> Self {
        Self::with_id("node-device")
    }

    fn with_id(id: &str) -> Self {
        Self {
            inner: MockDriver::new(id)
                .with_session_evidence()
                .with_action_delay(Duration::from_millis(250)),
            execute_calls: AtomicUsize::new(0),
            observe_feature_flags: AtomicUsize::new(0),
            execute_feature_flags: AtomicUsize::new(0),
            health_calls: AtomicUsize::new(0),
            disconnect_calls: AtomicUsize::new(0),
            disconnect_failures: AtomicUsize::new(0),
            connect_failures: AtomicUsize::new(0),
            connect_blocks: AtomicUsize::new(0),
            panic_capabilities: false,
            connect_started: tokio::sync::Notify::new(),
        }
    }

    fn with_disconnect_failures(self, count: usize) -> Self {
        self.disconnect_failures.store(count, Ordering::SeqCst);
        self
    }

    fn with_connect_failures(self, count: usize) -> Self {
        self.connect_failures.store(count, Ordering::SeqCst);
        self
    }

    fn with_blocked_connect(self) -> Self {
        self.connect_blocks.store(1, Ordering::SeqCst);
        self
    }

    fn with_panicking_capabilities(mut self) -> Self {
        self.panic_capabilities = true;
        self
    }

    fn device_info(&self) -> DeviceInfo {
        self.inner.device_info()
    }
}

#[async_trait]
impl DeviceDriver for CountingDriver {
    fn id(&self) -> &DeviceId {
        self.inner.id()
    }

    async fn connect(&self, control: &ExecutionControl) -> DriverResult<DeviceInfo> {
        let info = self.inner.connect(control).await?;
        if self
            .connect_failures
            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |remaining| {
                remaining.checked_sub(1)
            })
            .is_ok()
        {
            return Err(DriverError::Platform {
                code: "fixture_connect_transient".to_owned(),
                retryable: true,
            });
        }
        if self
            .connect_blocks
            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |remaining| {
                remaining.checked_sub(1)
            })
            .is_ok()
        {
            self.connect_started.notify_one();
            pending::<()>().await;
        }
        Ok(info)
    }

    async fn disconnect(&self, control: &ExecutionControl) -> DriverResult<()> {
        self.disconnect_calls.fetch_add(1, Ordering::SeqCst);
        if self
            .disconnect_failures
            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |remaining| {
                remaining.checked_sub(1)
            })
            .is_ok()
        {
            return Err(DriverError::Platform {
                code: "fixture_disconnect_transient".to_owned(),
                retryable: true,
            });
        }
        self.inner.disconnect(control).await
    }

    async fn capabilities(
        &self,
        control: &ExecutionControl,
    ) -> DriverResult<Vec<ActionDefinition>> {
        assert!(!self.panic_capabilities, "fixture capabilities panic");
        self.inner.capabilities(control).await
    }

    async fn health_check(&self, control: &ExecutionControl) -> DriverResult<()> {
        self.health_calls.fetch_add(1, Ordering::SeqCst);
        self.inner.health_check(control).await
    }

    fn action_protection(&self, name: &str) -> Option<ActionProtection> {
        self.inner.action_protection(name)
    }

    async fn observe(
        &self,
        context: &DriverOperationContext,
    ) -> DeviceOperationResult<Observation> {
        self.observe_feature_flags.store(
            usize::from(context.ui_snapshots_enabled())
                | (usize::from(context.semantic_actions_enabled()) << 1),
            Ordering::SeqCst,
        );
        self.inner.observe(context).await
    }

    async fn execute(
        &self,
        context: &DriverOperationContext,
        call: ActionCall,
    ) -> DeviceOperationResult<ActionResult> {
        self.execute_calls.fetch_add(1, Ordering::SeqCst);
        self.execute_feature_flags.store(
            usize::from(context.ui_snapshots_enabled())
                | (usize::from(context.semantic_actions_enabled()) << 1),
            Ordering::SeqCst,
        );
        self.inner.execute(context, call).await
    }
}

async fn write_request(stream: &mut BufStream<DuplexStream>, request: &PeerRequest) {
    let mut frame = serde_json::to_vec(request).expect("serialize peer request");
    frame.push(b'\n');
    stream.write_all(&frame).await.expect("write peer request");
    stream.flush().await.expect("flush peer request");
}

async fn read_response(stream: &mut BufStream<DuplexStream>) -> PeerResponse {
    let mut frame = Vec::new();
    let read = stream
        .read_until(b'\n', &mut frame)
        .await
        .expect("read peer response");
    assert!(read > 0, "peer closed before returning a response");
    assert_eq!(frame.pop(), Some(b'\n'));
    serde_json::from_slice(&frame).expect("decode peer response")
}

async fn round_trip(stream: &mut BufStream<DuplexStream>, request: PeerRequest) -> PeerResponse {
    write_request(stream, &request).await;
    let response = read_response(stream).await;
    response
        .validate_for(&request)
        .expect("response is correlated and valid");
    response
}

fn success(response: PeerResponse) -> PeerResult {
    assert!(response.ok, "peer returned error: {:?}", response.error);
    response.result.expect("successful peer result")
}

fn with_lease(node_id: &NodeId, operation: PeerOperation, lease: &PeerLease) -> PeerRequest {
    let mut request = PeerRequest::new(node_id.clone(), Some(NODE_EPOCH), operation);
    request.lease = Some(lease.clone());
    request
}

struct ServiceFixture {
    _remote_root: tempfile::TempDir,
    remote_evidence: Arc<FileEvidenceStore>,
    events: Arc<MemoryEventStore>,
    driver: Arc<CountingDriver>,
    registry: Arc<DriverRegistry<MemoryEventStore>>,
    node_id: NodeId,
    service: Arc<RegistryPeerService<MemoryEventStore>>,
    security: PeerSecurity,
}

async fn service_fixture(driver: CountingDriver, node_name: &str) -> ServiceFixture {
    let remote_root = tempfile::tempdir().expect("remote Evidence Store root");
    let remote_evidence = Arc::new(
        FileEvidenceStore::new(remote_root.path(), FileEvidenceStoreConfig::default())
            .expect("remote Evidence Store"),
    );
    let evidence: Arc<dyn EvidenceStore> = remote_evidence.clone();
    let events = Arc::new(MemoryEventStore::default());
    let registry = Arc::new(DriverRegistry::with_evidence(
        Arc::clone(&events),
        Arc::clone(&evidence),
    ));
    let driver = Arc::new(driver);
    let erased: Arc<dyn DeviceDriver> = driver.clone();
    registry
        .register(erased, driver.device_info())
        .await
        .expect("register node Driver");
    let node_id = NodeId::parse(node_name).expect("node id");
    let service = RegistryPeerService::new(
        node_id.clone(),
        NODE_EPOCH,
        1,
        Arc::clone(&registry),
        Arc::clone(&events),
        evidence,
    )
    .await
    .expect("Registry-backed peer service");
    ServiceFixture {
        _remote_root: remote_root,
        remote_evidence,
        events,
        driver,
        registry,
        node_id,
        service,
        security: PeerSecurity::external_tunnel("client-a").expect("tunnel attestation"),
    }
}

async fn raw_acquire(
    client: &mut BufStream<DuplexStream>,
    node_id: &NodeId,
    security: &PeerSecurity,
) -> (String, PeerLease) {
    success(
        round_trip(
            client,
            PeerRequest::new(node_id.clone(), None, PeerOperation::Hello),
        )
        .await,
    );
    let inventory = success(
        round_trip(
            client,
            PeerRequest::new(node_id.clone(), None, PeerOperation::Inventory),
        )
        .await,
    );
    let PeerResult::Inventory { inventory } = inventory else {
        panic!("inventory result expected");
    };
    let device_key = inventory.devices[0].device_key.clone();
    let lease = success(
        round_trip(
            client,
            PeerRequest::new(
                node_id.clone(),
                Some(NODE_EPOCH),
                PeerOperation::LeaseAcquire {
                    device_key: device_key.clone(),
                    owner_id: security.subject().to_owned(),
                    // Debug and sanitizer builds can spend well over 30s in
                    // the bounded 16 MiB Evidence transfer test. Lease expiry
                    // is not under test in this shared setup.
                    ttl_ms: 300_000,
                },
            ),
        )
        .await,
    );
    let PeerResult::Lease { lease } = lease else {
        panic!("lease result expected");
    };
    (device_key, lease)
}

async fn raw_connect(
    client: &mut BufStream<DuplexStream>,
    node_id: &NodeId,
    security: &PeerSecurity,
) -> (String, PeerLease) {
    let (device_key, lease) = raw_acquire(client, node_id, security).await;
    success(
        round_trip(
            client,
            with_lease(
                node_id,
                PeerOperation::Connect {
                    device_key: device_key.clone(),
                },
                &lease,
            ),
        )
        .await,
    );
    (device_key, lease)
}

#[tokio::test]
async fn peer_server_rejects_v1_with_eof_and_an_explicit_version_error() {
    let fixture = service_fixture(CountingDriver::new(), "node-reject-v1").await;
    let (client_stream, server_stream) = tokio::io::duplex(64 * 1024);
    let server = tokio::spawn(serve_peer_stream(
        server_stream,
        fixture.security.clone(),
        Arc::clone(&fixture.service),
    ));
    let mut client = BufStream::new(client_stream);
    let request = PeerRequest::new(fixture.node_id.clone(), None, PeerOperation::Hello);
    let mut value = serde_json::to_value(request).expect("serialize current request");
    value["protocolVersion"] = json!(1);
    let mut frame = serde_json::to_vec(&value).expect("serialize v1 request");
    frame.push(b'\n');
    client.write_all(&frame).await.expect("write v1 request");
    client.flush().await.expect("flush v1 request");

    let mut trailing = Vec::new();
    let read = tokio::time::timeout(
        Duration::from_secs(2),
        client.read_until(b'\n', &mut trailing),
    )
    .await
    .expect("server closes v1 stream promptly")
    .expect("read closed v1 stream");
    assert_eq!(read, 0, "v1 must not receive a misleading response");
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(2), server)
            .await
            .expect("v1 server stopped")
            .expect("v1 server task"),
        Err(PeerServerError::UnsupportedVersion)
    );
}

#[tokio::test]
async fn peer_v2_preserves_operation_scoped_ui_feature_gates_at_the_node_driver() {
    let fixture = service_fixture(CountingDriver::new(), "node-feature-gates").await;
    let (client_stream, server_stream) = tokio::io::duplex(256 * 1024);
    let server = tokio::spawn(serve_peer_stream(
        server_stream,
        fixture.security.clone(),
        Arc::clone(&fixture.service),
    ));
    let mut client = BufStream::new(client_stream);
    let (device_key, lease) = raw_connect(&mut client, &fixture.node_id, &fixture.security).await;

    success(
        round_trip(
            &mut client,
            with_lease(
                &fixture.node_id,
                PeerOperation::Observe {
                    device_key: device_key.clone(),
                    screenshot_omission: None,
                    ui_snapshots_enabled: true,
                    semantic_actions_enabled: false,
                },
                &lease,
            ),
        )
        .await,
    );
    success(
        round_trip(
            &mut client,
            with_lease(
                &fixture.node_id,
                PeerOperation::Execute {
                    device_key: device_key.clone(),
                    call: ActionCall {
                        id: Uuid::new_v4(),
                        name: "tap".to_owned(),
                        arguments: json!({"x": 1, "y": 1}),
                    },
                    screenshot_omission: None,
                    ui_snapshots_enabled: false,
                    semantic_actions_enabled: true,
                },
                &lease,
            ),
        )
        .await,
    );

    assert_eq!(
        fixture.driver.observe_feature_flags.load(Ordering::SeqCst),
        0b01
    );
    assert_eq!(
        fixture.driver.execute_feature_flags.load(Ordering::SeqCst),
        0b10
    );

    success(
        round_trip(
            &mut client,
            with_lease(
                &fixture.node_id,
                PeerOperation::Disconnect { device_key },
                &lease,
            ),
        )
        .await,
    );
    client.shutdown().await.expect("close client");
    drop(client);
    tokio::time::timeout(Duration::from_secs(2), server)
        .await
        .expect("server stopped")
        .expect("server task")
        .expect("clean peer shutdown");
}

#[tokio::test]
async fn starting_service_allows_discovery_but_not_leases_until_ready() {
    let fixture = service_fixture(CountingDriver::new(), "node-starting").await;
    fixture.service.mark_starting();
    assert!(!fixture.service.is_ready());
    let connection_id = Uuid::new_v4();

    let hello = fixture
        .service
        .handle(
            PeerRequest::new(fixture.node_id.clone(), None, PeerOperation::Hello),
            &fixture.security,
            connection_id,
        )
        .await
        .expect("starting hello");
    assert!(hello.ok);
    let inventory = fixture
        .service
        .handle(
            PeerRequest::new(fixture.node_id.clone(), None, PeerOperation::Inventory),
            &fixture.security,
            connection_id,
        )
        .await
        .expect("starting inventory");
    let PeerResult::Inventory { inventory } = success(inventory) else {
        panic!("inventory result expected");
    };
    let device_key = inventory.devices[0].device_key.clone();
    let health = fixture
        .service
        .handle(
            PeerRequest::new(
                fixture.node_id.clone(),
                Some(NODE_EPOCH),
                PeerOperation::Health,
            ),
            &fixture.security,
            connection_id,
        )
        .await
        .expect("starting health");
    assert!(health.ok);
    let capabilities = fixture
        .service
        .handle(
            PeerRequest::new(
                fixture.node_id.clone(),
                Some(NODE_EPOCH),
                PeerOperation::Capabilities {
                    device_key: device_key.clone(),
                },
            ),
            &fixture.security,
            connection_id,
        )
        .await
        .expect("starting capabilities");
    assert!(capabilities.ok);

    let acquire = || {
        PeerRequest::new(
            fixture.node_id.clone(),
            Some(NODE_EPOCH),
            PeerOperation::LeaseAcquire {
                device_key: device_key.clone(),
                owner_id: fixture.security.subject().to_owned(),
                ttl_ms: 30_000,
            },
        )
    };
    let blocked = fixture
        .service
        .handle(acquire(), &fixture.security, connection_id)
        .await
        .expect("starting lease response");
    let error = blocked.error.expect("starting gate error");
    assert_eq!(error.code, "node_starting");
    assert!(error.retryable);
    assert!(!error.outcome_unknown);

    fixture.service.mark_ready();
    assert!(fixture.service.is_ready());
    assert!(
        fixture
            .service
            .handle(acquire(), &fixture.security, connection_id)
            .await
            .expect("ready lease response")
            .ok
    );
    assert!(
        fixture
            .service
            .release_connection(connection_id)
            .await
            .is_empty()
    );
}

#[tokio::test]
async fn service_inventory_and_health_do_not_reexport_remote_routes() {
    let fixture = service_fixture(CountingDriver::new(), "node-local-health").await;
    let remote = Arc::new(CountingDriver::with_id("remote:upstream:device"));
    let erased: Arc<dyn DeviceDriver> = remote.clone();
    fixture
        .registry
        .register(erased, remote.device_info())
        .await
        .expect("register remote route after service snapshot");
    let connection_id = Uuid::new_v4();

    let inventory = fixture
        .service
        .handle(
            PeerRequest::new(fixture.node_id.clone(), None, PeerOperation::Inventory),
            &fixture.security,
            connection_id,
        )
        .await
        .expect("local inventory");
    let PeerResult::Inventory { inventory } = success(inventory) else {
        panic!("inventory result expected");
    };
    assert_eq!(inventory.devices.len(), 1);
    let health = fixture
        .service
        .handle(
            PeerRequest::new(
                fixture.node_id.clone(),
                Some(NODE_EPOCH),
                PeerOperation::Health,
            ),
            &fixture.security,
            connection_id,
        )
        .await
        .expect("local health");
    assert!(matches!(success(health), PeerResult::Health { .. }));
    assert_eq!(fixture.driver.health_calls.load(Ordering::SeqCst), 1);
    assert_eq!(remote.health_calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn cancelling_an_idle_peer_stream_runs_connection_cleanup() {
    let fixture = service_fixture(CountingDriver::new(), "node-server-shutdown").await;
    let (client_stream, server_stream) = tokio::io::duplex(256 * 1024);
    let (shutdown, shutdown_control) = ExecutionController::new();
    let server = tokio::spawn(serve_peer_stream_until_cancelled(
        server_stream,
        fixture.security.clone(),
        Arc::clone(&fixture.service),
        shutdown_control,
    ));
    let mut client = BufStream::new(client_stream);
    let (_device_key, _lease) = raw_connect(&mut client, &fixture.node_id, &fixture.security).await;

    assert!(shutdown.cancel(CancellationReason::Shutdown));
    tokio::time::timeout(Duration::from_secs(2), server)
        .await
        .expect("cancelled idle server stopped")
        .expect("server task")
        .expect("cancelled server cleaned up");
    assert!(fixture.service.shutdown().await.is_empty());
    assert_eq!(fixture.driver.disconnect_calls.load(Ordering::SeqCst), 1);
    assert!(
        fixture
            .events
            .list_sessions()
            .await
            .expect("sessions after server cancellation")
            .is_empty()
    );
    assert!(
        fixture
            .remote_evidence
            .referenced_sessions()
            .await
            .expect("Evidence after server cancellation")
            .is_empty()
    );
}

#[tokio::test]
async fn panicking_request_task_closes_the_stream_with_an_explicit_error() {
    let fixture = service_fixture(
        CountingDriver::new().with_panicking_capabilities(),
        "node-request-panic",
    )
    .await;
    let (client_stream, server_stream) = tokio::io::duplex(256 * 1024);
    let server = tokio::spawn(serve_peer_stream(
        server_stream,
        fixture.security.clone(),
        Arc::clone(&fixture.service),
    ));
    let mut client = BufStream::new(client_stream);
    success(
        round_trip(
            &mut client,
            PeerRequest::new(fixture.node_id.clone(), None, PeerOperation::Hello),
        )
        .await,
    );
    let inventory = success(
        round_trip(
            &mut client,
            PeerRequest::new(fixture.node_id.clone(), None, PeerOperation::Inventory),
        )
        .await,
    );
    let PeerResult::Inventory { inventory } = inventory else {
        panic!("inventory result expected");
    };
    write_request(
        &mut client,
        &PeerRequest::new(
            fixture.node_id.clone(),
            Some(NODE_EPOCH),
            PeerOperation::Capabilities {
                device_key: inventory.devices[0].device_key.clone(),
            },
        ),
    )
    .await;

    let mut trailing = Vec::new();
    let read = tokio::time::timeout(
        Duration::from_secs(2),
        client.read_until(b'\n', &mut trailing),
    )
    .await
    .expect("panicking request closes the stream promptly")
    .expect("read closed peer stream");
    assert_eq!(
        read, 0,
        "a panicking request must not leave the client waiting"
    );
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(2), server)
            .await
            .expect("peer server stopped")
            .expect("peer server task"),
        Err(PeerServerError::Task)
    );
    let shutdown_errors = fixture.service.shutdown().await;
    assert!(
        shutdown_errors.is_empty(),
        "service cleanup after request panic failed: {shutdown_errors:?}"
    );
    assert_eq!(fixture.driver.disconnect_calls.load(Ordering::SeqCst), 0);
    let pool_entries = fixture.registry.pool_entries(now_ms()).await;
    assert!(
        pool_entries.iter().all(|entry| entry.lease.is_none()),
        "Core leases remain after request panic: {pool_entries:?}"
    );
    assert!(
        fixture
            .events
            .list_sessions()
            .await
            .expect("sessions after request panic")
            .is_empty()
    );
    assert!(
        fixture
            .remote_evidence
            .referenced_sessions()
            .await
            .expect("Evidence after request panic")
            .is_empty()
    );
}

#[tokio::test]
async fn sixteen_mib_evidence_uses_one_continuous_store_reader() {
    let remote_root = tempfile::tempdir().expect("remote Evidence Store root");
    let file_store = Arc::new(
        FileEvidenceStore::new(remote_root.path(), FileEvidenceStoreConfig::default())
            .expect("remote Evidence Store"),
    );
    let counting_store = Arc::new(CountingEvidenceStore::new(Arc::clone(&file_store)));
    let evidence: Arc<dyn EvidenceStore> = counting_store.clone();
    let events = Arc::new(MemoryEventStore::default());
    let registry = Arc::new(DriverRegistry::with_evidence(
        Arc::clone(&events),
        Arc::clone(&evidence),
    ));
    let driver = Arc::new(LargeEvidenceDriver::new());
    registry
        .register(driver.clone(), driver.device_info())
        .await
        .expect("register large Evidence Driver");
    let node_id = NodeId::parse("node-large-evidence").expect("node id");
    let service =
        RegistryPeerService::new(node_id.clone(), NODE_EPOCH, 1, registry, events, evidence)
            .await
            .expect("Registry-backed peer service");
    let security = PeerSecurity::external_tunnel("client-large-evidence").expect("security");
    let connection_id = Uuid::new_v4();

    let inventory = service
        .handle(
            PeerRequest::new(node_id.clone(), None, PeerOperation::Inventory),
            &security,
            connection_id,
        )
        .await
        .expect("inventory response");
    let PeerResult::Inventory { inventory } = success(inventory) else {
        panic!("inventory result expected");
    };
    let device_key = inventory.devices[0].device_key.clone();
    let lease_response = service
        .handle(
            PeerRequest::new(
                node_id.clone(),
                Some(NODE_EPOCH),
                PeerOperation::LeaseAcquire {
                    device_key: device_key.clone(),
                    owner_id: security.subject().to_owned(),
                    // Slow debug/sanitizer builds can spend over 30 seconds
                    // hashing and streaming the bounded 16 MiB fixture.
                    ttl_ms: 300_000,
                },
            ),
            &security,
            connection_id,
        )
        .await
        .expect("lease response");
    let PeerResult::Lease { lease } = success(lease_response) else {
        panic!("lease result expected");
    };
    success(
        service
            .handle(
                with_lease(
                    &node_id,
                    PeerOperation::Connect {
                        device_key: device_key.clone(),
                    },
                    &lease,
                ),
                &security,
                connection_id,
            )
            .await
            .expect("connect response"),
    );
    let observation = service
        .handle(
            with_lease(
                &node_id,
                PeerOperation::Observe {
                    device_key: device_key.clone(),
                    screenshot_omission: None,
                    ui_snapshots_enabled: false,
                    semantic_actions_enabled: false,
                },
                &lease,
            ),
            &security,
            connection_id,
        )
        .await
        .expect("Observation response");
    let PeerResult::Observation { observation } = success(observation) else {
        panic!("Observation result expected");
    };
    let asset = observation.screenshot.expect("large Evidence reference");

    let mut offset = 0_u64;
    let mut chunks = 0_usize;
    let mut digest = Sha256::new();
    loop {
        let response = service
            .handle(
                with_lease(
                    &node_id,
                    PeerOperation::EvidenceRead {
                        device_key: device_key.clone(),
                        evidence_id: asset.id.clone(),
                        offset,
                        max_bytes: 256 * 1024,
                    },
                    &lease,
                ),
                &security,
                connection_id,
            )
            .await
            .expect("Evidence chunk response");
        let PeerResult::EvidenceChunk {
            total_size,
            offset: chunk_offset,
            data_base64,
            done,
            ..
        } = success(response)
        else {
            panic!("Evidence chunk expected");
        };
        assert_eq!(total_size, LARGE_EVIDENCE_BYTES as u64);
        assert_eq!(chunk_offset, offset);
        let chunk = BASE64.decode(data_base64).expect("canonical chunk base64");
        digest.update(&chunk);
        offset += chunk.len() as u64;
        chunks += 1;
        if done {
            break;
        }
    }

    assert_eq!(offset, LARGE_EVIDENCE_BYTES as u64);
    assert_eq!(chunks, 64);
    assert_eq!(
        hex::encode(digest.finalize()),
        asset.sha256.expect("Evidence digest")
    );
    assert_eq!(counting_store.open_calls.load(Ordering::SeqCst), 1);
    assert_eq!(
        counting_store.read_bytes.load(Ordering::SeqCst),
        LARGE_EVIDENCE_BYTES
    );
    assert!(service.release_connection(connection_id).await.is_empty());
}

#[tokio::test]
async fn aborted_connect_keeps_enough_state_for_connection_cleanup() {
    let fixture = service_fixture(CountingDriver::new().with_blocked_connect(), "node-drop").await;
    let connection_id = Uuid::new_v4();
    let inventory = fixture
        .service
        .handle(
            PeerRequest::new(fixture.node_id.clone(), None, PeerOperation::Inventory),
            &fixture.security,
            connection_id,
        )
        .await
        .expect("inventory response");
    let PeerResult::Inventory { inventory } = success(inventory) else {
        panic!("inventory result expected");
    };
    let device_key = inventory.devices[0].device_key.clone();
    let lease_response = fixture
        .service
        .handle(
            PeerRequest::new(
                fixture.node_id.clone(),
                Some(NODE_EPOCH),
                PeerOperation::LeaseAcquire {
                    device_key: device_key.clone(),
                    owner_id: fixture.security.subject().to_owned(),
                    ttl_ms: 30_000,
                },
            ),
            &fixture.security,
            connection_id,
        )
        .await
        .expect("lease response");
    let PeerResult::Lease { lease } = success(lease_response) else {
        panic!("lease result expected");
    };
    let connect_request = with_lease(
        &fixture.node_id,
        PeerOperation::Connect {
            device_key: device_key.clone(),
        },
        &lease,
    );
    let connect = tokio::spawn({
        let service = Arc::clone(&fixture.service);
        let security = fixture.security.clone();
        async move {
            service
                .handle(connect_request, &security, connection_id)
                .await
        }
    });
    fixture.driver.connect_started.notified().await;
    connect.abort();
    assert!(
        connect
            .await
            .expect_err("connect task was aborted")
            .is_cancelled()
    );

    let reconnected = tokio::time::timeout(
        Duration::from_secs(2),
        fixture.service.handle(
            with_lease(
                &fixture.node_id,
                PeerOperation::Connect {
                    device_key: device_key.clone(),
                },
                &lease,
            ),
            &fixture.security,
            connection_id,
        ),
    )
    .await
    .expect("same-lease reconnect must not remain stuck behind aborted initialization")
    .expect("reconnect response");
    assert!(matches!(
        success(reconnected),
        PeerResult::Device { device } if device.connected
    ));
    assert_eq!(
        fixture
            .events
            .list_sessions()
            .await
            .expect("reconnected Session")
            .len(),
        1
    );

    let observation = fixture
        .service
        .handle(
            with_lease(
                &fixture.node_id,
                PeerOperation::Observe {
                    device_key: device_key.clone(),
                    screenshot_omission: None,
                    ui_snapshots_enabled: false,
                    semantic_actions_enabled: false,
                },
                &lease,
            ),
            &fixture.security,
            connection_id,
        )
        .await
        .expect("observe response after reconnect");
    assert!(matches!(
        success(observation),
        PeerResult::Observation { observation } if observation.screenshot.is_some()
    ));
    let disconnected = fixture
        .service
        .handle(
            with_lease(
                &fixture.node_id,
                PeerOperation::Disconnect {
                    device_key: device_key.clone(),
                },
                &lease,
            ),
            &fixture.security,
            connection_id,
        )
        .await
        .expect("disconnect response after reconnect");
    assert!(matches!(success(disconnected), PeerResult::Ack));
    assert!(
        fixture
            .events
            .list_sessions()
            .await
            .expect("no leaked Session")
            .is_empty()
    );
    assert!(
        fixture
            .remote_evidence
            .referenced_sessions()
            .await
            .expect("no leaked Evidence references")
            .is_empty()
    );
    assert_eq!(fixture.driver.disconnect_calls.load(Ordering::SeqCst), 1);
    assert!(
        fixture
            .service
            .release_connection(connection_id)
            .await
            .is_empty()
    );

    let replacement_connection = Uuid::new_v4();
    let replacement = fixture
        .service
        .handle(
            PeerRequest::new(
                fixture.node_id.clone(),
                Some(NODE_EPOCH),
                PeerOperation::LeaseAcquire {
                    device_key,
                    owner_id: fixture.security.subject().to_owned(),
                    ttl_ms: 30_000,
                },
            ),
            &fixture.security,
            replacement_connection,
        )
        .await
        .expect("replacement lease response");
    assert!(replacement.ok);
    assert!(
        fixture
            .service
            .release_connection(replacement_connection)
            .await
            .is_empty()
    );
}

#[tokio::test]
async fn driver_connect_error_rolls_back_without_waiting_on_its_own_core_guard() {
    let fixture = service_fixture(
        CountingDriver::new().with_connect_failures(1),
        "node-connect-error",
    )
    .await;
    let (client_stream, server_stream) = tokio::io::duplex(256 * 1024);
    let server = tokio::spawn(serve_peer_stream(
        server_stream,
        fixture.security.clone(),
        Arc::clone(&fixture.service),
    ));
    let mut client = BufStream::new(client_stream);
    let (device_key, lease) = raw_acquire(&mut client, &fixture.node_id, &fixture.security).await;

    let failed = tokio::time::timeout(
        Duration::from_secs(2),
        round_trip(
            &mut client,
            with_lease(
                &fixture.node_id,
                PeerOperation::Connect {
                    device_key: device_key.clone(),
                },
                &lease,
            ),
        ),
    )
    .await
    .expect("connect rollback must not self-deadlock");
    assert_eq!(
        failed.error.as_ref().map(|error| error.code.as_str()),
        Some("platform_error")
    );
    assert_eq!(fixture.driver.disconnect_calls.load(Ordering::SeqCst), 1);
    assert!(
        fixture
            .events
            .list_sessions()
            .await
            .expect("failed connect Session cleanup")
            .is_empty()
    );
    assert!(
        fixture
            .remote_evidence
            .referenced_sessions()
            .await
            .expect("failed connect Evidence cleanup")
            .is_empty()
    );

    success(
        tokio::time::timeout(
            Duration::from_secs(2),
            round_trip(
                &mut client,
                with_lease(
                    &fixture.node_id,
                    PeerOperation::Connect {
                        device_key: device_key.clone(),
                    },
                    &lease,
                ),
            ),
        )
        .await
        .expect("same peer lease can reconnect after complete rollback"),
    );

    client.shutdown().await.expect("close client write half");
    drop(client);
    tokio::time::timeout(Duration::from_secs(2), server)
        .await
        .expect("server stopped after EOF")
        .expect("server task")
        .expect("clean peer shutdown");
    assert_eq!(fixture.driver.disconnect_calls.load(Ordering::SeqCst), 2);
    assert!(
        fixture
            .events
            .list_sessions()
            .await
            .expect("reconnected Session cleanup")
            .is_empty()
    );
}

#[tokio::test]
async fn incomplete_connect_rollback_is_outcome_unknown_and_later_disconnect_converges() {
    let fixture = service_fixture(
        CountingDriver::new()
            .with_connect_failures(1)
            .with_disconnect_failures(1),
        "node-connect-rollback",
    )
    .await;
    let (client_stream, server_stream) = tokio::io::duplex(256 * 1024);
    let server = tokio::spawn(serve_peer_stream(
        server_stream,
        fixture.security.clone(),
        Arc::clone(&fixture.service),
    ));
    let mut client = BufStream::new(client_stream);
    let (device_key, lease) = raw_acquire(&mut client, &fixture.node_id, &fixture.security).await;

    let failed = tokio::time::timeout(
        Duration::from_secs(2),
        round_trip(
            &mut client,
            with_lease(
                &fixture.node_id,
                PeerOperation::Connect {
                    device_key: device_key.clone(),
                },
                &lease,
            ),
        ),
    )
    .await
    .expect("incomplete rollback returns a bounded response");
    let error = failed.error.expect("explicit incomplete rollback error");
    assert_eq!(error.code, "connect_rollback_incomplete");
    assert!(error.retryable);
    assert!(error.outcome_unknown);
    assert_eq!(fixture.driver.disconnect_calls.load(Ordering::SeqCst), 1);

    success(
        round_trip(
            &mut client,
            with_lease(
                &fixture.node_id,
                PeerOperation::Disconnect {
                    device_key: device_key.clone(),
                },
                &lease,
            ),
        )
        .await,
    );
    assert_eq!(fixture.driver.disconnect_calls.load(Ordering::SeqCst), 2);
    assert!(
        fixture
            .events
            .list_sessions()
            .await
            .expect("rollback Session cleanup")
            .is_empty()
    );
    assert!(
        fixture
            .remote_evidence
            .referenced_sessions()
            .await
            .expect("rollback Evidence cleanup")
            .is_empty()
    );
    success(
        round_trip(
            &mut client,
            with_lease(
                &fixture.node_id,
                PeerOperation::LeaseRelease {
                    lease: lease.clone(),
                },
                &lease,
            ),
        )
        .await,
    );

    client.shutdown().await.expect("close client write half");
    drop(client);
    tokio::time::timeout(Duration::from_secs(2), server)
        .await
        .expect("server stopped after EOF")
        .expect("server task")
        .expect("clean peer shutdown");
}

#[tokio::test]
async fn cleanup_survives_external_core_lease_release_and_route_removal() {
    let fixture = service_fixture(CountingDriver::new(), "node-route-removed").await;
    let (client_stream, server_stream) = tokio::io::duplex(256 * 1024);
    let server = tokio::spawn(serve_peer_stream(
        server_stream,
        fixture.security.clone(),
        Arc::clone(&fixture.service),
    ));
    let mut client = BufStream::new(client_stream);
    let (device_key, lease) = raw_connect(&mut client, &fixture.node_id, &fixture.security).await;
    success(
        round_trip(
            &mut client,
            with_lease(
                &fixture.node_id,
                PeerOperation::Observe {
                    device_key: device_key.clone(),
                    screenshot_omission: None,
                    ui_snapshots_enabled: false,
                    semantic_actions_enabled: false,
                },
                &lease,
            ),
        )
        .await,
    );
    assert_eq!(
        fixture
            .remote_evidence
            .referenced_sessions()
            .await
            .expect("remote Evidence reference")
            .len(),
        1
    );

    let owner = LeaseOwnerId::new(lease.lease_id);
    assert_eq!(
        fixture
            .registry
            .release_owner_leases(owner, now_ms())
            .await
            .len(),
        1
    );
    fixture
        .registry
        .unregister(fixture.driver.id(), now_ms())
        .await
        .expect("remove Core route after external lease release");

    success(
        round_trip(
            &mut client,
            with_lease(
                &fixture.node_id,
                PeerOperation::Disconnect {
                    device_key: device_key.clone(),
                },
                &lease,
            ),
        )
        .await,
    );
    assert!(
        fixture
            .events
            .list_sessions()
            .await
            .expect("Session cleanup after route removal")
            .is_empty()
    );
    assert!(
        fixture
            .remote_evidence
            .referenced_sessions()
            .await
            .expect("Evidence cleanup after route removal")
            .is_empty()
    );
    success(
        round_trip(
            &mut client,
            with_lease(
                &fixture.node_id,
                PeerOperation::LeaseRelease {
                    lease: lease.clone(),
                },
                &lease,
            ),
        )
        .await,
    );

    client.shutdown().await.expect("close client write half");
    drop(client);
    tokio::time::timeout(Duration::from_secs(2), server)
        .await
        .expect("server stopped after EOF")
        .expect("server task")
        .expect("clean peer shutdown");
}

#[tokio::test]
async fn real_peer_service_covers_evidence_replay_cancel_and_eof_cleanup() {
    let remote_root = tempfile::tempdir().expect("remote Evidence Store root");
    let remote_evidence = Arc::new(
        FileEvidenceStore::new(remote_root.path(), FileEvidenceStoreConfig::default())
            .expect("remote Evidence Store"),
    );
    let evidence: Arc<dyn EvidenceStore> = remote_evidence.clone();
    let events = Arc::new(MemoryEventStore::default());
    let registry = Arc::new(DriverRegistry::with_evidence(
        Arc::clone(&events),
        Arc::clone(&evidence),
    ));
    let driver = Arc::new(CountingDriver::new());
    let erased: Arc<dyn DeviceDriver> = driver.clone();
    registry
        .register(erased, driver.device_info())
        .await
        .expect("register node Driver");

    let node_id = NodeId::parse("node-b").expect("node id");
    let telemetry = Arc::new(MemoryTelemetry::default());
    let telemetry_sink: Arc<dyn TelemetrySink> = telemetry.clone();
    let service = RegistryPeerService::new_with_telemetry(
        node_id.clone(),
        NODE_EPOCH,
        1,
        registry,
        Arc::clone(&events),
        evidence,
        telemetry_sink,
    )
    .await
    .expect("Registry-backed peer service");
    let security = PeerSecurity::external_tunnel("client-a").expect("tunnel attestation");
    let (client_stream, server_stream) = tokio::io::duplex(256 * 1024);
    let server = tokio::spawn(serve_peer_stream(
        server_stream,
        security.clone(),
        Arc::clone(&service),
    ));
    let mut client = BufStream::new(client_stream);

    let hello = success(
        round_trip(
            &mut client,
            PeerRequest::new(node_id.clone(), None, PeerOperation::Hello),
        )
        .await,
    );
    assert!(matches!(
        hello,
        PeerResult::Hello {
            epoch: NODE_EPOCH,
            ..
        }
    ));
    let inventory = success(
        round_trip(
            &mut client,
            PeerRequest::new(node_id.clone(), None, PeerOperation::Inventory),
        )
        .await,
    );
    let PeerResult::Inventory { inventory } = inventory else {
        panic!("inventory result expected");
    };
    let device_key = inventory.devices[0].device_key.clone();

    let lease = success(
        round_trip(
            &mut client,
            PeerRequest::new(
                node_id.clone(),
                Some(NODE_EPOCH),
                PeerOperation::LeaseAcquire {
                    device_key: device_key.clone(),
                    owner_id: security.subject().to_owned(),
                    ttl_ms: 30_000,
                },
            ),
        )
        .await,
    );
    let PeerResult::Lease { lease } = lease else {
        panic!("lease result expected");
    };
    let connected = success(
        round_trip(
            &mut client,
            with_lease(
                &node_id,
                PeerOperation::Connect {
                    device_key: device_key.clone(),
                },
                &lease,
            ),
        )
        .await,
    );
    assert!(matches!(connected, PeerResult::Device { device } if device.connected));
    assert_eq!(
        events.list_sessions().await.expect("active Session").len(),
        1
    );

    let observed = success(
        round_trip(
            &mut client,
            with_lease(
                &node_id,
                PeerOperation::Observe {
                    device_key: device_key.clone(),
                    screenshot_omission: None,
                    ui_snapshots_enabled: false,
                    semantic_actions_enabled: false,
                },
                &lease,
            ),
        )
        .await,
    );
    let PeerResult::Observation { observation } = observed else {
        panic!("observation result expected");
    };
    let screenshot = observation
        .screenshot
        .expect("durable screenshot reference");
    let evidence_chunk = success(
        round_trip(
            &mut client,
            with_lease(
                &node_id,
                PeerOperation::EvidenceRead {
                    device_key: device_key.clone(),
                    evidence_id: screenshot.id.clone(),
                    offset: 0,
                    max_bytes: 256 * 1024,
                },
                &lease,
            ),
        )
        .await,
    );
    let PeerResult::EvidenceChunk {
        data_base64,
        done,
        sha256,
        ..
    } = evidence_chunk
    else {
        panic!("evidence chunk expected");
    };
    let evidence_bytes = BASE64.decode(data_base64).expect("base64 evidence");
    assert!(done);
    assert!(sha256.is_some());
    assert_eq!(&evidence_bytes[..8], b"\x89PNG\r\n\x1a\n");

    let call = ActionCall {
        id: Uuid::new_v4(),
        name: "tap".to_owned(),
        arguments: json!({"x": 1, "y": 2}),
    };
    let operation = PeerOperation::Execute {
        device_key: device_key.clone(),
        call: call.clone(),
        screenshot_omission: None,
        ui_snapshots_enabled: false,
        semantic_actions_enabled: false,
    };
    let first =
        success(round_trip(&mut client, with_lease(&node_id, operation.clone(), &lease)).await);
    let replay = success(round_trip(&mut client, with_lease(&node_id, operation, &lease)).await);
    assert_eq!(first, replay);
    assert_eq!(driver.execute_calls.load(Ordering::SeqCst), 1);

    let cancelled_call = ActionCall {
        id: Uuid::new_v4(),
        name: "tap".to_owned(),
        arguments: json!({"x": 3, "y": 4}),
    };
    let execute_request = with_lease(
        &node_id,
        PeerOperation::Execute {
            device_key,
            call: cancelled_call.clone(),
            screenshot_omission: None,
            ui_snapshots_enabled: false,
            semantic_actions_enabled: false,
        },
        &lease,
    );
    write_request(&mut client, &execute_request).await;
    tokio::time::timeout(Duration::from_secs(1), async {
        while driver.execute_calls.load(Ordering::SeqCst) < 2 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("execute reached node Driver");
    let cancel_request = PeerRequest::new(
        node_id.clone(),
        Some(NODE_EPOCH),
        PeerOperation::Cancel {
            target_request_id: execute_request.request_id,
            call_id: Some(cancelled_call.id),
        },
    );
    write_request(&mut client, &cancel_request).await;
    let responses = [
        read_response(&mut client).await,
        read_response(&mut client).await,
    ];
    let cancel_response = responses
        .iter()
        .find(|response| response.request_id == cancel_request.request_id)
        .expect("cancel response");
    cancel_response
        .validate_for(&cancel_request)
        .expect("valid cancel response");
    assert!(cancel_response.ok);
    let execute_response = responses
        .iter()
        .find(|response| response.request_id == execute_request.request_id)
        .expect("cancelled execute response");
    execute_response
        .validate_for(&execute_request)
        .expect("valid cancelled execute response");
    assert_eq!(
        execute_response
            .error
            .as_ref()
            .map(|error| error.code.as_str()),
        Some("request_cancelled")
    );
    assert_eq!(service.active_request_count(), 0);

    client.shutdown().await.expect("close client write half");
    drop(client);
    tokio::time::timeout(Duration::from_secs(2), server)
        .await
        .expect("server stopped after EOF")
        .expect("server task")
        .expect("clean peer shutdown");
    assert!(
        events
            .list_sessions()
            .await
            .expect("cleaned Sessions")
            .is_empty()
    );
    assert!(
        remote_evidence
            .referenced_sessions()
            .await
            .expect("cleaned evidence references")
            .is_empty()
    );
    assert_eq!(driver.disconnect_calls.load(Ordering::SeqCst), 1);
    assert_eq!(
        telemetry.count(
            node_id.as_str(),
            OperationMethod::Execute,
            OperationOutcome::Success,
        ),
        2
    );
    assert_eq!(
        telemetry.count(
            node_id.as_str(),
            OperationMethod::Execute,
            OperationOutcome::Cancelled,
        ),
        1
    );
}

#[tokio::test]
async fn failed_cleanup_remains_retryable_and_blocks_new_device_operations() {
    let fixture = service_fixture(
        CountingDriver::new().with_disconnect_failures(1),
        "node-cleanup",
    )
    .await;
    let (client_stream, server_stream) = tokio::io::duplex(256 * 1024);
    let server = tokio::spawn(serve_peer_stream(
        server_stream,
        fixture.security.clone(),
        Arc::clone(&fixture.service),
    ));
    let mut client = BufStream::new(client_stream);
    let (device_key, lease) = raw_connect(&mut client, &fixture.node_id, &fixture.security).await;

    success(
        round_trip(
            &mut client,
            with_lease(
                &fixture.node_id,
                PeerOperation::Observe {
                    device_key: device_key.clone(),
                    screenshot_omission: None,
                    ui_snapshots_enabled: false,
                    semantic_actions_enabled: false,
                },
                &lease,
            ),
        )
        .await,
    );
    assert_eq!(
        fixture
            .remote_evidence
            .referenced_sessions()
            .await
            .expect("remote Evidence references")
            .len(),
        1
    );

    let first_cleanup = round_trip(
        &mut client,
        with_lease(
            &fixture.node_id,
            PeerOperation::Disconnect {
                device_key: device_key.clone(),
            },
            &lease,
        ),
    )
    .await;
    assert!(!first_cleanup.ok);
    assert!(first_cleanup.error.expect("cleanup error").retryable);
    assert_eq!(
        fixture
            .events
            .list_sessions()
            .await
            .expect("retained Session")
            .len(),
        1
    );
    assert_eq!(
        fixture
            .remote_evidence
            .referenced_sessions()
            .await
            .expect("retained Evidence references")
            .len(),
        1
    );
    let retained_session = fixture
        .events
        .list_sessions()
        .await
        .expect("retained Session")
        .into_iter()
        .next()
        .expect("one retained Session");
    fixture
        .events
        .end_session(EndSession {
            session_id: retained_session.id,
            request_id: None,
            device_id: Some(fixture.driver.id().clone()),
            at_ms: now_ms(),
            outcome: SessionOutcome::Completed,
            reason: None,
        })
        .await
        .expect("external Session end commits before cleanup retry");

    let blocked = round_trip(
        &mut client,
        with_lease(
            &fixture.node_id,
            PeerOperation::Observe {
                device_key: device_key.clone(),
                screenshot_omission: None,
                ui_snapshots_enabled: false,
                semantic_actions_enabled: false,
            },
            &lease,
        ),
    )
    .await;
    assert_eq!(
        blocked.error.as_ref().map(|error| error.code.as_str()),
        Some("lease_cleanup_pending")
    );

    success(
        round_trip(
            &mut client,
            with_lease(
                &fixture.node_id,
                PeerOperation::Disconnect {
                    device_key: device_key.clone(),
                },
                &lease,
            ),
        )
        .await,
    );
    assert!(
        fixture
            .events
            .list_sessions()
            .await
            .expect("cleaned Sessions")
            .is_empty()
    );
    assert!(
        fixture
            .remote_evidence
            .referenced_sessions()
            .await
            .expect("cleaned Evidence references")
            .is_empty()
    );
    assert_eq!(fixture.driver.disconnect_calls.load(Ordering::SeqCst), 2);

    success(
        round_trip(
            &mut client,
            with_lease(
                &fixture.node_id,
                PeerOperation::LeaseRelease {
                    lease: lease.clone(),
                },
                &lease,
            ),
        )
        .await,
    );
    client.shutdown().await.expect("close client write half");
    drop(client);
    tokio::time::timeout(Duration::from_secs(2), server)
        .await
        .expect("server stopped after EOF")
        .expect("server task")
        .expect("clean peer shutdown");
}

#[tokio::test]
async fn eof_cleanup_retries_transient_failure_without_a_live_client() {
    let fixture = service_fixture(
        CountingDriver::new().with_disconnect_failures(1),
        "node-eof-cleanup-retry",
    )
    .await;
    let (client_stream, server_stream) = tokio::io::duplex(256 * 1024);
    let server = tokio::spawn(serve_peer_stream(
        server_stream,
        fixture.security.clone(),
        Arc::clone(&fixture.service),
    ));
    let mut client = BufStream::new(client_stream);
    let (device_key, lease) = raw_connect(&mut client, &fixture.node_id, &fixture.security).await;
    success(
        round_trip(
            &mut client,
            with_lease(
                &fixture.node_id,
                PeerOperation::Observe {
                    device_key,
                    screenshot_omission: None,
                    ui_snapshots_enabled: false,
                    semantic_actions_enabled: false,
                },
                &lease,
            ),
        )
        .await,
    );

    client.shutdown().await.expect("close client write half");
    drop(client);
    tokio::time::timeout(Duration::from_secs(2), server)
        .await
        .expect("server stopped after service-owned cleanup retry")
        .expect("server task")
        .expect("transient EOF cleanup converges");

    assert_eq!(fixture.driver.disconnect_calls.load(Ordering::SeqCst), 2);
    assert!(
        fixture
            .events
            .list_sessions()
            .await
            .expect("cleaned Session")
            .is_empty()
    );
    assert!(
        fixture
            .remote_evidence
            .referenced_sessions()
            .await
            .expect("cleaned Evidence references")
            .is_empty()
    );
}

#[tokio::test]
async fn disconnect_waits_for_an_admitted_execute_through_terminal_caching() {
    let fixture = service_fixture(CountingDriver::new(), "node-race").await;
    let (client_stream, server_stream) = tokio::io::duplex(256 * 1024);
    let server = tokio::spawn(serve_peer_stream(
        server_stream,
        fixture.security.clone(),
        Arc::clone(&fixture.service),
    ));
    let mut client = BufStream::new(client_stream);
    let (device_key, lease) = raw_connect(&mut client, &fixture.node_id, &fixture.security).await;
    let call = ActionCall {
        id: Uuid::new_v4(),
        name: "tap".to_owned(),
        arguments: json!({"x": 4, "y": 5}),
    };
    let execute = with_lease(
        &fixture.node_id,
        PeerOperation::Execute {
            device_key: device_key.clone(),
            call,
            screenshot_omission: None,
            ui_snapshots_enabled: false,
            semantic_actions_enabled: false,
        },
        &lease,
    );
    write_request(&mut client, &execute).await;
    tokio::time::timeout(Duration::from_secs(1), async {
        while fixture.driver.execute_calls.load(Ordering::SeqCst) == 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("execute reached the Driver");
    let disconnect = with_lease(
        &fixture.node_id,
        PeerOperation::Disconnect {
            device_key: device_key.clone(),
        },
        &lease,
    );
    write_request(&mut client, &disconnect).await;
    let responses = [
        read_response(&mut client).await,
        read_response(&mut client).await,
    ];
    for request in [&execute, &disconnect] {
        let response = responses
            .iter()
            .find(|response| response.request_id == request.request_id)
            .expect("correlated response");
        response.validate_for(request).expect("valid response");
        assert!(response.ok, "operation failed: {:?}", response.error);
    }
    assert_eq!(fixture.driver.execute_calls.load(Ordering::SeqCst), 1);
    assert_eq!(fixture.driver.disconnect_calls.load(Ordering::SeqCst), 1);

    success(
        round_trip(
            &mut client,
            with_lease(
                &fixture.node_id,
                PeerOperation::LeaseRelease {
                    lease: lease.clone(),
                },
                &lease,
            ),
        )
        .await,
    );
    client.shutdown().await.expect("close client write half");
    drop(client);
    tokio::time::timeout(Duration::from_secs(2), server)
        .await
        .expect("server stopped after EOF")
        .expect("server task")
        .expect("clean peer shutdown");
}

#[tokio::test]
async fn remote_node_driver_imports_evidence_into_the_local_runtime_store() {
    let fixture = service_fixture(CountingDriver::new(), "node-client").await;
    let (client_stream, server_stream) = tokio::io::duplex(256 * 1024);
    let server = tokio::spawn(serve_peer_stream(
        server_stream,
        fixture.security.clone(),
        Arc::clone(&fixture.service),
    ));
    let transport: Arc<dyn PeerTransport> = NdjsonPeerTransport::new(
        client_stream,
        fixture.node_id.clone(),
        fixture.security.clone(),
    );
    let control = ExecutionControl::unbounded();
    let node = RemoteNode::discover(
        Arc::clone(&transport),
        None,
        RouterConfig::default(),
        &control,
    )
    .await
    .expect("discover remote node through the real transport");
    let mut drivers = node
        .drivers(
            fixture.security.subject(),
            RemoteDriverConfig::default(),
            &control,
        )
        .await
        .expect("load remote Drivers");
    let driver = Arc::new(drivers.remove(0));

    let local_root = tempfile::tempdir().expect("local Evidence Store root");
    let local_evidence = Arc::new(
        FileEvidenceStore::new(local_root.path(), FileEvidenceStoreConfig::default())
            .expect("local Evidence Store"),
    );
    let local_store: Arc<dyn EvidenceStore> = local_evidence.clone();
    let local_events = Arc::new(MemoryEventStore::default());
    let runtime =
        DeviceRuntime::with_evidence(Arc::clone(&driver), Arc::clone(&local_events), local_store);
    let connected = runtime.connect(&control).await.expect("remote connect");
    assert!(connected.id.0.starts_with("remote:node-client:"));
    let session = local_events
        .start_session(StartSession::new(None, Some(driver.id().clone()), now_ms()))
        .await
        .expect("local Session");
    let context = OperationContext::new(session.id.clone(), None);
    let observation = runtime.observe(&context).await.expect("remote observe");
    assert_eq!(observation.device_id, *driver.id());
    let screenshot = observation.screenshot.expect("imported local screenshot");
    let digest = Sha256Digest::parse(
        screenshot
            .id
            .strip_prefix("sha256:")
            .expect("content-addressed local Evidence")
            .to_owned(),
    )
    .expect("local Evidence digest");
    let mut body = local_evidence
        .open(&digest)
        .await
        .expect("open local Evidence");
    let mut bytes = Vec::new();
    body.read_to_end(&mut bytes)
        .await
        .expect("read imported local Evidence");
    assert_eq!(&bytes[..8], b"\x89PNG\r\n\x1a\n");
    assert_eq!(
        local_evidence
            .referenced_sessions()
            .await
            .expect("local Evidence references"),
        vec![session.id.clone()]
    );
    assert_eq!(
        fixture
            .remote_evidence
            .referenced_sessions()
            .await
            .expect("remote Evidence references")
            .len(),
        1
    );

    let call_id = Uuid::new_v4();
    let result = runtime
        .execute(
            &context,
            ActionCall {
                id: call_id,
                name: "tap".to_owned(),
                arguments: json!({"x": 2, "y": 3}),
            },
        )
        .await
        .expect("remote execute");
    assert_eq!(result.call_id, call_id);
    assert!(!result.evidence.is_empty());
    assert_eq!(fixture.driver.execute_calls.load(Ordering::SeqCst), 1);

    runtime
        .disconnect(&control)
        .await
        .expect("remote disconnect");
    assert!(
        fixture
            .events
            .list_sessions()
            .await
            .expect("remote Sessions cleaned")
            .is_empty()
    );
    assert!(
        fixture
            .remote_evidence
            .referenced_sessions()
            .await
            .expect("remote Evidence cleaned")
            .is_empty()
    );

    local_events
        .end_session(EndSession {
            session_id: session.id.clone(),
            request_id: None,
            device_id: Some(driver.id().clone()),
            at_ms: now_ms(),
            outcome: SessionOutcome::Completed,
            reason: None,
        })
        .await
        .expect("end local Session");
    cleanup_ended_session(
        local_events.as_ref(),
        local_evidence.as_ref(),
        &session.id,
        now_ms(),
    )
    .await
    .expect("cleanup local Session Evidence");
    assert!(
        local_evidence
            .referenced_sessions()
            .await
            .expect("local Evidence cleaned")
            .is_empty()
    );

    drop(runtime);
    drop(driver);
    drop(node);
    drop(transport);
    tokio::time::timeout(Duration::from_secs(2), server)
        .await
        .expect("server stopped after client transport drop")
        .expect("server task")
        .expect("clean peer shutdown");
}

#[tokio::test]
async fn remote_driver_cancel_poisons_transport_and_eof_cleans_remote_state() {
    let fixture = service_fixture(CountingDriver::new(), "node-cancel").await;
    let (client_stream, server_stream) = tokio::io::duplex(256 * 1024);
    let server = tokio::spawn(serve_peer_stream(
        server_stream,
        fixture.security.clone(),
        Arc::clone(&fixture.service),
    ));
    let concrete_transport = NdjsonPeerTransport::new(
        client_stream,
        fixture.node_id.clone(),
        fixture.security.clone(),
    );
    let transport: Arc<dyn PeerTransport> = concrete_transport.clone();
    let setup_control = ExecutionControl::unbounded();
    let node = RemoteNode::discover(
        Arc::clone(&transport),
        None,
        RouterConfig::default(),
        &setup_control,
    )
    .await
    .expect("discover remote node");
    let mut drivers = node
        .drivers(
            fixture.security.subject(),
            RemoteDriverConfig::default(),
            &setup_control,
        )
        .await
        .expect("load remote Drivers");
    let driver = Arc::new(drivers.remove(0));
    let local_root = tempfile::tempdir().expect("local Evidence Store root");
    let local_evidence = Arc::new(
        FileEvidenceStore::new(local_root.path(), FileEvidenceStoreConfig::default())
            .expect("local Evidence Store"),
    );
    let local_store: Arc<dyn EvidenceStore> = local_evidence.clone();
    let local_events = Arc::new(MemoryEventStore::default());
    let runtime = Arc::new(DeviceRuntime::with_evidence(
        Arc::clone(&driver),
        Arc::clone(&local_events),
        local_store,
    ));
    runtime
        .connect(&setup_control)
        .await
        .expect("remote connect");
    let session = local_events
        .start_session(StartSession::new(None, Some(driver.id().clone()), now_ms()))
        .await
        .expect("local Session");
    let (controller, operation_control) =
        ExecutionController::with_timeout(5_000, TimeoutScope::Request);
    let execute = tokio::spawn({
        let runtime = Arc::clone(&runtime);
        let context =
            OperationContext::new(session.id.clone(), None).with_control(operation_control);
        async move {
            runtime
                .execute(
                    &context,
                    ActionCall {
                        id: Uuid::new_v4(),
                        name: "tap".to_owned(),
                        arguments: json!({"x": 7, "y": 8}),
                    },
                )
                .await
        }
    });
    tokio::time::timeout(Duration::from_secs(1), async {
        while fixture.driver.execute_calls.load(Ordering::SeqCst) == 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("execute reached the remote Driver");
    assert!(controller.cancel(CancellationReason::Requested));
    let error = execute
        .await
        .expect("execute task")
        .expect_err("post-send cancellation is outcome-unknown");
    let public = error.to_error_info();
    assert_eq!(
        public.details.expect("platform details")["platformCode"],
        "remote_execute_outcome_unknown"
    );
    assert!(!concrete_transport.is_open().await);
    let poison = runtime
        .health_check(&ExecutionControl::unbounded())
        .await
        .expect_err("poisoned transport rejects later work");
    assert_eq!(
        poison.to_error_info().details.expect("platform details")["platformCode"],
        "peer_closed_before_send"
    );

    let _server_result = tokio::time::timeout(Duration::from_secs(2), server)
        .await
        .expect("server stopped after poisoned transport EOF")
        .expect("server task");
    assert!(
        fixture
            .events
            .list_sessions()
            .await
            .expect("remote Sessions cleaned")
            .is_empty()
    );
    assert!(
        fixture
            .remote_evidence
            .referenced_sessions()
            .await
            .expect("remote Evidence cleaned")
            .is_empty()
    );
    assert_eq!(fixture.driver.disconnect_calls.load(Ordering::SeqCst), 1);

    local_events
        .end_session(EndSession {
            session_id: session.id.clone(),
            request_id: None,
            device_id: Some(driver.id().clone()),
            at_ms: now_ms(),
            outcome: SessionOutcome::Failed,
            reason: Some("cancelled remote outcome".to_owned()),
        })
        .await
        .expect("end local Session");
    cleanup_ended_session(
        local_events.as_ref(),
        local_evidence.as_ref(),
        &session.id,
        now_ms(),
    )
    .await
    .expect("cleanup local Session");
    assert!(
        local_evidence
            .referenced_sessions()
            .await
            .expect("local Evidence cleaned")
            .is_empty()
    );
}
