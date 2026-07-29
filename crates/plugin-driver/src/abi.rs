use devicerail_protocol::{
    ActionDefinition, ActionProtection, Platform, ProtocolVersion, Viewport,
};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use uuid::Uuid;

pub const PLUGIN_ABI_VERSION: u16 = 1;
pub const PLUGIN_ABI_SCHEMA: &str = include_str!("../protocol/plugin-abi-v1.schema.json");

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PluginManifestProtocol {
    pub major: u16,
    pub min_minor: u16,
    pub max_minor: u16,
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PluginCapabilityDeclaration {
    pub name: String,
    #[serde(default)]
    pub protection: ActionProtection,
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PluginManifestDevice {
    pub key: String,
    pub name: String,
    pub platform: Platform,
    pub os_version: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PluginManifest {
    pub manifest_version: u16,
    pub abi_version: u16,
    pub plugin_id: String,
    pub plugin_version: String,
    pub executable: String,
    pub protocol: PluginManifestProtocol,
    pub device: PluginManifestDevice,
    pub capabilities: Vec<PluginCapabilityDeclaration>,
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PluginRequest {
    pub abi_version: u16,
    pub request_id: Uuid,
    pub protocol: ProtocolVersion,
    pub operation: PluginOperation,
}

impl PluginRequest {
    pub fn new(protocol: ProtocolVersion, operation: PluginOperation) -> Self {
        Self {
            abi_version: PLUGIN_ABI_VERSION,
            request_id: Uuid::new_v4(),
            protocol,
            operation,
        }
    }
}

/// Closed operation set. There is intentionally no command/argv/shell
/// operation in the plugin protocol.
#[derive(Clone, Deserialize, Serialize)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum PluginOperation {
    Hello {
        plugin_id: String,
    },
    Health,
    Connect,
    Disconnect,
    Observe {
        capture_screenshot: bool,
    },
    Execute {
        call_id: Uuid,
        name: String,
        arguments: Value,
    },
}

impl std::fmt::Debug for PluginOperation {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Hello { plugin_id } => formatter
                .debug_struct("Hello")
                .field("plugin_id", plugin_id)
                .finish(),
            Self::Health => formatter.write_str("Health"),
            Self::Connect => formatter.write_str("Connect"),
            Self::Disconnect => formatter.write_str("Disconnect"),
            Self::Observe { capture_screenshot } => formatter
                .debug_struct("Observe")
                .field("capture_screenshot", capture_screenshot)
                .finish(),
            Self::Execute { call_id, name, .. } => formatter
                .debug_struct("Execute")
                .field("call_id", call_id)
                .field("name", name)
                .finish_non_exhaustive(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PluginHello {
    pub plugin_id: String,
    pub plugin_version: String,
    pub protocol: ProtocolVersion,
    pub device: PluginManifestDevice,
    pub capabilities: Vec<ActionDefinition>,
}

#[derive(Clone, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PluginFrame {
    pub viewport: Viewport,
    pub screenshot_base64: Option<String>,
    #[serde(default)]
    pub metadata: Map<String, Value>,
}

impl std::fmt::Debug for PluginFrame {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("PluginFrame")
            .field("viewport", &self.viewport)
            .field("has_screenshot_base64", &self.screenshot_base64.is_some())
            .field("metadata_keys", &self.metadata.keys().collect::<Vec<_>>())
            .finish()
    }
}

#[derive(Clone, PartialEq, Deserialize, Serialize)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum PluginResponseResult {
    Hello { hello: PluginHello },
    Ack,
    Frame { frame: PluginFrame },
    Action { output: Value },
}

impl std::fmt::Debug for PluginResponseResult {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Hello { hello } => formatter
                .debug_struct("Hello")
                .field("plugin_id", &hello.plugin_id)
                .field("plugin_version", &hello.plugin_version)
                .field("protocol", &hello.protocol)
                .field("device", &hello.device)
                .field("capability_count", &hello.capabilities.len())
                .finish(),
            Self::Ack => formatter.write_str("Ack"),
            Self::Frame { frame } => formatter.debug_tuple("Frame").field(frame).finish(),
            Self::Action { .. } => formatter.debug_struct("Action").finish_non_exhaustive(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PluginRemoteError {
    pub code: String,
    pub retryable: bool,
}

#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PluginResponse {
    pub abi_version: u16,
    pub request_id: Uuid,
    pub ok: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<PluginResponseResult>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<PluginRemoteError>,
}

impl PluginResponse {
    pub fn success(request_id: Uuid, result: PluginResponseResult) -> Self {
        Self {
            abi_version: PLUGIN_ABI_VERSION,
            request_id,
            ok: true,
            result: Some(result),
            error: None,
        }
    }

    pub fn failure(request_id: Uuid, code: impl Into<String>, retryable: bool) -> Self {
        Self {
            abi_version: PLUGIN_ABI_VERSION,
            request_id,
            ok: false,
            result: None,
            error: Some(PluginRemoteError {
                code: code.into(),
                retryable,
            }),
        }
    }
}
