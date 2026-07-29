use std::{fmt, str::FromStr, sync::LazyLock};

use devicerail_protocol::{
    ActionCall, ActionDefinition, ActionResult, DeviceInfo, Observation, Platform,
    ScreenshotOmissionReason,
};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use uuid::Uuid;

pub const DISTRIBUTED_PROTOCOL_VERSION: u16 = 2;
pub const MAX_NODE_ID_BYTES: usize = 64;
pub const MAX_DEVICE_KEY_BYTES: usize = 128;
pub const MAX_INVENTORY_DEVICES: usize = 256;
pub const MAX_CAPABILITIES: usize = 128;
pub const MAX_METADATA_BYTES: usize = 64 * 1024;
pub const MAX_SAFE_INTEGER: u64 = 9_007_199_254_740_991;
pub const MAX_REQUEST_TIMEOUT_MS: u64 = 5 * 60_000;

#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum ModelError {
    #[error("distributed protocol version is unsupported")]
    UnsupportedVersion,
    #[error("distributed protocol identifier is invalid")]
    InvalidIdentifier,
    #[error("distributed protocol envelope is invalid")]
    InvalidEnvelope,
    #[error("distributed inventory exceeds a bounded limit")]
    InventoryLimit,
    #[error("distributed inventory contains duplicate device keys")]
    DuplicateDevice,
    #[error("distributed response does not match its request")]
    ResponseMismatch,
}

#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct NodeId(String);

impl NodeId {
    pub fn parse(value: impl Into<String>) -> Result<Self, ModelError> {
        let value = value.into();
        if !valid_identifier(&value, MAX_NODE_ID_BYTES) {
            return Err(ModelError::InvalidIdentifier);
        }
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for NodeId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.debug_tuple("NodeId").field(&self.0).finish()
    }
}

impl fmt::Display for NodeId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl FromStr for NodeId {
    type Err = ModelError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::parse(value)
    }
}

impl<'de> Deserialize<'de> for NodeId {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::parse(value).map_err(serde::de::Error::custom)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RemoteDeviceDescriptor {
    pub device_key: String,
    pub name: String,
    pub platform: Platform,
    pub os_version: Option<String>,
}

impl RemoteDeviceDescriptor {
    pub fn validate(&self) -> Result<(), ModelError> {
        if !valid_identifier(&self.device_key, MAX_DEVICE_KEY_BYTES)
            || self.name.trim().is_empty()
            || self.name.len() > 256
            || self
                .os_version
                .as_deref()
                .is_some_and(|value| value.trim().is_empty() || value.len() > 128)
            || matches!(&self.platform, Platform::Other(value) if value.trim().is_empty() || value.len() > 64)
        {
            return Err(ModelError::InvalidIdentifier);
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct InventorySnapshot {
    pub node_id: NodeId,
    pub epoch: u64,
    pub revision: u64,
    pub generated_at_ms: u64,
    pub devices: Vec<RemoteDeviceDescriptor>,
}

impl InventorySnapshot {
    pub fn validate(&self) -> Result<(), ModelError> {
        if self.epoch == 0
            || self.epoch > MAX_SAFE_INTEGER
            || self.revision == 0
            || self.revision > MAX_SAFE_INTEGER
            || self.generated_at_ms == 0
            || self.generated_at_ms > MAX_SAFE_INTEGER
        {
            return Err(ModelError::InvalidEnvelope);
        }
        if self.devices.is_empty() || self.devices.len() > MAX_INVENTORY_DEVICES {
            return Err(ModelError::InventoryLimit);
        }
        let mut keys = std::collections::BTreeSet::new();
        for device in &self.devices {
            device.validate()?;
            if !keys.insert(device.device_key.as_str()) {
                return Err(ModelError::DuplicateDevice);
            }
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum HealthState {
    Healthy,
    Degraded,
    Unavailable,
}

/// Additive contracts guaranteed by distributed peer protocol v2.
///
/// These are transport capabilities, not per-device capabilities: a device
/// may still omit a UI Snapshot or reject a semantic Action with an explicit
/// Driver error. A peer must advertise both capabilities before the router can
/// safely preserve operation-scoped feature gates across the hop.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PeerProtocolCapabilities {
    pub ui_snapshots_v1: bool,
    pub semantic_actions_v1: bool,
}

impl PeerProtocolCapabilities {
    pub const REQUIRED: Self = Self {
        ui_snapshots_v1: true,
        semantic_actions_v1: true,
    };

    pub const fn supports_required(self) -> bool {
        self.ui_snapshots_v1 && self.semantic_actions_v1
    }
}

#[derive(Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PeerLease {
    pub lease_id: Uuid,
    pub device_key: String,
    pub owner_id: String,
    pub node_epoch: u64,
    pub expires_at_ms: u64,
}

impl PeerLease {
    pub fn validate(&self) -> Result<(), ModelError> {
        if self.lease_id.is_nil()
            || !valid_identifier(&self.device_key, MAX_DEVICE_KEY_BYTES)
            || !valid_identifier(&self.owner_id, 64)
            || self.node_epoch == 0
            || self.node_epoch > MAX_SAFE_INTEGER
            || self.expires_at_ms == 0
            || self.expires_at_ms > MAX_SAFE_INTEGER
        {
            return Err(ModelError::InvalidEnvelope);
        }
        Ok(())
    }
}

impl fmt::Debug for PeerLease {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PeerLease")
            .field("lease_id", &self.lease_id)
            .field("device_key", &self.device_key)
            .field("owner_id", &self.owner_id)
            .field("node_epoch", &self.node_epoch)
            .field("expires_at_ms", &self.expires_at_ms)
            .finish()
    }
}

#[derive(Clone, PartialEq, Deserialize, Serialize)]
#[serde(
    tag = "method",
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum PeerOperation {
    Hello,
    Inventory,
    Health,
    Capabilities {
        device_key: String,
    },
    LeaseAcquire {
        device_key: String,
        owner_id: String,
        ttl_ms: u64,
    },
    LeaseRenew {
        lease: PeerLease,
        ttl_ms: u64,
    },
    LeaseRelease {
        lease: PeerLease,
    },
    Connect {
        device_key: String,
    },
    Disconnect {
        device_key: String,
    },
    Observe {
        device_key: String,
        screenshot_omission: Option<ScreenshotOmissionReason>,
        ui_snapshots_enabled: bool,
        semantic_actions_enabled: bool,
    },
    Execute {
        device_key: String,
        call: ActionCall,
        screenshot_omission: Option<ScreenshotOmissionReason>,
        ui_snapshots_enabled: bool,
        semantic_actions_enabled: bool,
    },
    EvidenceRead {
        device_key: String,
        evidence_id: String,
        offset: u64,
        max_bytes: u32,
    },
    Cancel {
        target_request_id: Uuid,
        call_id: Option<Uuid>,
    },
}

impl PeerOperation {
    pub fn method_name(&self) -> &'static str {
        match self {
            Self::Hello => "hello",
            Self::Inventory => "inventory",
            Self::Health => "health",
            Self::Capabilities { .. } => "capabilities",
            Self::LeaseAcquire { .. } => "leaseAcquire",
            Self::LeaseRenew { .. } => "leaseRenew",
            Self::LeaseRelease { .. } => "leaseRelease",
            Self::Connect { .. } => "connect",
            Self::Disconnect { .. } => "disconnect",
            Self::Observe { .. } => "observe",
            Self::Execute { .. } => "execute",
            Self::EvidenceRead { .. } => "evidenceRead",
            Self::Cancel { .. } => "cancel",
        }
    }

    pub(crate) fn device_key(&self) -> Option<&str> {
        match self {
            Self::Capabilities { device_key }
            | Self::LeaseAcquire { device_key, .. }
            | Self::Connect { device_key }
            | Self::Disconnect { device_key }
            | Self::Observe { device_key, .. }
            | Self::Execute { device_key, .. }
            | Self::EvidenceRead { device_key, .. } => Some(device_key),
            Self::LeaseRenew { lease, .. } | Self::LeaseRelease { lease } => {
                Some(&lease.device_key)
            }
            Self::Hello | Self::Inventory | Self::Health | Self::Cancel { .. } => None,
        }
    }
}

impl fmt::Debug for PeerOperation {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PeerOperation")
            .field("method", &self.method_name())
            .field("device_key", &self.device_key())
            .field(
                "call_id",
                &match self {
                    Self::Execute { call, .. } => Some(call.id),
                    Self::Cancel { call_id, .. } => *call_id,
                    _ => None,
                },
            )
            .finish_non_exhaustive()
    }
}

#[derive(Clone, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PeerRequest {
    pub protocol_version: u16,
    pub request_id: Uuid,
    pub trace_id: Uuid,
    pub node_id: NodeId,
    pub node_epoch: Option<u64>,
    pub timeout_ms: Option<u64>,
    pub call_id: Option<Uuid>,
    pub lease: Option<PeerLease>,
    pub operation: PeerOperation,
}

impl fmt::Debug for PeerRequest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PeerRequest")
            .field("protocol_version", &self.protocol_version)
            .field("request_id", &self.request_id)
            .field("trace_id", &self.trace_id)
            .field("node_id", &self.node_id)
            .field("node_epoch", &self.node_epoch)
            .field("timeout_ms", &self.timeout_ms)
            .field("call_id", &self.call_id)
            .field("has_lease", &self.lease.is_some())
            .field("operation", &self.operation)
            .finish()
    }
}

impl PeerRequest {
    pub fn new(node_id: NodeId, node_epoch: Option<u64>, operation: PeerOperation) -> Self {
        let call_id = match &operation {
            PeerOperation::Execute { call, .. } => Some(call.id),
            _ => None,
        };
        Self {
            protocol_version: DISTRIBUTED_PROTOCOL_VERSION,
            request_id: Uuid::new_v4(),
            trace_id: Uuid::new_v4(),
            node_id,
            node_epoch,
            timeout_ms: None,
            call_id,
            lease: None,
            operation,
        }
    }

    pub fn validate(&self) -> Result<(), ModelError> {
        if self.protocol_version != DISTRIBUTED_PROTOCOL_VERSION {
            return Err(ModelError::UnsupportedVersion);
        }
        if self.request_id.is_nil()
            || self.trace_id.is_nil()
            || self
                .node_epoch
                .is_some_and(|value| value == 0 || value > MAX_SAFE_INTEGER)
            || self
                .timeout_ms
                .is_some_and(|value| value == 0 || value > MAX_REQUEST_TIMEOUT_MS)
            || self.call_id.is_some_and(|value| value.is_nil())
        {
            return Err(ModelError::InvalidEnvelope);
        }
        if let Some(key) = self.operation.device_key()
            && !valid_identifier(key, MAX_DEVICE_KEY_BYTES)
        {
            return Err(ModelError::InvalidIdentifier);
        }
        if let Some(lease) = &self.lease {
            lease.validate()?;
            if self.operation.device_key() != Some(lease.device_key.as_str())
                || self.node_epoch != Some(lease.node_epoch)
            {
                return Err(ModelError::InvalidEnvelope);
            }
        }
        match &self.operation {
            PeerOperation::Hello | PeerOperation::Inventory => {
                if self.node_epoch.is_some() || self.lease.is_some() || self.call_id.is_some() {
                    return Err(ModelError::InvalidEnvelope);
                }
            }
            PeerOperation::Health | PeerOperation::Capabilities { .. } => {
                if self.node_epoch.is_none() || self.lease.is_some() || self.call_id.is_some() {
                    return Err(ModelError::InvalidEnvelope);
                }
            }
            PeerOperation::LeaseAcquire {
                owner_id, ttl_ms, ..
            } => {
                if self.node_epoch.is_none()
                    || self.lease.is_some()
                    || self.call_id.is_some()
                    || !valid_identifier(owner_id, 64)
                    || !valid_lease_ttl(*ttl_ms)
                {
                    return Err(ModelError::InvalidEnvelope);
                }
            }
            PeerOperation::LeaseRenew { lease, ttl_ms } => {
                if self.node_epoch != Some(lease.node_epoch)
                    || self.lease.as_ref() != Some(lease)
                    || self.call_id.is_some()
                    || !valid_lease_ttl(*ttl_ms)
                {
                    return Err(ModelError::InvalidEnvelope);
                }
            }
            PeerOperation::LeaseRelease { lease } => {
                if self.node_epoch != Some(lease.node_epoch)
                    || self.lease.as_ref() != Some(lease)
                    || self.call_id.is_some()
                {
                    return Err(ModelError::InvalidEnvelope);
                }
            }
            PeerOperation::Connect { .. }
            | PeerOperation::Disconnect { .. }
            | PeerOperation::Observe { .. } => {
                if self.node_epoch.is_none() || self.lease.is_none() || self.call_id.is_some() {
                    return Err(ModelError::InvalidEnvelope);
                }
            }
            PeerOperation::Execute { call, .. } => {
                if self.node_epoch.is_none()
                    || self.lease.is_none()
                    || self.call_id != Some(call.id)
                    || call.id.is_nil()
                    || call.name.trim().is_empty()
                    || call.name.len() > 128
                    || !call.arguments.is_object()
                {
                    return Err(ModelError::InvalidEnvelope);
                }
            }
            PeerOperation::EvidenceRead {
                evidence_id,
                offset,
                max_bytes,
                ..
            } => {
                if self.node_epoch.is_none()
                    || self.lease.is_none()
                    || self.call_id.is_some()
                    || !valid_evidence_id(evidence_id)
                    || *offset > MAX_SAFE_INTEGER
                    || !(1..=256 * 1024).contains(max_bytes)
                {
                    return Err(ModelError::InvalidEnvelope);
                }
            }
            PeerOperation::Cancel {
                target_request_id,
                call_id,
            } => {
                if self.node_epoch.is_none()
                    || self.lease.is_some()
                    || self.call_id.is_some()
                    || target_request_id.is_nil()
                    || call_id.is_some_and(|value| value.is_nil())
                {
                    return Err(ModelError::InvalidEnvelope);
                }
            }
        }
        Ok(())
    }
}

#[derive(Clone, PartialEq, Deserialize, Serialize)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum PeerResult {
    Hello {
        node_id: NodeId,
        epoch: u64,
        max_frame_bytes: u32,
        capabilities: PeerProtocolCapabilities,
    },
    Inventory {
        inventory: InventorySnapshot,
    },
    Health {
        state: HealthState,
        checked_at_ms: u64,
    },
    Capabilities {
        actions: Vec<ActionDefinition>,
    },
    Lease {
        lease: PeerLease,
    },
    Device {
        device: DeviceInfo,
    },
    Observation {
        observation: Box<Observation>,
    },
    Action {
        result: Box<ActionResult>,
    },
    EvidenceChunk {
        evidence_id: String,
        media_type: String,
        total_size: u64,
        offset: u64,
        data_base64: String,
        sha256: Option<String>,
        done: bool,
    },
    Ack,
}

impl PeerResult {
    pub fn kind_name(&self) -> &'static str {
        match self {
            Self::Hello { .. } => "hello",
            Self::Inventory { .. } => "inventory",
            Self::Health { .. } => "health",
            Self::Capabilities { .. } => "capabilities",
            Self::Lease { .. } => "lease",
            Self::Device { .. } => "device",
            Self::Observation { .. } => "observation",
            Self::Action { .. } => "action",
            Self::EvidenceChunk { .. } => "evidenceChunk",
            Self::Ack => "ack",
        }
    }

    fn matches_request(&self, request: &PeerRequest, response_epoch: u64) -> bool {
        match (&request.operation, self) {
            (
                PeerOperation::Hello,
                Self::Hello {
                    node_id,
                    epoch,
                    max_frame_bytes,
                    ..
                },
            ) => {
                node_id == &request.node_id
                    && *epoch == response_epoch
                    && *max_frame_bytes > 0
                    && *max_frame_bytes as usize <= crate::MAX_PEER_FRAME_BYTES
            }
            (PeerOperation::Inventory, Self::Inventory { inventory }) => {
                inventory.validate().is_ok()
                    && inventory.node_id == request.node_id
                    && inventory.epoch == response_epoch
            }
            (PeerOperation::Health, Self::Health { checked_at_ms, .. }) => {
                *checked_at_ms > 0 && *checked_at_ms <= MAX_SAFE_INTEGER
            }
            (PeerOperation::Capabilities { .. }, Self::Capabilities { actions }) => {
                !actions.is_empty() && actions.len() <= MAX_CAPABILITIES
            }
            (
                PeerOperation::LeaseAcquire {
                    device_key,
                    owner_id,
                    ..
                },
                Self::Lease { lease },
            ) => {
                lease.validate().is_ok()
                    && lease.device_key == *device_key
                    && lease.owner_id == *owner_id
                    && lease.node_epoch == response_epoch
            }
            (PeerOperation::LeaseRenew { lease: old, .. }, Self::Lease { lease: renewed }) => {
                renewed.validate().is_ok()
                    && renewed.lease_id == old.lease_id
                    && renewed.device_key == old.device_key
                    && renewed.owner_id == old.owner_id
                    && renewed.node_epoch == old.node_epoch
            }
            (PeerOperation::LeaseRelease { .. }, Self::Ack)
            | (PeerOperation::Disconnect { .. }, Self::Ack)
            | (PeerOperation::Cancel { .. }, Self::Ack) => true,
            (PeerOperation::Connect { device_key }, Self::Device { device }) => {
                device.id.0 == *device_key && device.connected
            }
            (PeerOperation::Observe { device_key, .. }, Self::Observation { observation }) => {
                observation.device_id.0 == *device_key
            }
            (PeerOperation::Execute { call, .. }, Self::Action { result }) => {
                result.call_id == call.id
            }
            (
                PeerOperation::EvidenceRead {
                    evidence_id,
                    offset,
                    max_bytes,
                    ..
                },
                Self::EvidenceChunk {
                    evidence_id: returned_id,
                    total_size,
                    offset: returned_offset,
                    data_base64,
                    ..
                },
            ) => {
                returned_id == evidence_id
                    && returned_offset == offset
                    && *total_size > *offset
                    && data_base64.len() <= ((*max_bytes as usize).div_ceil(3) * 4)
            }
            _ => false,
        }
    }
}

impl fmt::Debug for PeerResult {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PeerResult")
            .field("kind", &self.kind_name())
            .finish_non_exhaustive()
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PeerError {
    pub code: String,
    pub retryable: bool,
    pub outcome_unknown: bool,
}

impl PeerError {
    pub fn validate(&self) -> Result<(), ModelError> {
        if !valid_code(&self.code) {
            return Err(ModelError::InvalidEnvelope);
        }
        Ok(())
    }
}

#[derive(Clone, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PeerResponse {
    pub protocol_version: u16,
    pub request_id: Uuid,
    pub node_id: NodeId,
    pub node_epoch: u64,
    pub ok: bool,
    pub result: Option<PeerResult>,
    pub error: Option<PeerError>,
}

impl fmt::Debug for PeerResponse {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PeerResponse")
            .field("protocol_version", &self.protocol_version)
            .field("request_id", &self.request_id)
            .field("node_id", &self.node_id)
            .field("node_epoch", &self.node_epoch)
            .field("ok", &self.ok)
            .field(
                "result_kind",
                &self.result.as_ref().map(PeerResult::kind_name),
            )
            .field("error", &self.error)
            .finish()
    }
}

impl PeerResponse {
    pub fn success(request: &PeerRequest, node_epoch: u64, result: PeerResult) -> Self {
        Self {
            protocol_version: DISTRIBUTED_PROTOCOL_VERSION,
            request_id: request.request_id,
            node_id: request.node_id.clone(),
            node_epoch,
            ok: true,
            result: Some(result),
            error: None,
        }
    }

    pub fn failure(request: &PeerRequest, node_epoch: u64, error: PeerError) -> Self {
        Self {
            protocol_version: DISTRIBUTED_PROTOCOL_VERSION,
            request_id: request.request_id,
            node_id: request.node_id.clone(),
            node_epoch,
            ok: false,
            result: None,
            error: Some(error),
        }
    }

    pub fn validate_for(&self, request: &PeerRequest) -> Result<(), ModelError> {
        if self.protocol_version != DISTRIBUTED_PROTOCOL_VERSION {
            return Err(ModelError::UnsupportedVersion);
        }
        if self.request_id != request.request_id
            || self.node_id != request.node_id
            || self.node_epoch == 0
            || self.node_epoch > MAX_SAFE_INTEGER
            || request
                .node_epoch
                .is_some_and(|expected| self.node_epoch != expected)
            || self.ok != (self.result.is_some() && self.error.is_none())
            || (!self.ok && (self.result.is_some() || self.error.is_none()))
        {
            return Err(ModelError::ResponseMismatch);
        }
        if let Some(error) = &self.error {
            error.validate()?;
        }
        if let Some(result) = &self.result
            && !result.matches_request(request, self.node_epoch)
        {
            return Err(ModelError::ResponseMismatch);
        }
        Ok(())
    }
}

pub(crate) fn valid_identifier(value: &str, max_bytes: usize) -> bool {
    !value.is_empty()
        && value.len() <= max_bytes
        && value.bytes().enumerate().all(|(index, byte)| {
            byte.is_ascii_alphanumeric() || (index > 0 && matches!(byte, b'.' | b'_' | b'-'))
        })
}

pub(crate) fn valid_code(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 64
        && value.bytes().enumerate().all(|(index, byte)| {
            byte.is_ascii_lowercase()
                || (index > 0 && byte.is_ascii_digit())
                || (index > 0 && matches!(byte, b'_' | b'-' | b'.'))
        })
}

pub(crate) fn valid_evidence_id(value: &str) -> bool {
    valid_identifier(value, 128)
        || value.strip_prefix("sha256:").is_some_and(|digest| {
            digest.len() == 64
                && digest
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        })
}

pub(crate) fn valid_lease_ttl(ttl_ms: u64) -> bool {
    (1_000..=5 * 60_000).contains(&ttl_ms)
}

pub(crate) fn wire_schema_accepts(bytes: &[u8]) -> bool {
    static VALIDATOR: LazyLock<jsonschema::Validator> = LazyLock::new(|| {
        let schema: serde_json::Value =
            serde_json::from_str(crate::PEER_PROTOCOL_SCHEMA).expect("bundled peer schema parses");
        jsonschema::validator_for(&schema).expect("bundled peer schema compiles")
    });
    serde_json::from_slice::<serde_json::Value>(bytes).is_ok_and(|value| VALIDATOR.is_valid(&value))
}

pub(crate) fn wire_protocol_version(bytes: &[u8]) -> Option<u16> {
    serde_json::from_slice::<serde_json::Value>(bytes)
        .ok()?
        .get("protocolVersion")?
        .as_u64()?
        .try_into()
        .ok()
}

#[cfg(test)]
mod tests {
    use devicerail_protocol::Platform;
    use serde_json::json;

    use super::{
        DISTRIBUTED_PROTOCOL_VERSION, InventorySnapshot, ModelError, NodeId, PeerOperation,
        PeerRequest, RemoteDeviceDescriptor,
    };

    fn node() -> NodeId {
        NodeId::parse("lab-a").expect("node id")
    }

    #[test]
    fn wire_is_camel_case_strict_and_debug_redacts_arguments() {
        let request = PeerRequest::new(
            node(),
            Some(7),
            PeerOperation::Execute {
                device_key: "phone-1".into(),
                call: devicerail_protocol::ActionCall {
                    id: uuid::Uuid::new_v4(),
                    name: "inputSecret".into(),
                    arguments: json!({"secret": "DO_NOT_LOG"}),
                },
                screenshot_omission: Some(
                    devicerail_protocol::ScreenshotOmissionReason::ProtectedAction,
                ),
                ui_snapshots_enabled: false,
                semantic_actions_enabled: false,
            },
        );
        assert!(format!("{request:?}").contains("execute"));
        assert!(!format!("{request:?}").contains("DO_NOT_LOG"));
        let value = serde_json::to_value(&request).expect("serialize");
        assert_eq!(value["protocolVersion"], DISTRIBUTED_PROTOCOL_VERSION);
        assert_eq!(value["operation"]["method"], "execute");
        assert!(value.get("protocol_version").is_none());
        assert!(
            serde_json::from_value::<PeerRequest>(json!({
                "protocolVersion": DISTRIBUTED_PROTOCOL_VERSION,
                "requestId": uuid::Uuid::new_v4(),
                "traceId": uuid::Uuid::new_v4(),
                "nodeId": "lab-a",
                "nodeEpoch": 7,
                "timeoutMs": null,
                "callId": null,
                "lease": null,
                "operation": {"method": "health"},
                "unexpected": true
            }))
            .is_err()
        );
    }

    #[test]
    fn duplicate_and_oversized_inventory_fail_explicitly() {
        let device = RemoteDeviceDescriptor {
            device_key: "phone-1".into(),
            name: "Phone".into(),
            platform: Platform::Android,
            os_version: Some("15".into()),
        };
        let duplicate = InventorySnapshot {
            node_id: node(),
            epoch: 1,
            revision: 1,
            generated_at_ms: 1,
            devices: vec![device.clone(), device],
        };
        assert_eq!(duplicate.validate(), Err(ModelError::DuplicateDevice));
    }
}
