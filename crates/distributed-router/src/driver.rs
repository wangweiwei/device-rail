use std::{
    collections::{BTreeMap, BTreeSet},
    io::Cursor,
    sync::Arc,
    time::Instant,
};

use async_trait::async_trait;
use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
use devicerail_core::{
    DeviceDriver, DeviceOperationResult, DriverError, DriverOperationContext, DriverResult,
    ExecutionControl, ScreenshotPolicy, now_ms,
};
use devicerail_protocol::{
    ActionCall, ActionDefinition, ActionExecution, ActionProtection, ActionResult, AssetRef,
    CLEAR_ELEMENT_ACTION, ClearElementArguments, DeviceId, DeviceInfo, ElementActionOutput,
    FIND_ELEMENT_ACTION, FindElementArguments, FindElementResult, Observation,
    SET_ELEMENT_VALUE_ACTION, ScreenshotOmissionReason, SetElementValueArguments,
    TAP_ELEMENT_ACTION, TapElementArguments, UiContextRef, UiNodeRef, UiSnapshot,
    UiSnapshotOmissionReason, UiSnapshotRef, WAIT_FOR_ELEMENT_ACTION, WaitForElementArguments,
    WaitForElementResult, is_semantic_action_name,
};
use serde_json::Value;
use tokio::sync::Mutex;

use crate::{
    NodeId, OperationMethod, OperationOutcome, PeerLease, PeerOperation, PeerRequest, PeerResponse,
    PeerResult, PeerTransport, RemoteDeviceDescriptor, TelemetrySink, TransportError,
    model::{
        MAX_CAPABILITIES, MAX_METADATA_BYTES, MAX_SAFE_INTEGER, valid_evidence_id, valid_identifier,
    },
    telemetry,
};

const MAX_ACTION_OUTPUT_BYTES: usize = 256 * 1024;
const MAX_EVIDENCE_BYTES: usize = 16 * 1024 * 1024;
const MAX_EVIDENCE_CHUNKS: usize = 64;
const EVIDENCE_CHUNK_BYTES: u32 = 256 * 1024;
const HEALTH_TIMESTAMP_MAX_AGE_MS: u64 = 60_000;
const HEALTH_TIMESTAMP_MAX_SKEW_MS: u64 = 5_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RemoteDriverConfig {
    lease_ttl_ms: u64,
    renew_before_ms: u64,
}

impl RemoteDriverConfig {
    pub fn new(lease_ttl_ms: u64, renew_before_ms: u64) -> Result<Self, DriverError> {
        if !(1_000..=5 * 60_000).contains(&lease_ttl_ms)
            || renew_before_ms >= lease_ttl_ms
            || renew_before_ms > 60_000
        {
            return Err(platform("remote_driver_config_invalid", false));
        }
        Ok(Self {
            lease_ttl_ms,
            renew_before_ms,
        })
    }
}

impl Default for RemoteDriverConfig {
    fn default() -> Self {
        Self {
            lease_ttl_ms: 30_000,
            renew_before_ms: 5_000,
        }
    }
}

#[derive(Default)]
struct DriverState {
    lease: Option<PeerLease>,
    lease_received_at: Option<Instant>,
    connected: bool,
}

struct ImportedAsset {
    reference: AssetRef,
    byte_length: u64,
    ui_snapshot: Option<Arc<UiSnapshot>>,
}

type ImportedAssets = BTreeMap<String, ImportedAsset>;

/// A real `DeviceDriver` that forwards operations to one authenticated node.
/// The stable local id is namespaced by node and remote device key.
pub struct RemoteDeviceDriver {
    id: DeviceId,
    node_id: NodeId,
    node_epoch: u64,
    descriptor: RemoteDeviceDescriptor,
    owner_id: String,
    config: RemoteDriverConfig,
    capabilities: Arc<Vec<ActionDefinition>>,
    protection: BTreeMap<String, ActionProtection>,
    validators: BTreeMap<String, jsonschema::Validator>,
    transport: Arc<dyn PeerTransport>,
    telemetry: Arc<dyn TelemetrySink>,
    state: Mutex<DriverState>,
}

impl std::fmt::Debug for RemoteDeviceDriver {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("RemoteDeviceDriver")
            .field("id", &self.id)
            .field("node_id", &self.node_id)
            .field("node_epoch", &self.node_epoch)
            .field("owner_id", &self.owner_id)
            .field("capability_count", &self.capabilities.len())
            .field("security", self.transport.security())
            .finish_non_exhaustive()
    }
}

impl RemoteDeviceDriver {
    /// Constructs a route from a capability contract already obtained during
    /// authenticated discovery. Most callers should use [`RemoteNode`] and
    /// [`NodeRouter`](crate::NodeRouter), which fetch this contract from the
    /// peer before exposing the Driver.
    #[allow(clippy::too_many_arguments)]
    pub fn from_contract(
        node_id: NodeId,
        node_epoch: u64,
        descriptor: RemoteDeviceDescriptor,
        owner_id: &str,
        config: RemoteDriverConfig,
        transport: Arc<dyn PeerTransport>,
        telemetry: Arc<dyn TelemetrySink>,
        actions: Vec<ActionDefinition>,
    ) -> DriverResult<Self> {
        if node_epoch == 0
            || node_epoch > MAX_SAFE_INTEGER
            || !valid_identifier(owner_id, 64)
            || transport.expected_node_id() != &node_id
            || transport.security().subject().is_empty()
        {
            return Err(platform("remote_driver_config_invalid", false));
        }
        descriptor
            .validate()
            .map_err(|_| platform("remote_device_descriptor_invalid", false))?;
        let (protection, validators) = validate_capabilities(&actions)?;
        let id = DeviceId::new(format!(
            "remote:{}:{}",
            node_id.as_str(),
            descriptor.device_key
        ));
        Ok(Self {
            id,
            node_id,
            node_epoch,
            descriptor,
            owner_id: owner_id.to_owned(),
            config,
            capabilities: Arc::new(actions),
            protection,
            validators,
            transport,
            telemetry,
            state: Mutex::new(DriverState::default()),
        })
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn load(
        node_id: NodeId,
        node_epoch: u64,
        descriptor: RemoteDeviceDescriptor,
        owner_id: &str,
        config: RemoteDriverConfig,
        transport: Arc<dyn PeerTransport>,
        telemetry: Arc<dyn TelemetrySink>,
        control: &ExecutionControl,
    ) -> DriverResult<Self> {
        let request = PeerRequest::new(
            node_id.clone(),
            Some(node_epoch),
            PeerOperation::Capabilities {
                device_key: descriptor.device_key.clone(),
            },
        );
        let result = exchange_raw(
            &transport,
            &telemetry,
            request,
            OperationMethod::Capabilities,
            control,
            false,
        )
        .await?;
        let PeerResult::Capabilities { actions } = result else {
            return Err(DriverError::Protocol(
                "peer returned the wrong capabilities result".to_owned(),
            ));
        };
        Self::from_contract(
            node_id, node_epoch, descriptor, owner_id, config, transport, telemetry, actions,
        )
    }

    pub async fn device_info(&self) -> DeviceInfo {
        let connected = self.state.lock().await.connected;
        self.info(connected)
    }

    fn info(&self, connected: bool) -> DeviceInfo {
        DeviceInfo {
            id: self.id.clone(),
            name: self.descriptor.name.clone(),
            platform: self.descriptor.platform.clone(),
            os_version: self.descriptor.os_version.clone(),
            connected,
        }
    }

    async fn ensure_lease(
        &self,
        state: &mut DriverState,
        control: &ExecutionControl,
    ) -> DriverResult<PeerLease> {
        let lease_age = state.lease_received_at.map(|received| received.elapsed());
        if let Some(current) = state.lease.clone()
            && lease_age.is_some_and(|age| {
                age < std::time::Duration::from_millis(
                    self.config
                        .lease_ttl_ms
                        .saturating_sub(self.config.renew_before_ms),
                )
            })
        {
            return Ok(current);
        }
        let operation = match state.lease.clone() {
            Some(lease)
                if lease_age.is_some_and(|age| {
                    age < std::time::Duration::from_millis(self.config.lease_ttl_ms)
                }) =>
            {
                PeerOperation::LeaseRenew {
                    lease: lease.clone(),
                    ttl_ms: self.config.lease_ttl_ms,
                }
            }
            _ => PeerOperation::LeaseAcquire {
                device_key: self.descriptor.device_key.clone(),
                owner_id: self.owner_id.clone(),
                ttl_ms: self.config.lease_ttl_ms,
            },
        };
        let method = match operation {
            PeerOperation::LeaseRenew { .. } => OperationMethod::LeaseRenew,
            _ => OperationMethod::LeaseAcquire,
        };
        let mut request = PeerRequest::new(self.node_id.clone(), Some(self.node_epoch), operation);
        if let PeerOperation::LeaseRenew { lease, .. } = &request.operation {
            request.lease = Some(lease.clone());
        }
        let result = self.exchange(request, method, control, false).await?;
        let PeerResult::Lease { lease } = result else {
            return Err(DriverError::Protocol(
                "peer returned the wrong lease result".to_owned(),
            ));
        };
        lease
            .validate()
            .map_err(|_| DriverError::Protocol("peer lease is invalid".to_owned()))?;
        if lease.device_key != self.descriptor.device_key
            || lease.owner_id != self.owner_id
            || lease.node_epoch != self.node_epoch
        {
            return Err(DriverError::Protocol(
                "peer lease does not match the route".to_owned(),
            ));
        }
        state.lease = Some(lease.clone());
        state.lease_received_at = Some(Instant::now());
        Ok(lease)
    }

    async fn exchange(
        &self,
        request: PeerRequest,
        method: OperationMethod,
        control: &ExecutionControl,
        mutation: bool,
    ) -> DriverResult<PeerResult> {
        exchange_raw(
            &self.transport,
            &self.telemetry,
            request,
            method,
            control,
            mutation,
        )
        .await
    }

    async fn request_with_lease(
        &self,
        operation: PeerOperation,
        lease: PeerLease,
        method: OperationMethod,
        control: &ExecutionControl,
        mutation: bool,
    ) -> DriverResult<PeerResult> {
        let mut request = PeerRequest::new(self.node_id.clone(), Some(self.node_epoch), operation);
        request.lease = Some(lease);
        self.exchange(request, method, control, mutation).await
    }

    async fn import_observation(
        &self,
        context: &DriverOperationContext,
        state: &mut DriverState,
        mut observation: Observation,
        expected_omission: Option<ScreenshotOmissionReason>,
        imported: &mut ImportedAssets,
    ) -> DeviceOperationResult<Observation> {
        if !context.ui_snapshots_enabled()
            && (observation.ui_snapshot.is_some() || observation.ui_snapshot_omission.is_some())
        {
            return Err(DriverError::Protocol(
                "peer returned UI Snapshot fields for a disabled operation".to_owned(),
            )
            .into());
        }
        let raw_id = DeviceId::new(self.descriptor.device_key.clone());
        if observation.device_id != raw_id && observation.device_id != self.id {
            return Err(DriverError::Protocol(
                "peer observation identifies another device".to_owned(),
            )
            .into());
        }
        if observation.id.is_nil()
            || observation.captured_at_ms == 0
            || observation.captured_at_ms > MAX_SAFE_INTEGER
            || observation.viewport.width == 0
            || observation.viewport.height == 0
            || !observation.viewport.scale_factor.is_finite()
            || observation.viewport.scale_factor <= 0.0
            || observation.screenshot_omission != expected_omission
            || serde_json::to_vec(&observation.metadata)
                .map_or(true, |bytes| bytes.len() > MAX_METADATA_BYTES)
        {
            return Err(DriverError::Protocol("peer observation is invalid".to_owned()).into());
        }
        if expected_omission.is_some() && observation.screenshot.is_some() {
            return Err(DriverError::Protocol(
                "peer returned screenshot evidence despite omission".to_owned(),
            )
            .into());
        }
        if observation.ui_snapshot.is_some() && observation.ui_snapshot_omission.is_some() {
            return Err(DriverError::Protocol(
                "peer observation returned both a UI Snapshot and an omission".to_owned(),
            )
            .into());
        }
        if observation
            .ui_snapshot
            .as_ref()
            .is_some_and(|snapshot| snapshot.validate().is_err())
        {
            return Err(DriverError::Protocol(
                "peer observation returned an invalid UI Snapshot reference".to_owned(),
            )
            .into());
        }
        let expected_ui_omission = match expected_omission {
            Some(ScreenshotOmissionReason::Policy) => Some(UiSnapshotOmissionReason::Policy),
            Some(ScreenshotOmissionReason::ProtectedAction) => {
                Some(UiSnapshotOmissionReason::ProtectedAction)
            }
            None => None,
        };
        if let Some(expected_ui_omission) = expected_ui_omission {
            if observation.ui_snapshot.is_some()
                || observation
                    .ui_snapshot_omission
                    .is_some_and(|reason| reason != expected_ui_omission)
            {
                return Err(DriverError::Protocol(
                    "peer observation violated the UI Snapshot omission contract".to_owned(),
                )
                .into());
            }
        } else if observation.ui_snapshot_omission
            == Some(UiSnapshotOmissionReason::ProtectedAction)
        {
            return Err(DriverError::Protocol(
                "standard peer observation used a protected UI Snapshot omission".to_owned(),
            )
            .into());
        }
        observation.device_id = self.id.clone();
        if let Some(remote) = observation.screenshot.take() {
            let local = self
                .import_asset(context, state, &remote, None, imported)
                .await?;
            observation.screenshot = Some(local);
        }
        if let Some(mut snapshot) = observation.ui_snapshot.take() {
            let remote = snapshot.evidence.clone();
            let local = self
                .import_asset(
                    context,
                    state,
                    &remote,
                    Some((observation.id, &snapshot)),
                    imported,
                )
                .await?;
            snapshot.evidence = local;
            observation.ui_snapshot = Some(snapshot);
        }
        if expected_omission == Some(ScreenshotOmissionReason::ProtectedAction) {
            observation.metadata.clear();
        }
        Ok(observation)
    }

    async fn import_asset(
        &self,
        context: &DriverOperationContext,
        state: &mut DriverState,
        remote: &AssetRef,
        expected_ui_snapshot: Option<(uuid::Uuid, &UiSnapshotRef)>,
        imported: &mut ImportedAssets,
    ) -> DeviceOperationResult<AssetRef> {
        if let Some(existing) = imported.get(&remote.id) {
            if existing.reference.media_type != remote.media_type {
                return Err(DriverError::Protocol(
                    "peer reused an evidence id with a different media type".to_owned(),
                )
                .into());
            }
            if remote
                .sha256
                .as_ref()
                .is_some_and(|remote_sha| existing.reference.sha256.as_ref() != Some(remote_sha))
            {
                return Err(DriverError::Protocol(
                    "peer reused an evidence id with a different digest".to_owned(),
                )
                .into());
            }
            if let Some((observation_id, reference)) = expected_ui_snapshot {
                if reference.byte_length != existing.byte_length {
                    return Err(DriverError::Protocol(
                        "peer UI Snapshot byte length does not match its evidence".to_owned(),
                    )
                    .into());
                }
                let snapshot = existing.ui_snapshot.as_deref().ok_or_else(|| {
                    DriverError::Protocol(
                        "peer reused evidence as a UI Snapshot without a validated body".to_owned(),
                    )
                })?;
                snapshot
                    .validate_against(observation_id, reference)
                    .map_err(|error| {
                        DriverError::Protocol(format!(
                            "peer UI Snapshot body does not match its reference: {error}"
                        ))
                    })?;
            }
            return Ok(existing.reference.clone());
        }
        if !valid_evidence_id(&remote.id)
            || remote.media_type.trim().is_empty()
            || remote.media_type.len() > 128
        {
            return Err(
                DriverError::Protocol("peer evidence reference is invalid".to_owned()).into(),
            );
        }
        let lease = self.ensure_lease(state, context.control()).await?;
        let mut offset = 0_u64;
        let mut bytes = Vec::new();
        let mut expected_total = None;
        let mut expected_media = None;
        let mut expected_sha = None;
        let mut done = false;
        for _ in 0..MAX_EVIDENCE_CHUNKS {
            let result = self
                .request_with_lease(
                    PeerOperation::EvidenceRead {
                        device_key: self.descriptor.device_key.clone(),
                        evidence_id: remote.id.clone(),
                        offset,
                        max_bytes: EVIDENCE_CHUNK_BYTES,
                    },
                    lease.clone(),
                    OperationMethod::EvidenceRead,
                    context.control(),
                    false,
                )
                .await?;
            let PeerResult::EvidenceChunk {
                evidence_id,
                media_type,
                total_size,
                offset: chunk_offset,
                data_base64,
                sha256,
                done: chunk_done,
            } = result
            else {
                return Err(DriverError::Protocol(
                    "peer returned the wrong evidence result".to_owned(),
                )
                .into());
            };
            if evidence_id != remote.id
                || chunk_offset != offset
                || total_size == 0
                || total_size > MAX_EVIDENCE_BYTES as u64
                || media_type != remote.media_type
                || remote.sha256.as_ref().is_some_and(|remote_sha| {
                    sha256
                        .as_ref()
                        .is_some_and(|chunk_sha| chunk_sha != remote_sha)
                })
                || expected_ui_snapshot
                    .is_some_and(|(_, reference)| reference.byte_length != total_size)
                || expected_total.is_some_and(|value| value != total_size)
                || expected_media
                    .as_deref()
                    .is_some_and(|value| value != media_type)
                || expected_sha.as_ref().is_some_and(|value| value != &sha256)
            {
                return Err(DriverError::Protocol(
                    "peer evidence chunk metadata drifted".to_owned(),
                )
                .into());
            }
            let decoded = BASE64.decode(data_base64.as_bytes()).map_err(|_| {
                DriverError::Protocol("peer evidence chunk is not canonical base64".to_owned())
            })?;
            if BASE64.encode(&decoded) != data_base64
                || decoded.is_empty()
                || decoded.len() > EVIDENCE_CHUNK_BYTES as usize
                || bytes.len().saturating_add(decoded.len()) > total_size as usize
            {
                return Err(DriverError::Protocol(
                    "peer evidence chunk exceeds its bounds".to_owned(),
                )
                .into());
            }
            expected_total = Some(total_size);
            expected_media = Some(media_type);
            expected_sha = Some(sha256);
            offset = offset.saturating_add(decoded.len() as u64);
            bytes.extend_from_slice(&decoded);
            if chunk_done {
                if offset != total_size {
                    return Err(DriverError::Protocol(
                        "peer evidence ended at the wrong offset".to_owned(),
                    )
                    .into());
                }
                done = true;
                break;
            }
        }
        if !done {
            return Err(platform("remote_evidence_chunk_limit", false).into());
        }
        let total = expected_total
            .ok_or_else(|| DriverError::Protocol("peer returned no evidence chunks".to_owned()))?;
        let ui_snapshot = if let Some((observation_id, reference)) = expected_ui_snapshot {
            let snapshot = serde_json::from_slice::<UiSnapshot>(&bytes).map_err(|error| {
                DriverError::Protocol(format!(
                    "peer UI Snapshot body is not a canonical UI Tree: {error}"
                ))
            })?;
            snapshot
                .validate_against(observation_id, reference)
                .map_err(|error| {
                    DriverError::Protocol(format!(
                        "peer UI Snapshot body does not match its reference: {error}"
                    ))
                })?;
            Some(Arc::new(snapshot))
        } else {
            None
        };
        let stored = context
            .evidence()
            .put_with_declared_size(&remote.media_type, total, Box::pin(Cursor::new(bytes)))
            .await?;
        let local = stored.asset_ref();
        let chunk_sha = expected_sha.as_ref().and_then(|value| value.as_ref());
        if [remote.sha256.as_ref(), chunk_sha]
            .into_iter()
            .flatten()
            .any(|claimed_sha| local.sha256.as_ref() != Some(claimed_sha))
        {
            return Err(DriverError::Protocol(
                "peer evidence digest does not match its bytes".to_owned(),
            )
            .into());
        }
        imported.insert(
            remote.id.clone(),
            ImportedAsset {
                reference: local.clone(),
                byte_length: total,
                ui_snapshot,
            },
        );
        Ok(local)
    }
}

#[async_trait]
impl DeviceDriver for RemoteDeviceDriver {
    fn id(&self) -> &DeviceId {
        &self.id
    }

    async fn connect(&self, control: &ExecutionControl) -> DriverResult<DeviceInfo> {
        ensure_active(control)?;
        let mut state = self.state.lock().await;
        if state.connected {
            let _ = self.ensure_lease(&mut state, control).await?;
            return Ok(self.info(true));
        }
        let lease = self.ensure_lease(&mut state, control).await?;
        let result = self
            .request_with_lease(
                PeerOperation::Connect {
                    device_key: self.descriptor.device_key.clone(),
                },
                lease,
                OperationMethod::Connect,
                control,
                true,
            )
            .await?;
        let PeerResult::Device { device } = result else {
            return Err(DriverError::Protocol(
                "peer returned the wrong connect result".to_owned(),
            ));
        };
        if device.id != DeviceId::new(self.descriptor.device_key.clone()) && device.id != self.id {
            return Err(DriverError::Protocol(
                "peer connected another device".to_owned(),
            ));
        }
        if !device.connected {
            return Err(DriverError::Protocol(
                "peer connect result is disconnected".to_owned(),
            ));
        }
        state.connected = true;
        Ok(self.info(true))
    }

    async fn disconnect(&self, control: &ExecutionControl) -> DriverResult<()> {
        ensure_active(control)?;
        let mut state = self.state.lock().await;
        let Some(lease) = state.lease.clone() else {
            state.connected = false;
            return Ok(());
        };
        if state.connected {
            let result = self
                .request_with_lease(
                    PeerOperation::Disconnect {
                        device_key: self.descriptor.device_key.clone(),
                    },
                    lease.clone(),
                    OperationMethod::Disconnect,
                    control,
                    true,
                )
                .await?;
            expect_ack(result)?;
            state.connected = false;
        }
        let mut request = PeerRequest::new(
            self.node_id.clone(),
            Some(self.node_epoch),
            PeerOperation::LeaseRelease {
                lease: lease.clone(),
            },
        );
        request.lease = Some(lease);
        let result = self
            .exchange(request, OperationMethod::LeaseRelease, control, false)
            .await;
        // Whether release succeeds or the connection dies, this driver must
        // never reuse the token. The node-side TTL is the failure backstop.
        state.lease = None;
        state.lease_received_at = None;
        expect_ack(result?)
    }

    async fn capabilities(
        &self,
        control: &ExecutionControl,
    ) -> DriverResult<Vec<ActionDefinition>> {
        ensure_active(control)?;
        Ok(self.capabilities.as_ref().clone())
    }

    async fn health_check(&self, control: &ExecutionControl) -> DriverResult<()> {
        ensure_active(control)?;
        let request = PeerRequest::new(
            self.node_id.clone(),
            Some(self.node_epoch),
            PeerOperation::Health,
        );
        let result = self
            .exchange(request, OperationMethod::Health, control, false)
            .await?;
        match result {
            PeerResult::Health {
                state: crate::HealthState::Healthy | crate::HealthState::Degraded,
                checked_at_ms,
            } if checked_at_ms > 0
                && checked_at_ms <= MAX_SAFE_INTEGER
                && now_ms().saturating_sub(checked_at_ms) <= HEALTH_TIMESTAMP_MAX_AGE_MS
                && checked_at_ms <= now_ms().saturating_add(HEALTH_TIMESTAMP_MAX_SKEW_MS) =>
            {
                Ok(())
            }
            PeerResult::Health { .. } => Err(platform("remote_node_unhealthy", true)),
            _ => Err(DriverError::Protocol(
                "peer returned the wrong health result".to_owned(),
            )),
        }
    }

    fn action_protection(&self, name: &str) -> Option<ActionProtection> {
        self.protection.get(name).copied()
    }

    async fn observe(
        &self,
        context: &DriverOperationContext,
    ) -> DeviceOperationResult<Observation> {
        ensure_active(context.control())?;
        let mut state = self.state.lock().await;
        if !state.connected {
            return Err(DriverError::NotConnected(self.id.clone()).into());
        }
        let lease = self.ensure_lease(&mut state, context.control()).await?;
        let omission = match context.screenshot_policy() {
            ScreenshotPolicy::Capture => None,
            ScreenshotPolicy::Omit => Some(ScreenshotOmissionReason::Policy),
        };
        let result = self
            .request_with_lease(
                PeerOperation::Observe {
                    device_key: self.descriptor.device_key.clone(),
                    screenshot_omission: omission,
                    ui_snapshots_enabled: context.ui_snapshots_enabled(),
                    semantic_actions_enabled: context.semantic_actions_enabled(),
                },
                lease,
                OperationMethod::Observe,
                context.control(),
                false,
            )
            .await?;
        let PeerResult::Observation { observation } = result else {
            return Err(DriverError::Protocol(
                "peer returned the wrong observation result".to_owned(),
            )
            .into());
        };
        self.import_observation(
            context,
            &mut state,
            *observation,
            omission,
            &mut BTreeMap::new(),
        )
        .await
    }

    async fn execute(
        &self,
        context: &DriverOperationContext,
        call: ActionCall,
    ) -> DeviceOperationResult<ActionResult> {
        ensure_active(context.control())?;
        let mut state = self.state.lock().await;
        if !state.connected {
            return Err(DriverError::NotConnected(self.id.clone()).into());
        }
        let definition = self
            .capabilities
            .iter()
            .find(|definition| definition.name == call.name)
            .ok_or_else(|| DriverError::UnknownAction(call.name.clone()))?;
        let validator = self.validators.get(&call.name).ok_or_else(|| {
            DriverError::Protocol("cached peer capability schema is missing".to_owned())
        })?;
        if !validator.is_valid(&call.arguments) {
            return Err(invalid_arguments(
                &call.name,
                "arguments do not satisfy the advertised schema",
            )
            .into());
        }
        validate_semantic_arguments(&call.name, &call.arguments)?;
        let omission = if definition.protection == ActionProtection::Protected {
            Some(ScreenshotOmissionReason::ProtectedAction)
        } else {
            match context.screenshot_policy() {
                ScreenshotPolicy::Capture => None,
                ScreenshotPolicy::Omit => Some(ScreenshotOmissionReason::Policy),
            }
        };
        let lease = self.ensure_lease(&mut state, context.control()).await?;
        let call_id = call.id;
        let result = self
            .request_with_lease(
                PeerOperation::Execute {
                    device_key: self.descriptor.device_key.clone(),
                    call,
                    screenshot_omission: omission,
                    ui_snapshots_enabled: context.ui_snapshots_enabled(),
                    semantic_actions_enabled: context.semantic_actions_enabled(),
                },
                lease,
                OperationMethod::Execute,
                context.control(),
                true,
            )
            .await?;
        let PeerResult::Action { result } = result else {
            return Err(
                DriverError::Protocol("peer returned the wrong action result".to_owned()).into(),
            );
        };
        let mut result = *result;
        if result.call_id != call_id
            || result.started_at_ms == 0
            || result.finished_at_ms < result.started_at_ms
            || result.after.is_none()
            || serde_json::to_vec(&result.output)
                .map_or(true, |bytes| bytes.len() > MAX_ACTION_OUTPUT_BYTES)
        {
            return Err(DriverError::Protocol("peer action result is invalid".to_owned()).into());
        }
        if definition.protection == ActionProtection::Protected && result.before.is_none() {
            return Err(DriverError::Protocol(
                "protected peer action omitted its before observation".to_owned(),
            )
            .into());
        }
        if omission.is_some() && !result.evidence.is_empty() {
            return Err(DriverError::Protocol(
                "peer action returned evidence despite omission".to_owned(),
            )
            .into());
        }
        let mut imported = BTreeMap::new();
        if let Some(before) = result.before.take() {
            result.before = Some(
                self.import_observation(context, &mut state, before, omission, &mut imported)
                    .await?,
            );
        }
        let after = result.after.take().expect("validated action result after");
        result.after = Some(
            self.import_observation(context, &mut state, after, omission, &mut imported)
                .await?,
        );
        validate_semantic_action_result(definition, &result, omission, &imported)?;
        let mut evidence = Vec::new();
        let mut ids = BTreeSet::new();
        for remote in std::mem::take(&mut result.evidence) {
            let local = self
                .import_asset(context, &mut state, &remote, None, &mut imported)
                .await?;
            if ids.insert(local.id.clone()) {
                evidence.push(local);
            }
        }
        if omission.is_none() && evidence.is_empty() {
            return Err(DriverError::Protocol(
                "peer action result omitted required evidence".to_owned(),
            )
            .into());
        }
        if omission.is_some() && !evidence.is_empty() {
            return Err(DriverError::Protocol(
                "peer action returned evidence despite omission".to_owned(),
            )
            .into());
        }
        result.evidence = evidence;
        if definition.protection == ActionProtection::Protected {
            result.output = serde_json::json!({"accepted": true});
        }
        ensure_active(context.control())?;
        Ok(result)
    }
}

fn validate_capabilities(
    actions: &[ActionDefinition],
) -> DriverResult<(
    BTreeMap<String, ActionProtection>,
    BTreeMap<String, jsonschema::Validator>,
)> {
    if actions.is_empty() || actions.len() > MAX_CAPABILITIES {
        return Err(platform("remote_capability_limit", false));
    }
    let mut protection = BTreeMap::new();
    let mut validators = BTreeMap::new();
    for action in actions {
        let schema_bytes = serde_json::to_vec(&action.input_schema).map_err(|_| {
            DriverError::Protocol("peer capability declaration is invalid".to_owned())
        })?;
        if action.name.trim().is_empty()
            || action.name.len() > 128
            || action.description.trim().is_empty()
            || action.description.len() > 1_024
            || action.input_schema.get("type") != Some(&Value::String("object".to_owned()))
            || schema_bytes.len() > 64 * 1024
            || protection.contains_key(&action.name)
        {
            return Err(DriverError::Protocol(
                "peer capability declaration is invalid".to_owned(),
            ));
        }
        if let Some(source) = canonical_semantic_schema(&action.name) {
            let canonical = serde_json::from_str::<Value>(source).map_err(|error| {
                DriverError::Protocol(format!(
                    "canonical semantic schema for `{}` is invalid: {error}",
                    action.name
                ))
            })?;
            if action.input_schema != canonical {
                return Err(DriverError::Protocol(format!(
                    "peer semantic capability `{}` does not advertise the canonical input schema",
                    action.name
                )));
            }
        }
        let validator = jsonschema::validator_for(&action.input_schema).map_err(|_| {
            DriverError::Protocol("peer capability declaration is invalid".to_owned())
        })?;
        protection.insert(action.name.clone(), action.protection);
        validators.insert(action.name.clone(), validator);
    }
    Ok((protection, validators))
}

fn canonical_semantic_schema(name: &str) -> Option<&'static str> {
    match name {
        FIND_ELEMENT_ACTION => Some(include_str!(
            "../../../protocol/schema/v1/find-element-arguments.schema.json"
        )),
        TAP_ELEMENT_ACTION => Some(include_str!(
            "../../../protocol/schema/v1/tap-element-arguments.schema.json"
        )),
        CLEAR_ELEMENT_ACTION => Some(include_str!(
            "../../../protocol/schema/v1/clear-element-arguments.schema.json"
        )),
        SET_ELEMENT_VALUE_ACTION => Some(include_str!(
            "../../../protocol/schema/v1/set-element-value-arguments.schema.json"
        )),
        WAIT_FOR_ELEMENT_ACTION => Some(include_str!(
            "../../../protocol/schema/v1/wait-for-element-arguments.schema.json"
        )),
        _ => None,
    }
}

fn validate_semantic_arguments(name: &str, arguments: &Value) -> DriverResult<()> {
    let valid = match name {
        FIND_ELEMENT_ACTION => serde_json::from_value::<FindElementArguments>(arguments.clone())
            .is_ok_and(|arguments| arguments.validate().is_ok()),
        TAP_ELEMENT_ACTION => serde_json::from_value::<TapElementArguments>(arguments.clone())
            .is_ok_and(|arguments| arguments.validate().is_ok()),
        CLEAR_ELEMENT_ACTION => serde_json::from_value::<ClearElementArguments>(arguments.clone())
            .is_ok_and(|arguments| arguments.validate().is_ok()),
        SET_ELEMENT_VALUE_ACTION => {
            serde_json::from_value::<SetElementValueArguments>(arguments.clone())
                .is_ok_and(|arguments| arguments.validate().is_ok())
        }
        WAIT_FOR_ELEMENT_ACTION => {
            serde_json::from_value::<WaitForElementArguments>(arguments.clone())
                .is_ok_and(|arguments| arguments.validate().is_ok())
        }
        _ => return Ok(()),
    };
    if valid {
        Ok(())
    } else {
        Err(invalid_arguments(
            name,
            "arguments violate the canonical semantic contract",
        ))
    }
}

fn invalid_arguments(action: &str, message: &str) -> DriverError {
    const MAX_ACTION_CHARS: usize = 128;
    const MAX_MESSAGE_CHARS: usize = 256;
    let bounded_action = action.chars().take(MAX_ACTION_CHARS).collect::<String>();
    let bounded_message = message.chars().take(MAX_MESSAGE_CHARS).collect::<String>();
    DriverError::InvalidArguments {
        action: if bounded_action.trim().is_empty() {
            "unknown_action".to_owned()
        } else {
            bounded_action
        },
        message: if bounded_message.trim().is_empty() {
            "arguments were rejected".to_owned()
        } else {
            bounded_message
        },
    }
}

fn validate_semantic_action_result(
    definition: &ActionDefinition,
    result: &ActionResult,
    expected_omission: Option<ScreenshotOmissionReason>,
    imported: &ImportedAssets,
) -> DeviceOperationResult<()> {
    if !is_semantic_action_name(&definition.name) {
        if result.execution.is_some() {
            return Err(DriverError::Protocol(format!(
                "non-semantic peer action `{}` returned execution metadata",
                definition.name
            ))
            .into());
        }
        return Ok(());
    }

    let execution = result.execution.as_ref().ok_or_else(|| {
        DriverError::Protocol(format!(
            "semantic peer action `{}` omitted execution metadata",
            definition.name
        ))
    })?;
    execution.validate().map_err(|error| {
        DriverError::Protocol(format!(
            "semantic peer action `{}` returned invalid execution metadata: {error}",
            definition.name
        ))
    })?;
    let execution_context = execution_context(execution);
    let node = semantic_result_node(definition, result)?;

    let observations = result.before.iter().chain(result.after.iter());
    let has_execution_context = observations.clone().any(|observation| {
        observation
            .ui_snapshot
            .as_ref()
            .is_some_and(|snapshot| snapshot.context == *execution_context)
            || (expected_omission.is_some()
                && node
                    .as_ref()
                    .is_none_or(|node| node.observation_id == observation.id))
    });
    if !has_execution_context {
        return Err(DriverError::Protocol(format!(
            "semantic peer action `{}` returned no observation for its execution context",
            definition.name
        ))
        .into());
    }

    if let Some(node) = node {
        validate_semantic_node_ref(
            definition,
            &node,
            execution_context,
            result,
            expected_omission,
            imported,
        )?;
    }
    Ok(())
}

fn semantic_result_node(
    definition: &ActionDefinition,
    result: &ActionResult,
) -> DeviceOperationResult<Option<UiNodeRef>> {
    let parse_error = |error: serde_json::Error| {
        DriverError::Protocol(format!(
            "semantic peer action `{}` returned a non-canonical output DTO: {error}",
            definition.name
        ))
    };
    let node = match definition.name.as_str() {
        FIND_ELEMENT_ACTION => Some(
            serde_json::from_value::<FindElementResult>(result.output.clone())
                .map_err(parse_error)?
                .element,
        ),
        TAP_ELEMENT_ACTION | CLEAR_ELEMENT_ACTION | SET_ELEMENT_VALUE_ACTION => Some(
            serde_json::from_value::<ElementActionOutput>(result.output.clone())
                .map_err(parse_error)?
                .element,
        ),
        WAIT_FOR_ELEMENT_ACTION => {
            let output = serde_json::from_value::<WaitForElementResult>(result.output.clone())
                .map_err(parse_error)?;
            output.validate().map_err(|error| {
                DriverError::Protocol(format!(
                    "semantic peer action `{}` returned an invalid wait result: {error}",
                    definition.name
                ))
            })?;
            output.element
        }
        _ => unreachable!("semantic action name was checked"),
    };
    Ok(node)
}

fn execution_context(execution: &ActionExecution) -> &UiContextRef {
    match execution {
        ActionExecution::NativeSemantic { context }
        | ActionExecution::WebSemantic { context }
        | ActionExecution::CoordinateFallback { context, .. } => context,
    }
}

fn validate_semantic_node_ref(
    definition: &ActionDefinition,
    node: &UiNodeRef,
    execution_context: &UiContextRef,
    result: &ActionResult,
    expected_omission: Option<ScreenshotOmissionReason>,
    imported: &ImportedAssets,
) -> DeviceOperationResult<()> {
    node.validate().map_err(|error| {
        DriverError::Protocol(format!(
            "semantic peer action `{}` returned an invalid node reference: {error}",
            definition.name
        ))
    })?;
    if &node.context != execution_context {
        return Err(DriverError::Protocol(format!(
            "semantic peer action `{}` returned a node from another execution context",
            definition.name
        ))
        .into());
    }
    let source = result
        .before
        .iter()
        .chain(result.after.iter())
        .find(|observation| observation.id == node.observation_id)
        .ok_or_else(|| {
            DriverError::Protocol(format!(
                "semantic peer action `{}` returned a node from an absent observation",
                definition.name
            ))
        })?;
    if expected_omission.is_none() {
        let reference = source.ui_snapshot.as_ref().ok_or_else(|| {
            DriverError::Protocol(format!(
                "semantic peer action `{}` returned a node without UI Snapshot evidence",
                definition.name
            ))
        })?;
        if reference.context != node.context {
            return Err(DriverError::Protocol(format!(
                "semantic peer action `{}` linked a node to a different snapshot context",
                definition.name
            ))
            .into());
        }
        let snapshot = imported
            .values()
            .filter_map(|asset| {
                asset
                    .ui_snapshot
                    .as_deref()
                    .map(|snapshot| (asset, snapshot))
            })
            .find(|(asset, snapshot)| {
                asset.reference.id == reference.evidence.id
                    && snapshot.observation_id == source.id
                    && snapshot.context == reference.context
            })
            .map(|(_, snapshot)| snapshot)
            .ok_or_else(|| {
                DriverError::Protocol(format!(
                    "semantic peer action `{}` linked a node to an unvalidated UI Snapshot",
                    definition.name
                ))
            })?;
        if !snapshot
            .nodes
            .iter()
            .any(|candidate| candidate.stable_node_id == node.stable_node_id)
        {
            return Err(DriverError::Protocol(format!(
                "semantic peer action `{}` returned a node absent from its UI Snapshot",
                definition.name
            ))
            .into());
        }
    }
    Ok(())
}

async fn exchange_raw(
    transport: &Arc<dyn PeerTransport>,
    telemetry_sink: &Arc<dyn TelemetrySink>,
    request: PeerRequest,
    method: OperationMethod,
    control: &ExecutionControl,
    mutation: bool,
) -> DriverResult<PeerResult> {
    let started = Instant::now();
    let trace_id = request.trace_id;
    let node = request.node_id.clone();
    let request_copy = request.clone();
    let exchange = transport.request(request, control).await;
    let (outcome, result) = match exchange {
        Ok(response) => match response_to_result(response, &request_copy) {
            Ok(result) => (OperationOutcome::Success, Ok(result)),
            Err(error) => {
                let outcome = match &error {
                    DriverError::Platform { code, .. } if code.contains("outcome_unknown") => {
                        OperationOutcome::OutcomeUnknown
                    }
                    DriverError::Protocol(_) => OperationOutcome::ProtocolError,
                    _ => OperationOutcome::RemoteError,
                };
                (outcome, Err(error))
            }
        },
        Err(TransportError::Cancelled) => {
            (OperationOutcome::Cancelled, Err(DriverError::Cancelled))
        }
        Err(TransportError::TimedOut) => (OperationOutcome::TimedOut, Err(DriverError::TimedOut)),
        Err(error @ (TransportError::CancelledAfterSend | TransportError::TimedOutAfterSend))
            if mutation =>
        {
            let code = if method == OperationMethod::Execute {
                "remote_execute_outcome_unknown"
            } else {
                "remote_lifecycle_outcome_unknown"
            };
            let _ = error;
            (OperationOutcome::OutcomeUnknown, Err(platform(code, false)))
        }
        Err(TransportError::CancelledAfterSend) => {
            (OperationOutcome::Cancelled, Err(DriverError::Cancelled))
        }
        Err(TransportError::TimedOutAfterSend) => {
            (OperationOutcome::TimedOut, Err(DriverError::TimedOut))
        }
        Err(
            error @ (TransportError::Protocol
            | TransportError::InvalidRequest
            | TransportError::UnsupportedVersion),
        ) => (
            OperationOutcome::ProtocolError,
            Err(DriverError::Protocol(error.code().to_owned())),
        ),
        Err(error) if mutation && error.may_have_reached_peer() => {
            let code = if method == OperationMethod::Execute {
                "remote_execute_outcome_unknown"
            } else {
                "remote_lifecycle_outcome_unknown"
            };
            (OperationOutcome::OutcomeUnknown, Err(platform(code, false)))
        }
        Err(error) => (
            OperationOutcome::TransportError,
            Err(platform(error.code(), true)),
        ),
    };
    telemetry::record(
        telemetry_sink.as_ref(),
        trace_id,
        &node,
        method,
        outcome,
        started,
    );
    result
}

fn response_to_result(response: PeerResponse, request: &PeerRequest) -> DriverResult<PeerResult> {
    response
        .validate_for(request)
        .map_err(|_| DriverError::Protocol("peer response mismatch".to_owned()))?;
    if response.ok {
        response
            .result
            .ok_or_else(|| DriverError::Protocol("peer response has no result".to_owned()))
    } else {
        let error = response
            .error
            .ok_or_else(|| DriverError::Protocol("peer response has no error".to_owned()))?;
        if !error.outcome_unknown
            && error.code == "invalid_arguments"
            && let PeerOperation::Execute { call, .. } = &request.operation
        {
            return Err(invalid_arguments(
                &call.name,
                "remote peer rejected the action arguments",
            ));
        }
        if !error.outcome_unknown
            && let Some(error) = semantic_driver_error(&error.code)
        {
            return Err(error);
        }
        let code = if error.outcome_unknown {
            match request.operation {
                PeerOperation::Execute { .. } => "remote_execute_outcome_unknown".to_owned(),
                PeerOperation::Connect { .. } | PeerOperation::Disconnect { .. } => {
                    "remote_lifecycle_outcome_unknown".to_owned()
                }
                _ => "remote_outcome_unknown".to_owned(),
            }
        } else {
            let mut code = format!("remote_{}", error.code);
            code.truncate(64);
            code
        };
        Err(platform(
            &code,
            if error.outcome_unknown {
                false
            } else {
                error.retryable
            },
        ))
    }
}

fn semantic_driver_error(code: &str) -> Option<DriverError> {
    match code {
        "element_not_found" => Some(DriverError::ElementNotFound),
        "element_ambiguous" => Some(DriverError::ElementAmbiguous),
        "element_stale" => Some(DriverError::ElementStale),
        "element_not_interactable" => Some(DriverError::ElementNotInteractable),
        "ui_context_not_found" => Some(DriverError::UiContextNotFound),
        "ui_context_ambiguous" => Some(DriverError::UiContextAmbiguous),
        "ui_context_changed" => Some(DriverError::UiContextChanged),
        "semantic_channel_unavailable" => Some(DriverError::SemanticChannelUnavailable),
        _ => None,
    }
}

fn expect_ack(result: PeerResult) -> DriverResult<()> {
    if result == PeerResult::Ack {
        Ok(())
    } else {
        Err(DriverError::Protocol(
            "peer returned the wrong acknowledgement".to_owned(),
        ))
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

fn platform(code: &str, retryable: bool) -> DriverError {
    DriverError::Platform {
        code: code.to_owned(),
        retryable,
    }
}

#[cfg(test)]
mod tests {
    use devicerail_protocol::{ActionCall, ActionDefinition, ActionProtection};
    use serde_json::json;
    use uuid::Uuid;

    use super::{
        invalid_arguments, response_to_result, semantic_driver_error, validate_capabilities,
    };
    use crate::{NodeId, PeerError, PeerOperation, PeerRequest, PeerResponse};

    #[test]
    fn semantic_peer_errors_keep_the_cross_platform_driver_taxonomy() {
        for (code, expected) in [
            ("element_not_found", "element_not_found"),
            ("element_ambiguous", "element_ambiguous"),
            ("element_stale", "element_stale"),
            ("element_not_interactable", "element_not_interactable"),
            ("ui_context_not_found", "ui_context_not_found"),
            ("ui_context_ambiguous", "ui_context_ambiguous"),
            ("ui_context_changed", "ui_context_changed"),
            (
                "semantic_channel_unavailable",
                "semantic_channel_unavailable",
            ),
        ] {
            assert_eq!(
                semantic_driver_error(code)
                    .expect("semantic error")
                    .to_error_info()
                    .code,
                expected
            );
        }
        assert!(semantic_driver_error("future_error").is_none());
    }

    #[test]
    fn peer_invalid_arguments_preserves_bounded_driver_taxonomy() {
        let node = NodeId::parse("peer-invalid-arguments").expect("node");
        let request = PeerRequest::new(
            node,
            Some(7),
            PeerOperation::Execute {
                device_key: "device-1".to_owned(),
                call: ActionCall {
                    id: Uuid::new_v4(),
                    name: "tapElement".to_owned(),
                    arguments: json!({"target": {"kind": "selector", "selector": {}}}),
                },
                screenshot_omission: None,
                ui_snapshots_enabled: true,
                semantic_actions_enabled: true,
            },
        );
        let response = PeerResponse::failure(
            &request,
            7,
            PeerError {
                code: "invalid_arguments".to_owned(),
                retryable: false,
                outcome_unknown: false,
            },
        );
        let error = response_to_result(response, &request).expect_err("peer rejection");
        let devicerail_core::DriverError::InvalidArguments { action, message } = error else {
            panic!("expected InvalidArguments taxonomy");
        };
        assert_eq!(action, "tapElement");
        assert!(!message.is_empty());
        assert!(action.chars().count() <= 128);
        assert!(message.chars().count() <= 256);

        let devicerail_core::DriverError::InvalidArguments { action, message } =
            invalid_arguments(&"a".repeat(1_024), &"m".repeat(1_024))
        else {
            unreachable!();
        };
        assert_eq!(action.chars().count(), 128);
        assert_eq!(message.chars().count(), 256);
    }

    #[test]
    fn capability_validation_builds_one_reusable_validator_per_action() {
        let actions = vec![ActionDefinition {
            name: "tap".to_owned(),
            description: "Tap one point".to_owned(),
            protection: ActionProtection::Standard,
            input_schema: json!({
                "$schema": "https://json-schema.org/draft/2020-12/schema",
                "type": "object",
                "additionalProperties": false,
                "required": ["x"],
                "properties": { "x": { "type": "integer", "minimum": 0 } }
            }),
        }];

        let (_, validators) = validate_capabilities(&actions).expect("valid capability contract");
        assert_eq!(validators.len(), 1);
        let validator = validators.get("tap").expect("cached tap validator");
        assert!(validator.is_valid(&json!({ "x": 1 })));
        assert!(validator.is_valid(&json!({ "x": 2 })));
        assert!(!validator.is_valid(&json!({ "x": -1 })));
    }
}
