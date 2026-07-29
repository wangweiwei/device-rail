use std::{collections::BTreeMap, sync::Arc, time::Instant};

use devicerail_core::ExecutionControl;
use devicerail_protocol::{DeviceId, DeviceInfo};
use futures_util::future::join_all;
use thiserror::Error;
use tokio::sync::RwLock;

use crate::{
    HealthState, InventorySnapshot, MemoryTelemetry, NodeId, OperationMethod, OperationOutcome,
    PeerOperation, PeerProtocolCapabilities, PeerRequest, PeerResult, PeerTransport,
    RemoteDeviceDescriptor, RemoteDeviceDriver, RemoteDriverConfig, ShardedPeerTransport,
    TelemetrySink, TransportError,
    model::{MAX_SAFE_INTEGER, ModelError},
    telemetry,
};

const DEFAULT_MAX_NODES: usize = 32;
const ABSOLUTE_MAX_NODES: usize = 64;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RouterConfig {
    max_nodes: usize,
    max_inventory_age_ms: u64,
    max_health_age_ms: u64,
}

impl RouterConfig {
    pub fn new(
        max_nodes: usize,
        max_inventory_age_ms: u64,
        max_health_age_ms: u64,
        max_clock_skew_ms: u64,
    ) -> Result<Self, RouteError> {
        if max_nodes == 0
            || max_nodes > ABSOLUTE_MAX_NODES
            || !(1_000..=5 * 60_000).contains(&max_inventory_age_ms)
            || !(1_000..=5 * 60_000).contains(&max_health_age_ms)
            || max_clock_skew_ms > 60_000
        {
            return Err(RouteError::InvalidConfiguration);
        }
        Ok(Self {
            max_nodes,
            max_inventory_age_ms,
            max_health_age_ms,
        })
    }
}

impl Default for RouterConfig {
    fn default() -> Self {
        Self {
            max_nodes: DEFAULT_MAX_NODES,
            max_inventory_age_ms: 30_000,
            max_health_age_ms: 10_000,
        }
    }
}

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum RouteError {
    #[error("distributed router configuration is invalid")]
    InvalidConfiguration,
    #[error("distributed node transport is unauthenticated")]
    Unauthenticated,
    #[error("distributed node returned an invalid protocol response")]
    Protocol,
    #[error("distributed node protocol version is unsupported")]
    UnsupportedVersion,
    #[error("distributed node lacks required UI routing capabilities")]
    UnsupportedCapabilities,
    #[error("distributed node transport failed: {0}")]
    Transport(TransportError),
    #[error("distributed node operation failed: {code}")]
    Remote { code: String, outcome_unknown: bool },
    #[error("distributed node id or inventory is duplicated")]
    Duplicate,
    #[error("distributed router node limit reached")]
    NodeLimit,
    #[error("distributed node epoch is stale or replayed")]
    StaleEpoch,
    #[error("distributed node inventory is stale or replayed")]
    StaleInventory,
    #[error("distributed device identity drifted within a stable namespace")]
    IdentityDrift,
    #[error("distributed node health is stale or unavailable")]
    Unhealthy,
    #[error("distributed route was not found")]
    NotFound,
}

impl From<TransportError> for RouteError {
    fn from(value: TransportError) -> Self {
        Self::Transport(value)
    }
}

impl From<ModelError> for RouteError {
    fn from(error: ModelError) -> Self {
        match error {
            ModelError::UnsupportedVersion => Self::UnsupportedVersion,
            _ => Self::Protocol,
        }
    }
}

#[derive(Clone, Debug)]
struct HealthSnapshot {
    state: HealthState,
    received_at: Instant,
}

/// One discovered remote node over a reusable authenticated transport.
pub struct RemoteNode {
    node_id: NodeId,
    epoch: u64,
    inventory: InventorySnapshot,
    inventory_received_at: Instant,
    health: HealthSnapshot,
    protocol_capabilities: PeerProtocolCapabilities,
    transport: Arc<dyn PeerTransport>,
    telemetry: Arc<dyn TelemetrySink>,
}

impl std::fmt::Debug for RemoteNode {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("RemoteNode")
            .field("node_id", &self.node_id)
            .field("epoch", &self.epoch)
            .field("inventory_revision", &self.inventory.revision)
            .field("device_count", &self.inventory.devices.len())
            .field("health", &self.health.state)
            .field("security", self.transport.security())
            .finish()
    }
}

impl RemoteNode {
    pub(crate) fn with_transport_shards(nodes: Vec<Arc<Self>>) -> Result<Arc<Self>, RouteError> {
        let primary = nodes.first().ok_or(RouteError::InvalidConfiguration)?;
        if nodes.len() == 1 {
            return Ok(Arc::clone(primary));
        }
        for node in nodes.iter().skip(1) {
            if node.node_id != primary.node_id
                || node.epoch != primary.epoch
                || node.inventory.revision != primary.inventory.revision
                || node.inventory.devices != primary.inventory.devices
                || node.health.state != primary.health.state
                || node.protocol_capabilities != primary.protocol_capabilities
                || node.transport.security() != primary.transport.security()
            {
                return Err(RouteError::IdentityDrift);
            }
        }
        let transports = nodes
            .iter()
            .map(|node| Arc::clone(&node.transport))
            .collect::<Vec<_>>();
        let device_keys = primary
            .inventory
            .devices
            .iter()
            .map(|device| device.device_key.clone())
            .collect::<Vec<_>>();
        let transport: Arc<dyn PeerTransport> =
            ShardedPeerTransport::new(transports, device_keys).map_err(RouteError::Transport)?;
        Ok(Arc::new(Self {
            node_id: primary.node_id.clone(),
            epoch: primary.epoch,
            inventory: primary.inventory.clone(),
            inventory_received_at: primary.inventory_received_at,
            health: primary.health.clone(),
            protocol_capabilities: primary.protocol_capabilities,
            transport,
            telemetry: Arc::clone(&primary.telemetry),
        }))
    }

    pub async fn discover(
        transport: Arc<dyn PeerTransport>,
        telemetry: Option<Arc<dyn TelemetrySink>>,
        _config: RouterConfig,
        control: &ExecutionControl,
    ) -> Result<Arc<Self>, RouteError> {
        // The trait deliberately has no unauthenticated constructor. Still
        // inspect the attestation so implementations cannot hide an empty
        // subject behind a custom transport.
        if transport.security().subject().is_empty() {
            return Err(RouteError::Unauthenticated);
        }
        let telemetry = telemetry
            .unwrap_or_else(|| Arc::new(MemoryTelemetry::default()) as Arc<dyn TelemetrySink>);
        let node_id = transport.expected_node_id().clone();
        let hello = exchange(
            &transport,
            &telemetry,
            PeerRequest::new(node_id.clone(), None, PeerOperation::Hello),
            OperationMethod::Hello,
            control,
        )
        .await?;
        let PeerResult::Hello {
            node_id: hello_id,
            epoch,
            max_frame_bytes,
            capabilities,
        } = success(hello)?
        else {
            return Err(RouteError::Protocol);
        };
        if hello_id != node_id
            || epoch == 0
            || epoch > MAX_SAFE_INTEGER
            || max_frame_bytes == 0
            || max_frame_bytes as usize > crate::MAX_PEER_FRAME_BYTES
        {
            return Err(RouteError::Protocol);
        }
        if !capabilities.supports_required() {
            return Err(RouteError::UnsupportedCapabilities);
        }

        let inventory_response = exchange(
            &transport,
            &telemetry,
            PeerRequest::new(node_id.clone(), None, PeerOperation::Inventory),
            OperationMethod::Inventory,
            control,
        )
        .await?;
        if inventory_response.node_epoch != epoch {
            return Err(RouteError::StaleEpoch);
        }
        let PeerResult::Inventory { inventory } = success(inventory_response)? else {
            return Err(RouteError::Protocol);
        };
        inventory.validate()?;
        if inventory.node_id != node_id || inventory.epoch != epoch {
            return Err(RouteError::Protocol);
        }
        let inventory_received_at = Instant::now();

        let health_response = exchange(
            &transport,
            &telemetry,
            PeerRequest::new(node_id.clone(), Some(epoch), PeerOperation::Health),
            OperationMethod::Health,
            control,
        )
        .await?;
        let PeerResult::Health {
            state,
            checked_at_ms,
        } = success(health_response)?
        else {
            return Err(RouteError::Protocol);
        };
        if checked_at_ms == 0 || checked_at_ms > MAX_SAFE_INTEGER {
            return Err(RouteError::Protocol);
        }
        if state == HealthState::Unavailable {
            return Err(RouteError::Unhealthy);
        }
        Ok(Arc::new(Self {
            node_id,
            epoch,
            inventory,
            inventory_received_at,
            health: HealthSnapshot {
                state,
                received_at: Instant::now(),
            },
            protocol_capabilities: capabilities,
            transport,
            telemetry,
        }))
    }

    pub fn node_id(&self) -> &NodeId {
        &self.node_id
    }

    pub fn epoch(&self) -> u64 {
        self.epoch
    }

    pub fn inventory(&self) -> &InventorySnapshot {
        &self.inventory
    }

    pub fn local_device_id(&self, device_key: &str) -> Result<DeviceId, RouteError> {
        if !self
            .inventory
            .devices
            .iter()
            .any(|device| device.device_key == device_key)
        {
            return Err(RouteError::NotFound);
        }
        Ok(namespaced_id(&self.node_id, device_key))
    }

    pub async fn drivers(
        &self,
        owner_id: &str,
        config: RemoteDriverConfig,
        control: &ExecutionControl,
    ) -> Result<Vec<RemoteDeviceDriver>, RouteError> {
        let discoveries = self
            .inventory
            .devices
            .iter()
            .map(|descriptor| self.driver(&descriptor.device_key, owner_id, config, control));
        let mut drivers = join_all(discoveries)
            .await
            .into_iter()
            .collect::<Result<Vec<_>, _>>()?;
        drivers.sort_by(|left, right| {
            devicerail_core::DeviceDriver::id(left).cmp(devicerail_core::DeviceDriver::id(right))
        });
        Ok(drivers)
    }

    fn is_fresh(&self, config: RouterConfig) -> bool {
        self.health.state != HealthState::Unavailable
            && self.inventory_received_at.elapsed()
                <= std::time::Duration::from_millis(config.max_inventory_age_ms)
            && self.health.received_at.elapsed()
                <= std::time::Duration::from_millis(config.max_health_age_ms)
    }

    fn descriptor(&self, device_key: &str) -> Option<&RemoteDeviceDescriptor> {
        self.inventory
            .devices
            .iter()
            .find(|device| device.device_key == device_key)
    }

    async fn driver(
        &self,
        device_key: &str,
        owner_id: &str,
        config: RemoteDriverConfig,
        control: &ExecutionControl,
    ) -> Result<RemoteDeviceDriver, RouteError> {
        let descriptor = self
            .descriptor(device_key)
            .cloned()
            .ok_or(RouteError::NotFound)?;
        RemoteDeviceDriver::load(
            self.node_id.clone(),
            self.epoch,
            descriptor,
            owner_id,
            config,
            Arc::clone(&self.transport),
            Arc::clone(&self.telemetry),
            control,
        )
        .await
        .map_err(|_| RouteError::InvalidConfiguration)
    }
}

#[derive(Clone)]
struct RouteEntry {
    node: Arc<RemoteNode>,
    device_key: String,
}

#[derive(Default)]
struct RouterState {
    nodes: BTreeMap<NodeId, Arc<RemoteNode>>,
    routes: BTreeMap<DeviceId, RouteEntry>,
}

pub struct NodeRouter {
    config: RouterConfig,
    state: RwLock<RouterState>,
}

impl std::fmt::Debug for NodeRouter {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("NodeRouter")
            .field("config", &self.config)
            .finish_non_exhaustive()
    }
}

impl NodeRouter {
    pub fn new(config: RouterConfig) -> Self {
        Self {
            config,
            state: RwLock::new(RouterState::default()),
        }
    }

    pub async fn upsert(&self, node: Arc<RemoteNode>) -> Result<(), RouteError> {
        if !node.is_fresh(self.config) {
            return Err(RouteError::Unhealthy);
        }
        let mut state = self.state.write().await;
        if !state.nodes.contains_key(node.node_id()) && state.nodes.len() == self.config.max_nodes {
            return Err(RouteError::NodeLimit);
        }
        if let Some(existing) = state.nodes.get(node.node_id()) {
            validate_successor(existing, &node)?;
        }
        state
            .routes
            .retain(|_, route| route.node.node_id() != node.node_id());
        for descriptor in &node.inventory.devices {
            let local_id = namespaced_id(node.node_id(), &descriptor.device_key);
            if state
                .routes
                .insert(
                    local_id,
                    RouteEntry {
                        node: Arc::clone(&node),
                        device_key: descriptor.device_key.clone(),
                    },
                )
                .is_some()
            {
                return Err(RouteError::Duplicate);
            }
        }
        state.nodes.insert(node.node_id.clone(), node);
        Ok(())
    }

    /// Atomically removes a disconnected node and every namespaced route
    /// derived from its inventory. Existing Driver handles remain stale and
    /// fail through their closed transport; they are never rebound to another
    /// node or device key.
    pub async fn remove_node(&self, node_id: &NodeId) -> Result<(), RouteError> {
        let mut state = self.state.write().await;
        if state.nodes.remove(node_id).is_none() {
            return Err(RouteError::NotFound);
        }
        state
            .routes
            .retain(|_, route| route.node.node_id() != node_id);
        Ok(())
    }

    pub async fn inventory(&self, _now_ms: u64) -> Result<Vec<DeviceInfo>, RouteError> {
        let state = self.state.read().await;
        let mut devices = Vec::new();
        for (local_id, route) in &state.routes {
            if !route.node.is_fresh(self.config) {
                continue;
            }
            let descriptor = route
                .node
                .descriptor(&route.device_key)
                .ok_or(RouteError::IdentityDrift)?;
            devices.push(DeviceInfo {
                id: local_id.clone(),
                name: descriptor.name.clone(),
                platform: descriptor.platform.clone(),
                os_version: descriptor.os_version.clone(),
                connected: false,
            });
        }
        if devices.is_empty() && !state.routes.is_empty() {
            return Err(RouteError::Unhealthy);
        }
        Ok(devices)
    }

    pub async fn route_driver(
        &self,
        local_id: &DeviceId,
        owner_id: &str,
        driver_config: RemoteDriverConfig,
        _now_ms: u64,
        control: &ExecutionControl,
    ) -> Result<RemoteDeviceDriver, RouteError> {
        let route = {
            let state = self.state.read().await;
            state
                .routes
                .get(local_id)
                .cloned()
                .ok_or(RouteError::NotFound)?
        };
        if !route.node.is_fresh(self.config) {
            return Err(RouteError::Unhealthy);
        }
        route
            .node
            .driver(&route.device_key, owner_id, driver_config, control)
            .await
    }
}

fn namespaced_id(node_id: &NodeId, device_key: &str) -> DeviceId {
    DeviceId::new(format!("remote:{}:{}", node_id.as_str(), device_key))
}

fn validate_successor(old: &RemoteNode, new: &RemoteNode) -> Result<(), RouteError> {
    if new.epoch < old.epoch {
        return Err(RouteError::StaleEpoch);
    }
    if new.epoch == old.epoch {
        if new.inventory.revision < old.inventory.revision {
            return Err(RouteError::StaleInventory);
        }
        if new.inventory.revision == old.inventory.revision
            && new.inventory.devices != old.inventory.devices
        {
            return Err(RouteError::IdentityDrift);
        }
    }
    for previous in &old.inventory.devices {
        if let Some(current) = new
            .inventory
            .devices
            .iter()
            .find(|candidate| candidate.device_key == previous.device_key)
            && current.platform != previous.platform
        {
            return Err(RouteError::IdentityDrift);
        }
    }
    Ok(())
}

async fn exchange(
    transport: &Arc<dyn PeerTransport>,
    sink: &Arc<dyn TelemetrySink>,
    request: PeerRequest,
    method: OperationMethod,
    control: &ExecutionControl,
) -> Result<crate::PeerResponse, RouteError> {
    let started = Instant::now();
    let trace_id = request.trace_id;
    let node = request.node_id.clone();
    let response = transport.request(request, control).await;
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
        Ok(_) => OperationOutcome::RemoteError,
        Err(TransportError::Cancelled | TransportError::CancelledAfterSend) => {
            OperationOutcome::Cancelled
        }
        Err(TransportError::TimedOut | TransportError::TimedOutAfterSend) => {
            OperationOutcome::TimedOut
        }
        Err(
            TransportError::Protocol
            | TransportError::InvalidRequest
            | TransportError::UnsupportedVersion,
        ) => OperationOutcome::ProtocolError,
        Err(_) => OperationOutcome::TransportError,
    };
    telemetry::record(sink.as_ref(), trace_id, &node, method, outcome, started);
    response.map_err(|error| match error {
        TransportError::UnsupportedVersion => RouteError::UnsupportedVersion,
        other => RouteError::Transport(other),
    })
}

fn success(response: crate::PeerResponse) -> Result<PeerResult, RouteError> {
    if response.ok {
        response.result.ok_or(RouteError::Protocol)
    } else {
        let error = response.error.ok_or(RouteError::Protocol)?;
        Err(RouteError::Remote {
            code: error.code,
            outcome_unknown: error.outcome_unknown,
        })
    }
}
