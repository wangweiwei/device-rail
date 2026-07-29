use std::{
    collections::{BTreeMap, BTreeSet, VecDeque, btree_map::Entry},
    sync::{
        Arc, Mutex as StdMutex,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
use devicerail_core::{
    CancellationReason, DeviceLease, DevicePoolError, DriverRegistry, EndSession, EventStoreError,
    EvidenceOutput, EvidenceStore, ExecutionControl, ExecutionController, LeaseOwnerId,
    MemoryEventStore, OperationContext, PoolHealth, RegistryError, RuntimeError, ScreenshotPolicy,
    SessionEventStore, Sha256Digest, StartSession, TimeoutScope, cleanup_ended_session, now_ms,
};
use devicerail_protocol::{
    ActionProtection, ActionResult, DeviceId, DeviceInfo, Observation, ScreenshotOmissionReason,
    SessionOutcome,
};
use futures_util::future::join_all;
use sha2::{Digest as _, Sha256};
use thiserror::Error;
use tokio::{
    io::AsyncReadExt as _,
    sync::{Mutex, OwnedMutexGuard, Semaphore, oneshot},
};
use uuid::Uuid;

use crate::{
    CallLedger, CallLedgerDecision, DISTRIBUTED_PROTOCOL_VERSION, HealthState, InventorySnapshot,
    LeaseTable, MemoryTelemetry, NodeId, OperationMethod, OperationOutcome, PeerError, PeerLease,
    PeerOperation, PeerRequest, PeerResponse, PeerResult, PeerSecurity, RemoteDeviceDescriptor,
    TelemetrySink,
    model::{MAX_SAFE_INTEGER, valid_code},
    telemetry,
};

const MAX_SERVICE_DEVICES: usize = 256;
const MAX_EVIDENCE_BYTES: u64 = 16 * 1024 * 1024;
const MAX_TERMINAL_CALLS: usize = 1_024;
const MAX_TERMINAL_BYTES: usize = 16 * 1024 * 1024;
const MAX_ONE_TERMINAL_BYTES: usize = 256 * 1024;
const MAX_OPERATION_TIMEOUT_MS: u64 = 5 * 60_000;
const CLEANUP_TIMEOUT_MS: u64 = 5_000;
const MAX_CONNECTION_LEASES: usize = 16;
const MAX_EVIDENCE_CURSORS_PER_BINDING: usize = 4;
const MAX_BACKGROUND_CONNECTION_CLEANUPS: usize = 64;
const MAX_CONNECTION_CLEANUP_ATTEMPTS: usize = 3;
const CONNECTION_CLEANUP_ATTEMPT_GRACE: Duration = Duration::from_millis(5_250);
const CONNECTION_CLEANUP_RETRY_DELAY: Duration = Duration::from_millis(25);

#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum PeerServiceError {
    #[error("peer service configuration is invalid")]
    InvalidConfiguration,
    #[error("peer service inventory is empty, oversized, or unstable")]
    InvalidInventory,
    #[error("peer request violates the service protocol")]
    Protocol,
}

#[derive(Clone)]
struct Binding {
    connection_id: Uuid,
    peer_lease: PeerLease,
    device_id: DeviceId,
    // The reservation is published before Core lease acquisition. Once the
    // acquisition future returns, the lease is written synchronously so task
    // cancellation cannot leave an owned lease absent from cleanup state.
    core_lease: Arc<StdMutex<Option<DeviceLease>>>,
    session_start: StartSession,
    session_started: Arc<AtomicBool>,
    allowed_evidence: BTreeSet<String>,
    evidence_cursors: Arc<Mutex<EvidenceCursorSet>>,
    cleanup: Arc<StdMutex<CleanupProgress>>,
}

struct EvidenceReadCursor {
    digest: Sha256Digest,
    media_type: String,
    total_size: u64,
    offset: u64,
    body: EvidenceOutput,
}

#[derive(Default)]
struct EvidenceCursorSet {
    order: VecDeque<String>,
    values: BTreeMap<String, EvidenceReadCursor>,
}

impl EvidenceCursorSet {
    fn take(&mut self, evidence_id: &str) -> Option<EvidenceReadCursor> {
        self.order.retain(|candidate| candidate != evidence_id);
        self.values.remove(evidence_id)
    }

    fn insert(&mut self, evidence_id: String, cursor: EvidenceReadCursor) {
        self.take(&evidence_id);
        while self.values.len() >= MAX_EVIDENCE_CURSORS_PER_BINDING {
            let Some(evicted) = self.order.pop_front() else {
                break;
            };
            self.values.remove(&evicted);
        }
        self.order.push_back(evidence_id.clone());
        self.values.insert(evidence_id, cursor);
    }
}

impl Binding {
    fn core_lease(&self) -> Option<DeviceLease> {
        self.core_lease
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    fn publish_core_lease(&self, lease: DeviceLease) {
        *self
            .core_lease
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(lease);
    }
}

#[derive(Clone)]
struct IssuedLease {
    connection_id: Uuid,
    lease: PeerLease,
    gate: Arc<Mutex<()>>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct CleanupProgress {
    started: bool,
    disconnected: bool,
    session_ended: bool,
    evidence_cleaned: bool,
    core_lease_released: bool,
}

struct ExecuteInput<'a> {
    fingerprint: Vec<u8>,
    device_key: &'a str,
    lease: &'a PeerLease,
    connection_id: Uuid,
    requested_omission: Option<ScreenshotOmissionReason>,
    ui_snapshots_enabled: bool,
    semantic_actions_enabled: bool,
    call: devicerail_protocol::ActionCall,
}

struct ObserveInput<'a> {
    device_key: &'a str,
    lease: &'a PeerLease,
    connection_id: Uuid,
    requested_omission: Option<ScreenshotOmissionReason>,
    ui_snapshots_enabled: bool,
    semantic_actions_enabled: bool,
}

#[derive(Clone)]
enum CachedTerminal {
    Success(PeerResult),
    Failure(PeerError),
}

#[derive(Clone)]
struct CachedEntry {
    lease_id: Uuid,
    terminal: CachedTerminal,
    bytes: usize,
}

#[derive(Default)]
struct TerminalCache {
    order: VecDeque<Uuid>,
    values: BTreeMap<Uuid, CachedEntry>,
    bytes: usize,
}

impl TerminalCache {
    fn get(&self, call_id: &Uuid, lease_id: Uuid) -> Option<CachedTerminal> {
        self.values
            .get(call_id)
            .filter(|entry| entry.lease_id == lease_id)
            .map(|entry| entry.terminal.clone())
    }

    fn insert(&mut self, call_id: Uuid, lease_id: Uuid, terminal: CachedTerminal) {
        let bytes = terminal_bytes(&terminal);
        if bytes > MAX_ONE_TERMINAL_BYTES {
            return;
        }
        self.remove(&call_id);
        while self.order.len() >= MAX_TERMINAL_CALLS
            || self.bytes.saturating_add(bytes) > MAX_TERMINAL_BYTES
        {
            let Some(evicted) = self.order.pop_front() else {
                break;
            };
            if let Some(entry) = self.values.remove(&evicted) {
                self.bytes = self.bytes.saturating_sub(entry.bytes);
            }
        }
        self.order.push_back(call_id);
        self.bytes += bytes;
        self.values.insert(
            call_id,
            CachedEntry {
                lease_id,
                terminal,
                bytes,
            },
        );
    }

    fn remove(&mut self, call_id: &Uuid) {
        if let Some(entry) = self.values.remove(call_id) {
            self.bytes = self.bytes.saturating_sub(entry.bytes);
            self.order.retain(|candidate| candidate != call_id);
        }
    }

    fn remove_lease(&mut self, lease_id: Uuid) {
        let call_ids = self
            .values
            .iter()
            .filter(|(_, entry)| entry.lease_id == lease_id)
            .map(|(call_id, _)| *call_id)
            .collect::<Vec<_>>();
        for call_id in call_ids {
            self.remove(&call_id);
        }
    }
}

struct ActiveRequestGuard {
    key: (Uuid, Uuid),
    active: Arc<StdMutex<BTreeMap<(Uuid, Uuid), ExecutionController>>>,
}

struct InFlightCallGuard {
    call_id: Uuid,
    calls: Arc<StdMutex<BTreeSet<Uuid>>>,
}

impl Drop for InFlightCallGuard {
    fn drop(&mut self) {
        self.calls
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&self.call_id);
    }
}

impl Drop for ActiveRequestGuard {
    fn drop(&mut self) {
        self.active
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&self.key);
    }
}

/// A node-side implementation of peer-v2 backed by the real Core Registry,
/// Device Pool, Session Event Store, and Evidence Store.
///
/// The service owns no listener. Call [`crate::serve_peer_stream`] only after
/// authenticating a loopback connection with `devicerail-remote-auth` or
/// accepting it from an independently authenticated SSH/mTLS tunnel.
pub struct RegistryPeerService<S = MemoryEventStore>
where
    S: SessionEventStore + ?Sized + 'static,
{
    node_id: NodeId,
    epoch: u64,
    revision: u64,
    registry: Arc<DriverRegistry<S>>,
    events: Arc<S>,
    evidence: Arc<dyn EvidenceStore>,
    telemetry: Arc<dyn TelemetrySink>,
    inventory: Vec<RemoteDeviceDescriptor>,
    devices: BTreeMap<String, DeviceId>,
    ready: AtomicBool,
    leases: LeaseTable,
    issued_leases: StdMutex<BTreeMap<Uuid, IssuedLease>>,
    bindings: Mutex<BTreeMap<Uuid, Binding>>,
    calls: CallLedger,
    terminals: StdMutex<TerminalCache>,
    in_flight_calls: Arc<StdMutex<BTreeSet<Uuid>>>,
    active: Arc<StdMutex<BTreeMap<(Uuid, Uuid), ExecutionController>>>,
    cleanup_admission: Arc<Semaphore>,
}

impl<S> std::fmt::Debug for RegistryPeerService<S>
where
    S: SessionEventStore + ?Sized,
{
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("RegistryPeerService")
            .field("node_id", &self.node_id)
            .field("epoch", &self.epoch)
            .field("revision", &self.revision)
            .field("device_count", &self.devices.len())
            .field("ready", &self.is_ready())
            .field("active_request_count", &self.active_request_count())
            .finish_non_exhaustive()
    }
}

impl<S> RegistryPeerService<S>
where
    S: SessionEventStore + ?Sized + 'static,
{
    pub async fn new(
        node_id: NodeId,
        epoch: u64,
        revision: u64,
        registry: Arc<DriverRegistry<S>>,
        events: Arc<S>,
        evidence: Arc<dyn EvidenceStore>,
    ) -> Result<Arc<Self>, PeerServiceError> {
        Self::new_with_telemetry(
            node_id,
            epoch,
            revision,
            registry,
            events,
            evidence,
            Arc::new(MemoryTelemetry::default()),
        )
        .await
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn new_with_telemetry(
        node_id: NodeId,
        epoch: u64,
        revision: u64,
        registry: Arc<DriverRegistry<S>>,
        events: Arc<S>,
        evidence: Arc<dyn EvidenceStore>,
        telemetry: Arc<dyn TelemetrySink>,
    ) -> Result<Arc<Self>, PeerServiceError> {
        if epoch == 0 || epoch > MAX_SAFE_INTEGER || revision == 0 || revision > MAX_SAFE_INTEGER {
            return Err(PeerServiceError::InvalidConfiguration);
        }
        let infos = registry
            .list()
            .await
            .into_iter()
            .filter(|info| !info.id.0.starts_with("remote:"))
            .collect::<Vec<_>>();
        if infos.is_empty() || infos.len() > MAX_SERVICE_DEVICES {
            return Err(PeerServiceError::InvalidInventory);
        }
        let mut devices = BTreeMap::new();
        let mut inventory = Vec::with_capacity(infos.len());
        for info in infos {
            let key = stable_device_key(&info.id);
            if devices.insert(key.clone(), info.id.clone()).is_some() {
                return Err(PeerServiceError::InvalidInventory);
            }
            let descriptor = RemoteDeviceDescriptor {
                device_key: key,
                name: info.name,
                platform: info.platform,
                os_version: info.os_version,
            };
            descriptor
                .validate()
                .map_err(|_| PeerServiceError::InvalidInventory)?;
            inventory.push(descriptor);
        }
        inventory.sort_by(|left, right| left.device_key.cmp(&right.device_key));
        let leases = LeaseTable::new(epoch, devices.keys().cloned())
            .map_err(|_| PeerServiceError::InvalidConfiguration)?;
        Ok(Arc::new(Self {
            node_id,
            epoch,
            revision,
            registry,
            events,
            evidence,
            telemetry,
            inventory,
            devices,
            ready: AtomicBool::new(true),
            leases,
            issued_leases: StdMutex::new(BTreeMap::new()),
            bindings: Mutex::new(BTreeMap::new()),
            calls: CallLedger::default(),
            terminals: StdMutex::new(TerminalCache::default()),
            in_flight_calls: Arc::new(StdMutex::new(BTreeSet::new())),
            active: Arc::new(StdMutex::new(BTreeMap::new())),
            cleanup_admission: Arc::new(Semaphore::new(MAX_BACKGROUND_CONNECTION_CLEANUPS)),
        }))
    }

    pub fn node_id(&self) -> &NodeId {
        &self.node_id
    }

    pub fn epoch(&self) -> u64 {
        self.epoch
    }

    /// Temporarily exposes only discovery-safe, read-only peer operations.
    /// Call this before publishing a listener while mandatory outbound peers
    /// are still being discovered, then call [`Self::mark_ready`].
    pub fn mark_starting(&self) {
        self.ready.store(false, Ordering::Release);
    }

    pub fn mark_ready(&self) {
        self.ready.store(true, Ordering::Release);
    }

    pub fn is_ready(&self) -> bool {
        self.ready.load(Ordering::Acquire)
    }

    pub fn active_request_count(&self) -> usize {
        self.active
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .len()
    }

    pub async fn handle(
        &self,
        request: PeerRequest,
        security: &PeerSecurity,
        connection_id: Uuid,
    ) -> Result<PeerResponse, PeerServiceError> {
        let started = Instant::now();
        let trace_id = request.trace_id;
        let method = operation_method(&request.operation);
        let response = self.handle_inner(request, security, connection_id).await;
        let outcome = match &response {
            Ok(response) if response.ok => OperationOutcome::Success,
            Ok(response)
                if response
                    .error
                    .as_ref()
                    .is_some_and(|error| error.outcome_unknown) =>
            {
                OperationOutcome::OutcomeUnknown
            }
            Ok(response)
                if response.error.as_ref().is_some_and(|error| {
                    matches!(
                        error.code.as_str(),
                        "request_cancelled" | "action_cancelled"
                    )
                }) =>
            {
                OperationOutcome::Cancelled
            }
            Ok(response)
                if response.error.as_ref().is_some_and(|error| {
                    matches!(error.code.as_str(), "request_timed_out" | "action_timeout")
                }) =>
            {
                OperationOutcome::TimedOut
            }
            Ok(_) => OperationOutcome::RemoteError,
            Err(PeerServiceError::Protocol) => OperationOutcome::ProtocolError,
            Err(_) => OperationOutcome::RemoteError,
        };
        telemetry::record(
            self.telemetry.as_ref(),
            trace_id,
            &self.node_id,
            method,
            outcome,
            started,
        );
        response
    }

    async fn handle_inner(
        &self,
        request: PeerRequest,
        security: &PeerSecurity,
        connection_id: Uuid,
    ) -> Result<PeerResponse, PeerServiceError> {
        request.validate().map_err(|_| PeerServiceError::Protocol)?;
        if request.protocol_version != DISTRIBUTED_PROTOCOL_VERSION
            || request.node_id != self.node_id
        {
            return Err(PeerServiceError::Protocol);
        }
        if !matches!(
            request.operation,
            PeerOperation::Hello | PeerOperation::Inventory
        ) && request.node_epoch != Some(self.epoch)
        {
            return Ok(PeerResponse::failure(
                &request,
                self.epoch,
                peer_error("node_epoch_stale", false, false),
            ));
        }
        if let PeerOperation::Cancel {
            target_request_id, ..
        } = request.operation
        {
            let cancelled = self
                .active
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .get(&(connection_id, target_request_id))
                .is_some_and(|controller| controller.cancel(CancellationReason::Requested));
            let response = if cancelled {
                PeerResponse::success(&request, self.epoch, PeerResult::Ack)
            } else {
                PeerResponse::failure(
                    &request,
                    self.epoch,
                    peer_error("cancel_target_not_found", false, false),
                )
            };
            return Ok(response);
        }

        let (controller, control) = match operation_control(request.timeout_ms) {
            Ok(control) => control,
            Err(_) => {
                return Ok(PeerResponse::failure(
                    &request,
                    self.epoch,
                    peer_error("request_deadline_elapsed", false, false),
                ));
            }
        };
        let guard = {
            let mut active = self
                .active
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let key = (connection_id, request.request_id);
            match active.entry(key) {
                Entry::Vacant(entry) => {
                    entry.insert(controller);
                }
                Entry::Occupied(_) => {
                    return Ok(PeerResponse::failure(
                        &request,
                        self.epoch,
                        peer_error("duplicate_request_id", false, false),
                    ));
                }
            }
            ActiveRequestGuard {
                key,
                active: Arc::clone(&self.active),
            }
        };
        let terminal = self
            .dispatch(&request, security.subject(), connection_id, &control)
            .await;
        drop(guard);
        Ok(match terminal {
            Ok(result) => PeerResponse::success(&request, self.epoch, result),
            Err(error) => PeerResponse::failure(&request, self.epoch, error),
        })
    }

    pub fn cancel_all(&self, reason: CancellationReason) {
        for controller in self
            .active
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .values()
        {
            controller.cancel(reason);
        }
    }

    pub fn cancel_request(
        &self,
        connection_id: Uuid,
        request_id: Uuid,
        reason: CancellationReason,
    ) -> bool {
        self.active
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&(connection_id, request_id))
            .is_some_and(|controller| controller.cancel(reason))
    }

    /// Best-effort service teardown. It first cancels admitted requests, then
    /// disconnects bound devices, ends their local Sessions, and releases Core
    /// and peer leases. Returned errors contain only stable classifications.
    pub async fn shutdown(&self) -> Vec<PeerError> {
        self.mark_starting();
        self.cancel_all(CancellationReason::Shutdown);
        // Stream teardown admits cleanup into service-owned tasks. Once the
        // listener has stopped and joined every stream, acquiring every permit
        // is a barrier that keeps global Registry shutdown from racing those
        // bounded retry loops.
        let cleanup_barrier = Arc::clone(&self.cleanup_admission)
            .acquire_many_owned(MAX_BACKGROUND_CONNECTION_CLEANUPS as u32)
            .await
            .expect("connection cleanup admission is never closed");
        let leases = self
            .issued_leases
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .values()
            .cloned()
            .collect::<Vec<_>>();
        let (_cleanup_controller, control) =
            ExecutionController::with_timeout(CLEANUP_TIMEOUT_MS, TimeoutScope::Request);
        let mut errors = Vec::new();
        for issued in leases {
            if let Err(error) = self
                .cleanup_and_revoke(&issued.lease, issued.connection_id, &control)
                .await
            {
                errors.push(error);
            }
        }
        if errors.is_empty() {
            self.leases.invalidate_all();
        }
        drop(cleanup_barrier);
        errors
    }

    pub async fn release_connection(&self, connection_id: Uuid) -> Vec<PeerError> {
        let active = self
            .active
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .keys()
            .filter(|(candidate, _)| *candidate == connection_id)
            .copied()
            .collect::<Vec<_>>();
        for (_, request_id) in active {
            self.cancel_request(connection_id, request_id, CancellationReason::Shutdown);
        }
        let leases = self
            .issued_leases
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .values()
            .filter(|issued| issued.connection_id == connection_id)
            .cloned()
            .collect::<Vec<_>>();
        let (_cleanup_controller, control) =
            ExecutionController::with_timeout(CLEANUP_TIMEOUT_MS, TimeoutScope::Request);
        let mut errors = Vec::new();
        for issued in leases {
            if let Err(error) = self
                .cleanup_and_revoke(&issued.lease, connection_id, &control)
                .await
            {
                errors.push(error);
            }
        }
        errors
    }

    /// Admits connection teardown into a bounded service-owned task.
    ///
    /// Waiting for capacity applies backpressure to stream teardown. Once this
    /// method returns, dropping the receiver cannot cancel cleanup: the task
    /// retains the service and retries staged cleanup a bounded number of times.
    pub(crate) async fn admit_connection_cleanup(
        self: &Arc<Self>,
        connection_id: Uuid,
    ) -> oneshot::Receiver<Vec<PeerError>> {
        let permit = Arc::clone(&self.cleanup_admission)
            .acquire_owned()
            .await
            .expect("connection cleanup admission is never closed");
        let service = Arc::clone(self);
        let (completed, receiver) = oneshot::channel();
        std::mem::drop(tokio::spawn(async move {
            let errors = service.release_connection_with_retry(connection_id).await;
            let _ = completed.send(errors);
            drop(permit);
        }));
        receiver
    }

    async fn release_connection_with_retry(&self, connection_id: Uuid) -> Vec<PeerError> {
        let mut errors = Vec::new();
        for attempt in 0..MAX_CONNECTION_CLEANUP_ATTEMPTS {
            errors = match tokio::time::timeout(
                CONNECTION_CLEANUP_ATTEMPT_GRACE,
                self.release_connection(connection_id),
            )
            .await
            {
                Ok(errors) => errors,
                Err(_) => vec![peer_error("connection_cleanup_timeout", true, false)],
            };
            if errors.is_empty() || errors.iter().any(|error| !error.retryable) {
                break;
            }
            if attempt + 1 < MAX_CONNECTION_CLEANUP_ATTEMPTS {
                tokio::time::sleep(CONNECTION_CLEANUP_RETRY_DELAY).await;
            }
        }
        errors
    }

    async fn dispatch(
        &self,
        request: &PeerRequest,
        authenticated_subject: &str,
        connection_id: Uuid,
        control: &ExecutionControl,
    ) -> Result<PeerResult, PeerError> {
        if !self.is_ready()
            && !matches!(
                &request.operation,
                PeerOperation::Hello
                    | PeerOperation::Inventory
                    | PeerOperation::Health
                    | PeerOperation::Capabilities { .. }
            )
        {
            return Err(peer_error("node_starting", true, false));
        }
        match &request.operation {
            PeerOperation::Hello => Ok(PeerResult::Hello {
                node_id: self.node_id.clone(),
                epoch: self.epoch,
                max_frame_bytes: crate::MAX_PEER_FRAME_BYTES as u32,
                capabilities: crate::PeerProtocolCapabilities::REQUIRED,
            }),
            PeerOperation::Inventory => Ok(PeerResult::Inventory {
                inventory: InventorySnapshot {
                    node_id: self.node_id.clone(),
                    epoch: self.epoch,
                    revision: self.revision,
                    generated_at_ms: now_ms(),
                    devices: self.inventory.clone(),
                },
            }),
            PeerOperation::Health => self.health(control).await,
            PeerOperation::Capabilities { device_key } => {
                let handle = self.resolve(device_key).await?;
                let actions = handle.capabilities(control).await.map_err(runtime_error)?;
                Ok(PeerResult::Capabilities { actions })
            }
            PeerOperation::LeaseAcquire {
                device_key,
                owner_id,
                ttl_ms,
            } => {
                if owner_id != authenticated_subject {
                    return Err(peer_error("lease_owner_mismatch", false, false));
                }
                let now = now_ms();
                self.reap_expired_device(device_key, now, control).await?;
                let mut issued_leases = self
                    .issued_leases
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                let existing = issued_leases.values().find(|issued| {
                    issued.lease.device_key == *device_key
                        && self.leases.authorize(&issued.lease, now).is_ok()
                });
                if existing.is_some_and(|issued| issued.connection_id != connection_id) {
                    return Err(peer_error("lease_held", true, false));
                }
                let connection_lease_count = issued_leases
                    .values()
                    .filter(|issued| {
                        issued.connection_id == connection_id
                            && self.leases.authorize(&issued.lease, now).is_ok()
                    })
                    .count();
                if existing.is_none() && connection_lease_count >= MAX_CONNECTION_LEASES {
                    return Err(peer_error("connection_lease_limit", false, false));
                }
                let lease = self
                    .leases
                    .acquire(device_key, authenticated_subject, *ttl_ms, now)
                    .map_err(lease_error)?;
                let gate = issued_leases
                    .get(&lease.lease_id)
                    .map(|issued| Arc::clone(&issued.gate))
                    .unwrap_or_else(|| Arc::new(Mutex::new(())));
                issued_leases.insert(
                    lease.lease_id,
                    IssuedLease {
                        connection_id,
                        lease: lease.clone(),
                        gate,
                    },
                );
                Ok(PeerResult::Lease { lease })
            }
            PeerOperation::LeaseRenew { lease, ttl_ms } => {
                require_lease_subject(lease, authenticated_subject)?;
                let _gate = self
                    .lock_issued_lease(lease, connection_id, control, true)
                    .await?;
                let mut bindings = self.bindings.lock().await;
                if let Some(binding) = bindings.get(&lease.lease_id)
                    && (binding.peer_lease != *lease || binding.connection_id != connection_id)
                {
                    return Err(peer_error("lease_binding_mismatch", false, false));
                }
                let mut issued_leases = self
                    .issued_leases
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                let issued = issued_leases
                    .get_mut(&lease.lease_id)
                    .filter(|issued| {
                        issued.connection_id == connection_id && issued.lease == *lease
                    })
                    .ok_or_else(|| peer_error("lease_connection_mismatch", false, false))?;
                let renewed = self
                    .leases
                    .renew(lease, *ttl_ms, now_ms())
                    .map_err(lease_error)?;
                if let Some(binding) = bindings.get_mut(&lease.lease_id) {
                    binding.peer_lease = renewed.clone();
                }
                issued.lease = renewed.clone();
                Ok(PeerResult::Lease { lease: renewed })
            }
            PeerOperation::LeaseRelease { lease } => {
                require_lease_subject(lease, authenticated_subject)?;
                let _gate = self
                    .lock_issued_lease(lease, connection_id, control, true)
                    .await?;
                self.cleanup_binding_locked(lease, connection_id, control)
                    .await?;
                match self.leases.release(lease, now_ms()) {
                    Ok(()) => {}
                    Err(crate::LeaseError::Stale) => {
                        self.leases.revoke(lease).map_err(lease_error)?;
                    }
                    Err(error) => return Err(lease_error(error)),
                }
                self.issued_leases
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .remove(&lease.lease_id);
                Ok(PeerResult::Ack)
            }
            PeerOperation::Connect { device_key } => {
                let lease = self.authorized_lease(request, authenticated_subject, connection_id)?;
                let info = self
                    .connect(device_key, lease, connection_id, control)
                    .await?;
                Ok(PeerResult::Device { device: info })
            }
            PeerOperation::Disconnect { .. } => {
                let lease = self.authorized_lease(request, authenticated_subject, connection_id)?;
                self.cleanup_binding(lease, connection_id, control).await?;
                Ok(PeerResult::Ack)
            }
            PeerOperation::Observe {
                device_key,
                screenshot_omission,
                ui_snapshots_enabled,
                semantic_actions_enabled,
            } => {
                let lease = self.authorized_lease(request, authenticated_subject, connection_id)?;
                let observation = self
                    .observe(
                        ObserveInput {
                            device_key,
                            lease,
                            connection_id,
                            requested_omission: *screenshot_omission,
                            ui_snapshots_enabled: *ui_snapshots_enabled,
                            semantic_actions_enabled: *semantic_actions_enabled,
                        },
                        control,
                    )
                    .await?;
                Ok(PeerResult::Observation {
                    observation: Box::new(observation),
                })
            }
            PeerOperation::Execute {
                device_key,
                call,
                screenshot_omission,
                ui_snapshots_enabled,
                semantic_actions_enabled,
            } => {
                let lease = self.authorized_lease(request, authenticated_subject, connection_id)?;
                let fingerprint = serde_json::to_vec(&request.operation)
                    .map_err(|_| peer_error("call_fingerprint_failed", false, false))?;
                self.execute(
                    ExecuteInput {
                        fingerprint,
                        device_key,
                        lease,
                        connection_id,
                        requested_omission: *screenshot_omission,
                        ui_snapshots_enabled: *ui_snapshots_enabled,
                        semantic_actions_enabled: *semantic_actions_enabled,
                        call: call.clone(),
                    },
                    control,
                )
                .await
            }
            PeerOperation::EvidenceRead {
                evidence_id,
                offset,
                max_bytes,
                ..
            } => {
                let lease = self.authorized_lease(request, authenticated_subject, connection_id)?;
                self.evidence_chunk(
                    lease,
                    connection_id,
                    evidence_id,
                    *offset,
                    *max_bytes,
                    control,
                )
                .await
            }
            PeerOperation::Cancel { .. } => unreachable!("cancel handled before dispatch"),
        }
    }

    async fn health(&self, control: &ExecutionControl) -> Result<PeerResult, PeerError> {
        // Device health probes are independent and already bounded by the
        // service inventory and the shared operation control. Run them
        // concurrently so one slow route does not serialize every other
        // device's health result.
        let checks = self.devices.values().map(|id| async move {
            match self.registry.resolve(id).await {
                Ok(handle) => handle.health_check(control).await.is_ok(),
                Err(_) => false,
            }
        });
        let degraded = join_all(checks).await.into_iter().any(|healthy| !healthy);
        Ok(PeerResult::Health {
            state: if degraded {
                HealthState::Degraded
            } else {
                HealthState::Healthy
            },
            checked_at_ms: now_ms(),
        })
    }

    fn authorized_lease<'a>(
        &self,
        request: &'a PeerRequest,
        authenticated_subject: &str,
        connection_id: Uuid,
    ) -> Result<&'a PeerLease, PeerError> {
        let lease = request
            .lease
            .as_ref()
            .ok_or_else(|| peer_error("lease_required", false, false))?;
        require_lease_subject(lease, authenticated_subject)?;
        self.require_lease_connection(lease, connection_id)?;
        self.leases
            .authorize(lease, now_ms())
            .map_err(lease_error)?;
        Ok(lease)
    }

    fn require_lease_connection(
        &self,
        lease: &PeerLease,
        connection_id: Uuid,
    ) -> Result<(), PeerError> {
        match self
            .issued_leases
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&lease.lease_id)
        {
            Some(issued) if issued.connection_id == connection_id && issued.lease == *lease => {
                Ok(())
            }
            _ => Err(peer_error("lease_connection_mismatch", false, false)),
        }
    }

    async fn lock_issued_lease(
        &self,
        lease: &PeerLease,
        connection_id: Uuid,
        control: &ExecutionControl,
        require_active: bool,
    ) -> Result<OwnedMutexGuard<()>, PeerError> {
        let gate = match self
            .issued_leases
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&lease.lease_id)
        {
            Some(issued) if issued.connection_id == connection_id && issued.lease == *lease => {
                Arc::clone(&issued.gate)
            }
            _ => return Err(peer_error("lease_connection_mismatch", false, false)),
        };
        if control.is_cancelled() {
            return Err(peer_error("request_cancelled", false, false));
        }
        if control.is_expired() {
            return Err(peer_error("request_timed_out", true, false));
        }
        let guard = match control.remaining() {
            Some(remaining) => {
                tokio::select! {
                    biased;
                    _ = control.cancelled() => {
                        return Err(peer_error("request_cancelled", false, false));
                    }
                    _ = tokio::time::sleep(remaining) => {
                        return Err(peer_error("request_timed_out", true, false));
                    }
                    guard = gate.lock_owned() => guard,
                }
            }
            None => {
                tokio::select! {
                    biased;
                    _ = control.cancelled() => {
                        return Err(peer_error("request_cancelled", false, false));
                    }
                    guard = gate.lock_owned() => guard,
                }
            }
        };
        self.require_lease_connection(lease, connection_id)?;
        if require_active {
            self.leases
                .authorize(lease, now_ms())
                .map_err(lease_error)?;
        }
        Ok(guard)
    }

    async fn resolve(
        &self,
        device_key: &str,
    ) -> Result<devicerail_core::DriverHandle<S>, PeerError> {
        let id = self
            .devices
            .get(device_key)
            .ok_or_else(|| peer_error("device_not_found", false, false))?;
        self.registry.resolve(id).await.map_err(registry_error)
    }

    async fn connect(
        &self,
        device_key: &str,
        peer_lease: &PeerLease,
        connection_id: Uuid,
        control: &ExecutionControl,
    ) -> Result<DeviceInfo, PeerError> {
        let _gate = self
            .lock_issued_lease(peer_lease, connection_id, control, true)
            .await?;
        let existing = self
            .bindings
            .lock()
            .await
            .get(&peer_lease.lease_id)
            .cloned();
        if let Some(binding) = existing {
            if binding.peer_lease != *peer_lease || binding.connection_id != connection_id {
                return Err(peer_error("lease_binding_mismatch", false, false));
            }
            if binding
                .cleanup
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .started
            {
                return Err(peer_error("lease_cleanup_pending", true, false));
            }
            let session_started = binding.session_started.load(Ordering::SeqCst);
            let handle = self
                .registry
                .resolve(&binding.device_id)
                .await
                .map_err(registry_error)?;
            let core_lease = self
                .registry
                .acquire_lease(&handle, core_owner(peer_lease), now_ms())
                .await
                .map_err(pool_error)?;
            binding.publish_core_lease(core_lease.clone());
            let access = self
                .registry
                .access_with_lease(handle, core_lease.id, core_lease.owner_id, now_ms())
                .await
                .map_err(pool_error)?;
            let info = match access.connect(control).await {
                Ok(info) => info,
                Err(error) if !session_started => {
                    let original = runtime_error(error);
                    drop(access);
                    return Err(self
                        .rollback_connect_error(peer_lease, connection_id, original)
                        .await);
                }
                Err(error) => return Err(runtime_error(error)),
            };
            drop(access);
            if !session_started {
                if let Err(error) = self
                    .events
                    .start_session(binding.session_start.clone())
                    .await
                {
                    let original = event_error(error);
                    return Err(self
                        .rollback_connect_error(peer_lease, connection_id, original)
                        .await);
                }
                binding.session_started.store(true, Ordering::SeqCst);
            }
            return Ok(remote_info(info, device_key));
        }

        let handle = self.resolve(device_key).await?;
        handle.health_check(control).await.map_err(runtime_error)?;
        self.registry
            .record_health(&handle, PoolHealth::healthy(now_ms()), now_ms())
            .await
            .map_err(pool_error)?;
        let start = StartSession::new(None, Some(handle.id().clone()), now_ms());
        let binding = Binding {
            connection_id,
            peer_lease: peer_lease.clone(),
            device_id: handle.id().clone(),
            core_lease: Arc::new(StdMutex::new(None)),
            session_start: start.clone(),
            session_started: Arc::new(AtomicBool::new(false)),
            allowed_evidence: BTreeSet::new(),
            evidence_cursors: Arc::new(Mutex::new(EvidenceCursorSet::default())),
            cleanup: Arc::new(StdMutex::new(CleanupProgress::default())),
        };
        {
            let mut bindings = self.bindings.lock().await;
            if bindings.contains_key(&peer_lease.lease_id) {
                return Err(peer_error("lease_binding_conflict", false, false));
            }
            bindings.insert(peer_lease.lease_id, binding.clone());
        }
        let core_lease = match self
            .registry
            .acquire_lease(&handle, core_owner(peer_lease), now_ms())
            .await
        {
            Ok(core_lease) => {
                binding.publish_core_lease(core_lease.clone());
                core_lease
            }
            Err(error) => {
                let original = pool_error(error);
                return Err(self
                    .rollback_connect_error(peer_lease, connection_id, original)
                    .await);
            }
        };
        let access = self
            .registry
            .access_with_lease(handle, core_lease.id, core_lease.owner_id, now_ms())
            .await
            .map_err(pool_error)?;
        let info = match access.connect(control).await {
            Ok(info) => info,
            Err(error) => {
                let original = runtime_error(error);
                // `DriverAccess` holds the Core device read gate. Cleanup ends
                // by releasing the Core lease, which needs the write gate, so
                // never enter rollback while this access guard is still live.
                drop(access);
                return Err(self
                    .rollback_connect_error(peer_lease, connection_id, original)
                    .await);
            }
        };
        drop(access);
        if let Err(error) = self.events.start_session(start).await {
            let original = event_error(error);
            return Err(self
                .rollback_connect_error(peer_lease, connection_id, original)
                .await);
        }
        binding.session_started.store(true, Ordering::SeqCst);
        Ok(remote_info(info, device_key))
    }

    async fn rollback_connect(
        &self,
        peer_lease: &PeerLease,
        connection_id: Uuid,
    ) -> Result<(), PeerError> {
        let (_controller, cleanup_control) =
            ExecutionController::with_timeout(CLEANUP_TIMEOUT_MS, TimeoutScope::Request);
        self.cleanup_binding_locked(peer_lease, connection_id, &cleanup_control)
            .await
    }

    async fn rollback_connect_error(
        &self,
        peer_lease: &PeerLease,
        connection_id: Uuid,
        original: PeerError,
    ) -> PeerError {
        match self.rollback_connect(peer_lease, connection_id).await {
            Ok(()) => original,
            Err(_) => peer_error("connect_rollback_incomplete", true, true),
        }
    }

    async fn renewed_binding(
        &self,
        lease: &PeerLease,
        connection_id: Uuid,
    ) -> Result<Binding, PeerError> {
        let binding = self
            .bindings
            .lock()
            .await
            .get(&lease.lease_id)
            .cloned()
            .ok_or_else(|| peer_error("device_not_connected", true, false))?;
        if binding.peer_lease != *lease || binding.connection_id != connection_id {
            return Err(peer_error("lease_binding_mismatch", false, false));
        }
        if binding
            .cleanup
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .started
        {
            return Err(peer_error("lease_cleanup_pending", true, false));
        }
        let handle = self
            .registry
            .resolve(&binding.device_id)
            .await
            .map_err(registry_error)?;
        let core_lease = self
            .registry
            .acquire_lease(&handle, core_owner(lease), now_ms())
            .await
            .map_err(pool_error)?;
        binding.publish_core_lease(core_lease);
        Ok(binding)
    }

    async fn observe(
        &self,
        input: ObserveInput<'_>,
        control: &ExecutionControl,
    ) -> Result<Observation, PeerError> {
        let expected_omission = match self.registry.screenshot_policy() {
            ScreenshotPolicy::Capture => None,
            ScreenshotPolicy::Omit => Some(ScreenshotOmissionReason::Policy),
        };
        if input.requested_omission != expected_omission
            && input.requested_omission != Some(ScreenshotOmissionReason::Policy)
        {
            return Err(peer_error("screenshot_policy_mismatch", false, false));
        }
        let _gate = self
            .lock_issued_lease(input.lease, input.connection_id, control, true)
            .await?;
        let binding = self
            .renewed_binding(input.lease, input.connection_id)
            .await?;
        let handle = self
            .registry
            .resolve(&binding.device_id)
            .await
            .map_err(registry_error)?;
        let core_lease = binding
            .core_lease()
            .ok_or_else(|| peer_error("core_lease_missing", true, true))?;
        let access = self
            .registry
            .access_with_lease(handle, core_lease.id, core_lease.owner_id, now_ms())
            .await
            .map_err(pool_error)?;
        let policy = if input.requested_omission == Some(ScreenshotOmissionReason::Policy) {
            ScreenshotPolicy::Omit
        } else {
            ScreenshotPolicy::Capture
        };
        let mut observation = access
            .observe(
                &OperationContext::new(binding.session_start.session_id.clone(), None)
                    .with_control(control.clone())
                    .with_screenshot_policy(policy)
                    .with_ui_snapshots_enabled(input.ui_snapshots_enabled)
                    .with_semantic_actions_enabled(input.semantic_actions_enabled),
            )
            .await
            .map_err(runtime_error)?;
        observation.device_id = DeviceId::new(input.device_key);
        self.allow_observation_evidence(input.lease.lease_id, &observation)
            .await?;
        Ok(observation)
    }

    async fn execute(
        &self,
        input: ExecuteInput<'_>,
        control: &ExecutionControl,
    ) -> Result<PeerResult, PeerError> {
        let _gate = self
            .lock_issued_lease(input.lease, input.connection_id, control, true)
            .await?;
        match self.calls.register(input.call.id, &input.fingerprint) {
            CallLedgerDecision::Conflict => {
                return Err(peer_error("call_id_conflict", false, false));
            }
            CallLedgerDecision::Duplicate => {
                return match self
                    .terminals
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .get(&input.call.id, input.lease.lease_id)
                {
                    Some(CachedTerminal::Success(result)) => Ok(result),
                    Some(CachedTerminal::Failure(error)) => Err(error),
                    None if self
                        .in_flight_calls
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .contains(&input.call.id) =>
                    {
                        Err(peer_error("call_in_flight", true, false))
                    }
                    None => Err(peer_error("call_terminal_unavailable", false, true)),
                };
            }
            CallLedgerDecision::FirstSeen => {}
        }
        self.in_flight_calls
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(input.call.id);
        let in_flight = InFlightCallGuard {
            call_id: input.call.id,
            calls: Arc::clone(&self.in_flight_calls),
        };
        let terminal = self.execute_first(&input, control).await;
        let cached = match &terminal {
            Ok(result) => CachedTerminal::Success(result.clone()),
            Err(error) => CachedTerminal::Failure(error.clone()),
        };
        self.terminals
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(input.call.id, input.lease.lease_id, cached);
        drop(in_flight);
        terminal
    }

    async fn execute_first(
        &self,
        input: &ExecuteInput<'_>,
        control: &ExecutionControl,
    ) -> Result<PeerResult, PeerError> {
        let local_id = self
            .devices
            .get(input.device_key)
            .ok_or_else(|| peer_error("device_not_found", false, false))?;
        let protection = self
            .registry
            .action_protection(local_id, &input.call.name)
            .await
            .map_err(registry_error)?
            .ok_or_else(|| peer_error("unknown_action", false, false))?;
        let expected_omission = match protection {
            ActionProtection::Protected => Some(ScreenshotOmissionReason::ProtectedAction),
            ActionProtection::Standard => match self.registry.screenshot_policy() {
                ScreenshotPolicy::Capture => None,
                ScreenshotPolicy::Omit => Some(ScreenshotOmissionReason::Policy),
            },
        };
        let allowed_override = protection == ActionProtection::Standard
            && input.requested_omission == Some(ScreenshotOmissionReason::Policy);
        if input.requested_omission != expected_omission && !allowed_override {
            return Err(peer_error("screenshot_policy_mismatch", false, false));
        }
        let binding = self
            .renewed_binding(input.lease, input.connection_id)
            .await?;
        let handle = self
            .registry
            .resolve(&binding.device_id)
            .await
            .map_err(registry_error)?;
        let core_lease = binding
            .core_lease()
            .ok_or_else(|| peer_error("core_lease_missing", true, true))?;
        let access = self
            .registry
            .access_with_lease(handle, core_lease.id, core_lease.owner_id, now_ms())
            .await
            .map_err(pool_error)?;
        let policy = if input.requested_omission.is_some() {
            ScreenshotPolicy::Omit
        } else {
            ScreenshotPolicy::Capture
        };
        let context = OperationContext::new(binding.session_start.session_id.clone(), None)
            .with_control(control.clone())
            .with_screenshot_policy(policy)
            .with_ui_snapshots_enabled(input.ui_snapshots_enabled)
            .with_semantic_actions_enabled(input.semantic_actions_enabled);
        let mut result = access
            .execute(&context, input.call.clone())
            .await
            .map_err(runtime_error)?;
        rewrite_action_result(&mut result, input.device_key);
        self.allow_action_evidence(input.lease.lease_id, &result)
            .await?;
        Ok(PeerResult::Action {
            result: Box::new(result),
        })
    }

    async fn allow_observation_evidence(
        &self,
        lease_id: Uuid,
        observation: &Observation,
    ) -> Result<(), PeerError> {
        let mut bindings = self.bindings.lock().await;
        let binding = bindings
            .get_mut(&lease_id)
            .ok_or_else(|| peer_error("lease_binding_missing", false, false))?;
        if let Some(asset) = &observation.screenshot {
            binding.allowed_evidence.insert(asset.id.clone());
        }
        if let Some(snapshot) = &observation.ui_snapshot {
            binding
                .allowed_evidence
                .insert(snapshot.evidence.id.clone());
        }
        Ok(())
    }

    async fn allow_action_evidence(
        &self,
        lease_id: Uuid,
        result: &ActionResult,
    ) -> Result<(), PeerError> {
        let mut bindings = self.bindings.lock().await;
        let binding = bindings
            .get_mut(&lease_id)
            .ok_or_else(|| peer_error("lease_binding_missing", false, false))?;
        for asset in result
            .evidence
            .iter()
            .chain(
                result
                    .before
                    .iter()
                    .filter_map(|value| value.screenshot.as_ref()),
            )
            .chain(
                result
                    .after
                    .iter()
                    .filter_map(|value| value.screenshot.as_ref()),
            )
            .chain(
                result
                    .before
                    .iter()
                    .filter_map(|value| value.ui_snapshot.as_ref().map(|tree| &tree.evidence)),
            )
            .chain(
                result
                    .after
                    .iter()
                    .filter_map(|value| value.ui_snapshot.as_ref().map(|tree| &tree.evidence)),
            )
        {
            binding.allowed_evidence.insert(asset.id.clone());
        }
        Ok(())
    }

    async fn evidence_chunk(
        &self,
        lease: &PeerLease,
        connection_id: Uuid,
        evidence_id: &str,
        offset: u64,
        max_bytes: u32,
        control: &ExecutionControl,
    ) -> Result<PeerResult, PeerError> {
        if control.is_cancelled() {
            return Err(peer_error("request_cancelled", false, false));
        }
        if control.is_expired() {
            return Err(peer_error("request_timed_out", true, false));
        }
        let _gate = self
            .lock_issued_lease(lease, connection_id, control, true)
            .await?;
        let remaining = control
            .remaining()
            .expect("peer service operation controls always have a deadline");
        tokio::select! {
            biased;
            _ = control.cancelled() => Err(peer_error("request_cancelled", false, false)),
            _ = tokio::time::sleep(remaining) => {
                Err(peer_error("request_timed_out", true, false))
            }
            result = self.read_evidence_chunk(
                lease,
                connection_id,
                evidence_id,
                offset,
                max_bytes,
            ) => result,
        }
    }

    async fn read_evidence_chunk(
        &self,
        lease: &PeerLease,
        connection_id: Uuid,
        evidence_id: &str,
        offset: u64,
        max_bytes: u32,
    ) -> Result<PeerResult, PeerError> {
        let binding = self
            .bindings
            .lock()
            .await
            .get(&lease.lease_id)
            .cloned()
            .ok_or_else(|| peer_error("device_not_connected", true, false))?;
        if binding.peer_lease != *lease
            || binding.connection_id != connection_id
            || !binding.allowed_evidence.contains(evidence_id)
        {
            return Err(peer_error("evidence_not_authorized", false, false));
        }
        if binding
            .cleanup
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .started
        {
            return Err(peer_error("lease_cleanup_pending", true, false));
        }
        let digest = evidence_id
            .strip_prefix("sha256:")
            .ok_or_else(|| peer_error("evidence_reference_invalid", false, false))
            .and_then(|value| {
                Sha256Digest::parse(value.to_owned())
                    .map_err(|_| peer_error("evidence_reference_invalid", false, false))
            })?;
        // Remove the cursor before I/O. If cancellation drops this future after
        // a partial read, the cursor is dropped rather than reinserted with a
        // stale offset; a later request safely reopens and seeks once.
        let existing = binding.evidence_cursors.lock().await.take(evidence_id);
        let mut cursor =
            match existing.filter(|cursor| cursor.offset == offset && cursor.digest == digest) {
                Some(cursor) => cursor,
                None => self.open_evidence_cursor(digest, offset).await?,
            };
        let chunk_size =
            u64::from(max_bytes).min(cursor.total_size.saturating_sub(offset)) as usize;
        let mut chunk = vec![0_u8; chunk_size];
        cursor
            .body
            .read_exact(&mut chunk)
            .await
            .map_err(|_| peer_error("evidence_size_mismatch", false, false))?;
        let media_type = cursor.media_type.clone();
        let total_size = cursor.total_size;
        let digest_text = cursor.digest.to_string();
        let end = offset + chunk_size as u64;
        let done = end == total_size;
        if done {
            let mut trailing = [0_u8; 1];
            if cursor
                .body
                .read(&mut trailing)
                .await
                .map_err(|_| peer_error("evidence_read_failed", true, false))?
                != 0
            {
                return Err(peer_error("evidence_size_mismatch", false, false));
            }
        } else {
            cursor.offset = end;
            binding
                .evidence_cursors
                .lock()
                .await
                .insert(evidence_id.to_owned(), cursor);
        }
        Ok(PeerResult::EvidenceChunk {
            evidence_id: evidence_id.to_owned(),
            media_type,
            total_size,
            offset,
            data_base64: BASE64.encode(&chunk),
            sha256: Some(digest_text),
            done,
        })
    }

    async fn open_evidence_cursor(
        &self,
        digest: Sha256Digest,
        offset: u64,
    ) -> Result<EvidenceReadCursor, PeerError> {
        let metadata = self
            .evidence
            .metadata(&digest)
            .await
            .map_err(evidence_error)?;
        if metadata.byte_length() == 0 || metadata.byte_length() > MAX_EVIDENCE_BYTES {
            return Err(peer_error("evidence_size_invalid", false, false));
        }
        if offset >= metadata.byte_length() {
            return Err(peer_error("evidence_offset_invalid", false, false));
        }
        let mut body = self.evidence.open(&digest).await.map_err(evidence_error)?;
        if offset > 0 {
            let skipped = tokio::io::copy(&mut (&mut body).take(offset), &mut tokio::io::sink())
                .await
                .map_err(|_| peer_error("evidence_read_failed", true, false))?;
            if skipped != offset {
                return Err(peer_error("evidence_size_mismatch", false, false));
            }
        }
        Ok(EvidenceReadCursor {
            digest,
            media_type: metadata.media_type().to_owned(),
            total_size: metadata.byte_length(),
            offset,
            body,
        })
    }

    async fn cleanup_binding(
        &self,
        lease: &PeerLease,
        connection_id: Uuid,
        control: &ExecutionControl,
    ) -> Result<(), PeerError> {
        let _gate = self
            .lock_issued_lease(lease, connection_id, control, false)
            .await?;
        self.cleanup_binding_locked(lease, connection_id, control)
            .await
    }

    async fn cleanup_binding_locked(
        &self,
        lease: &PeerLease,
        connection_id: Uuid,
        control: &ExecutionControl,
    ) -> Result<(), PeerError> {
        let binding = {
            let bindings = self.bindings.lock().await;
            let Some(binding) = bindings.get(&lease.lease_id) else {
                drop(bindings);
                self.release_core_owner_leases(lease).await;
                return Ok(());
            };
            if binding.peer_lease != *lease || binding.connection_id != connection_id {
                return Err(peer_error("lease_binding_mismatch", false, false));
            }
            let binding = binding.clone();
            binding
                .cleanup
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .started = true;
            binding
        };
        let progress = || {
            *binding
                .cleanup
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
        };
        if !progress().disconnected {
            if let Some(core_lease) = binding.core_lease() {
                let handle = match self.registry.resolve(&binding.device_id).await {
                    Ok(handle) => Some(handle),
                    Err(RegistryError::DeviceNotFound(_)) => None,
                    Err(error) => return Err(registry_error(error)),
                };
                if let Some(handle) = handle {
                    let access = match self
                        .registry
                        .cleanup_access_with_lease(
                            handle.clone(),
                            core_lease.id,
                            core_lease.owner_id,
                            now_ms(),
                        )
                        .await
                    {
                        Ok(access) => access,
                        Err(DevicePoolError::LeaseExpired | DevicePoolError::LeaseNotFound) => self
                            .registry
                            .access_available_to(handle, core_lease.owner_id, now_ms())
                            .await
                            .map_err(pool_error)?,
                        Err(error) => return Err(pool_error(error)),
                    };
                    access.disconnect(control).await.map_err(runtime_error)?;
                }
            }
            binding
                .cleanup
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .disconnected = true;
        }
        if !progress().session_ended {
            match self
                .events
                .end_session(EndSession {
                    session_id: binding.session_start.session_id.clone(),
                    request_id: None,
                    device_id: Some(binding.device_id.clone()),
                    at_ms: now_ms(),
                    outcome: SessionOutcome::Completed,
                    reason: None,
                })
                .await
            {
                Ok(_)
                | Err(EventStoreError::SessionEnded(_))
                | Err(EventStoreError::SessionNotFound(_)) => {}
                Err(error) => return Err(event_error(error)),
            }
            binding
                .cleanup
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .session_ended = true;
        }
        if !progress().evidence_cleaned {
            cleanup_ended_session(
                self.events.as_ref(),
                self.evidence.as_ref(),
                &binding.session_start.session_id,
                now_ms(),
            )
            .await
            .map_err(|_| peer_error("session_cleanup_failed", true, false))?;
            binding
                .cleanup
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .evidence_cleaned = true;
        }
        if !progress().core_lease_released {
            self.release_core_owner_leases(lease).await;
            binding
                .cleanup
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .core_lease_released = true;
        }
        let mut bindings = self.bindings.lock().await;
        match bindings.get(&lease.lease_id) {
            Some(current)
                if current.connection_id == connection_id
                    && current.peer_lease == *lease
                    && Arc::ptr_eq(&current.cleanup, &binding.cleanup) =>
            {
                bindings.remove(&lease.lease_id);
            }
            None => {}
            Some(_) => return Err(peer_error("lease_binding_mismatch", false, false)),
        }
        self.terminals
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove_lease(lease.lease_id);
        Ok(())
    }

    async fn release_core_owner_leases(&self, lease: &PeerLease) {
        let owner = core_owner(lease);
        let released = self.registry.release_owner_leases(owner, now_ms()).await;
        debug_assert!(released.iter().all(|lease| lease.owner_id == owner));
    }

    async fn cleanup_and_revoke(
        &self,
        lease: &PeerLease,
        connection_id: Uuid,
        control: &ExecutionControl,
    ) -> Result<(), PeerError> {
        let _gate = self
            .lock_issued_lease(lease, connection_id, control, false)
            .await?;
        self.cleanup_binding_locked(lease, connection_id, control)
            .await?;
        self.leases.revoke(lease).map_err(lease_error)?;
        self.issued_leases
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&lease.lease_id);
        Ok(())
    }

    async fn reap_expired_device(
        &self,
        device_key: &str,
        now: u64,
        control: &ExecutionControl,
    ) -> Result<(), PeerError> {
        let expired = self
            .issued_leases
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .values()
            .filter(|issued| {
                issued.lease.device_key == device_key
                    && self.leases.authorize(&issued.lease, now).is_err()
            })
            .cloned()
            .collect::<Vec<_>>();
        let cleanup_control = control.with_timeout(CLEANUP_TIMEOUT_MS, TimeoutScope::Request);
        for issued in expired {
            self.cleanup_and_revoke(&issued.lease, issued.connection_id, &cleanup_control)
                .await?;
        }
        Ok(())
    }
}

fn stable_device_key(id: &DeviceId) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"devicerail.distributed.device-key.v1\0");
    hasher.update(id.0.as_bytes());
    format!("d-{}", hex::encode(hasher.finalize()))
}

fn operation_method(operation: &PeerOperation) -> OperationMethod {
    match operation {
        PeerOperation::Hello => OperationMethod::Hello,
        PeerOperation::Inventory => OperationMethod::Inventory,
        PeerOperation::Health => OperationMethod::Health,
        PeerOperation::Capabilities { .. } => OperationMethod::Capabilities,
        PeerOperation::LeaseAcquire { .. } => OperationMethod::LeaseAcquire,
        PeerOperation::LeaseRenew { .. } => OperationMethod::LeaseRenew,
        PeerOperation::LeaseRelease { .. } => OperationMethod::LeaseRelease,
        PeerOperation::Connect { .. } => OperationMethod::Connect,
        PeerOperation::Disconnect { .. } => OperationMethod::Disconnect,
        PeerOperation::Observe { .. } => OperationMethod::Observe,
        PeerOperation::Execute { .. } => OperationMethod::Execute,
        PeerOperation::EvidenceRead { .. } => OperationMethod::EvidenceRead,
        PeerOperation::Cancel { .. } => OperationMethod::Cancel,
    }
}

fn terminal_bytes(terminal: &CachedTerminal) -> usize {
    match terminal {
        CachedTerminal::Success(result) => {
            serde_json::to_vec(result).map_or(usize::MAX, |v| v.len())
        }
        CachedTerminal::Failure(error) => serde_json::to_vec(error).map_or(usize::MAX, |v| v.len()),
    }
}

fn remote_info(mut info: DeviceInfo, device_key: &str) -> DeviceInfo {
    info.id = DeviceId::new(device_key);
    info
}

fn rewrite_observation(observation: &mut Observation, device_key: &str) {
    observation.device_id = DeviceId::new(device_key);
}

fn rewrite_action_result(result: &mut ActionResult, device_key: &str) {
    if let Some(before) = &mut result.before {
        rewrite_observation(before, device_key);
    }
    if let Some(after) = &mut result.after {
        rewrite_observation(after, device_key);
    }
}

fn core_owner(lease: &PeerLease) -> LeaseOwnerId {
    LeaseOwnerId::new(lease.lease_id)
}

fn require_lease_subject(lease: &PeerLease, subject: &str) -> Result<(), PeerError> {
    if lease.owner_id == subject {
        Ok(())
    } else {
        Err(peer_error("lease_owner_mismatch", false, false))
    }
}

fn operation_control(
    timeout_ms: Option<u64>,
) -> Result<(ExecutionController, ExecutionControl), PeerServiceError> {
    match timeout_ms {
        Some(timeout) if timeout <= MAX_OPERATION_TIMEOUT_MS => Ok(
            ExecutionController::with_timeout(timeout, TimeoutScope::Request),
        ),
        Some(_) => Err(PeerServiceError::Protocol),
        None => Ok(ExecutionController::with_timeout(
            MAX_OPERATION_TIMEOUT_MS,
            TimeoutScope::Request,
        )),
    }
}

fn peer_error(code: &str, retryable: bool, outcome_unknown: bool) -> PeerError {
    debug_assert!(valid_code(code));
    PeerError {
        code: code.to_owned(),
        retryable,
        outcome_unknown,
    }
}

fn lease_error(error: crate::LeaseError) -> PeerError {
    peer_error(
        error.code(),
        matches!(error, crate::LeaseError::Held),
        false,
    )
}

fn registry_error(error: devicerail_core::RegistryError) -> PeerError {
    let public = error.to_error_info();
    peer_error(&public.code, public.retryable, false)
}

fn pool_error(_: devicerail_core::DevicePoolError) -> PeerError {
    peer_error("device_pool_error", true, false)
}

fn runtime_error(error: RuntimeError) -> PeerError {
    let public = error.to_error_info();
    let outcome_unknown = matches!(error, RuntimeError::Driver(devicerail_core::DriverError::Platform { ref code, .. }) if code.contains("outcome_unknown"));
    peer_error(
        &public.code,
        public.retryable && !outcome_unknown,
        outcome_unknown,
    )
}

fn event_error(error: devicerail_core::EventStoreError) -> PeerError {
    let public = error.to_error_info();
    peer_error(&public.code, public.retryable, false)
}

fn evidence_error(error: devicerail_core::EvidenceError) -> PeerError {
    let public = error.to_error_info();
    peer_error(&public.code, public.retryable, false)
}
