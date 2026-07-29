use std::{collections::HashSet, fmt};

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{ActionCall, AssetRef};

const fn default_true() -> bool {
    true
}

const fn is_true(value: &bool) -> bool {
    *value
}

/// Current canonical UI Tree payload version.
pub const UI_SNAPSHOT_FORMAT_VERSION: u16 = 1;

/// Media type used by Evidence objects containing a canonical UI Tree.
pub const UI_SNAPSHOT_MEDIA_TYPE: &str = "application/vnd.devicerail.ui-tree+json;version=1";

/// Hard limits shared by schema validation, Evidence loading, and Drivers.
pub const MAX_UI_SNAPSHOT_NODES: usize = 10_000;
/// Leaves headroom for the JSON-RPC response envelope under the 1 MiB frame cap.
pub const MAX_UI_SNAPSHOT_BYTES: u64 = 768 * 1_024;
pub const MAX_UI_IDENTIFIER_LENGTH: usize = 4_096;
pub const MAX_UI_ROLE_LENGTH: usize = 256;
pub const MAX_UI_TEXT_LENGTH: usize = 65_536;
pub const MAX_ELEMENT_VALUE_LENGTH: usize = 65_536;

pub const FIND_ELEMENT_ACTION: &str = "findElement";
pub const TAP_ELEMENT_ACTION: &str = "tapElement";
pub const CLEAR_ELEMENT_ACTION: &str = "clearElement";
pub const SET_ELEMENT_VALUE_ACTION: &str = "setElementValue";
pub const WAIT_FOR_ELEMENT_ACTION: &str = "waitForElement";
pub const SEMANTIC_ACTION_NAMES: [&str; 5] = [
    FIND_ELEMENT_ACTION,
    TAP_ELEMENT_ACTION,
    CLEAR_ELEMENT_ACTION,
    SET_ELEMENT_VALUE_ACTION,
    WAIT_FOR_ELEMENT_ACTION,
];

pub fn is_semantic_action_name(name: &str) -> bool {
    matches!(
        name,
        FIND_ELEMENT_ACTION
            | TAP_ELEMENT_ACTION
            | CLEAR_ELEMENT_ACTION
            | SET_ELEMENT_VALUE_ACTION
            | WAIT_FOR_ELEMENT_ACTION
    )
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum UiContractError {
    EmptyField(&'static str),
    FieldTooLong(&'static str),
    InvalidBounds(String),
    InvalidContextForCss,
    InvalidSemanticExecutionContext,
    EmptySelector,
    UnsupportedSnapshotFormat(u16),
    EmptySnapshot,
    TooManyNodes(usize),
    SnapshotTooLarge(usize),
    DuplicateStableNodeId(String),
    InvalidRootOrder,
    MissingOrLateParent(String),
    InvalidPreorder(String),
    InvalidNodeCount(u32),
    InvalidByteLength(u64),
    InvalidSnapshotMediaType(String),
    InvalidWaitResult,
}

impl fmt::Display for UiContractError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyField(field) => write!(formatter, "{field} must not be empty"),
            Self::FieldTooLong(field) => write!(formatter, "{field} exceeds its wire limit"),
            Self::InvalidBounds(node) => write!(formatter, "node {node} has invalid bounds"),
            Self::InvalidContextForCss => {
                write!(formatter, "css selectors cannot target a native context")
            }
            Self::InvalidSemanticExecutionContext => {
                write!(
                    formatter,
                    "semantic execution mode and context kind disagree"
                )
            }
            Self::EmptySelector => write!(formatter, "element selector has no matching fields"),
            Self::UnsupportedSnapshotFormat(version) => {
                write!(
                    formatter,
                    "unsupported UI Snapshot format version {version}"
                )
            }
            Self::EmptySnapshot => write!(formatter, "UI Snapshot must contain a root node"),
            Self::TooManyNodes(count) => write!(formatter, "UI Snapshot has {count} nodes"),
            Self::SnapshotTooLarge(bytes) => write!(formatter, "UI Snapshot is {bytes} bytes"),
            Self::DuplicateStableNodeId(id) => write!(formatter, "duplicate stable node id {id}"),
            Self::InvalidRootOrder => write!(formatter, "rootNodeIds do not match preorder roots"),
            Self::MissingOrLateParent(id) => {
                write!(formatter, "node {id} references a missing or later parent")
            }
            Self::InvalidPreorder(id) => write!(formatter, "node {id} breaks preorder traversal"),
            Self::InvalidNodeCount(count) => write!(formatter, "invalid UI node count {count}"),
            Self::InvalidByteLength(bytes) => write!(formatter, "invalid UI byte length {bytes}"),
            Self::InvalidSnapshotMediaType(media_type) => {
                write!(formatter, "invalid UI Snapshot media type {media_type}")
            }
            Self::InvalidWaitResult => write!(formatter, "wait result contradicts its condition"),
        }
    }
}

impl std::error::Error for UiContractError {}

fn validate_required(value: &str, field: &'static str, max: usize) -> Result<(), UiContractError> {
    if value.trim().is_empty() {
        return Err(UiContractError::EmptyField(field));
    }
    if value.chars().count() > max {
        return Err(UiContractError::FieldTooLong(field));
    }
    Ok(())
}

fn validate_optional(
    value: Option<&str>,
    field: &'static str,
    max: usize,
) -> Result<(), UiContractError> {
    if value.is_some_and(|value| value.chars().count() > max) {
        return Err(UiContractError::FieldTooLong(field));
    }
    Ok(())
}

#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum UiContextKind {
    Native,
    Web,
}

/// Full identity of one native accessibility or web-document context.
/// `documentEpoch` is required for both channels and changes after reconnect,
/// navigation, or any replacement that invalidates prior node references.
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct UiContextRef {
    pub context_kind: UiContextKind,
    #[cfg_attr(feature = "schema", schemars(length(min = 1, max = 4_096)))]
    pub context_id: String,
    #[cfg_attr(feature = "schema", schemars(length(min = 1, max = 4_096)))]
    pub document_epoch: String,
}

impl UiContextRef {
    pub fn validate(&self) -> Result<(), UiContractError> {
        validate_required(&self.context_id, "contextId", MAX_UI_IDENTIFIER_LENGTH)?;
        validate_required(
            &self.document_epoch,
            "documentEpoch",
            MAX_UI_IDENTIFIER_LENGTH,
        )
    }
}

/// Selects a current context without pretending to own its document epoch.
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct UiContextSelector {
    pub context_kind: UiContextKind,
    #[cfg_attr(feature = "schema", schemars(length(min = 1, max = 4_096)))]
    pub context_id: Option<String>,
}

impl UiContextSelector {
    pub fn validate(&self) -> Result<(), UiContractError> {
        if let Some(context_id) = &self.context_id {
            validate_required(context_id, "contextId", MAX_UI_IDENTIFIER_LENGTH)?;
        }
        Ok(())
    }
}

#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Clone, Copy, Debug, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct UiRect {
    pub x: f64,
    pub y: f64,
    #[cfg_attr(feature = "schema", schemars(range(min = 0.0)))]
    pub width: f64,
    #[cfg_attr(feature = "schema", schemars(range(min = 0.0)))]
    pub height: f64,
}

impl UiRect {
    pub const fn is_valid(self) -> bool {
        self.x.is_finite()
            && self.y.is_finite()
            && self.width.is_finite()
            && self.height.is_finite()
            && self.width >= 0.0
            && self.height >= 0.0
    }
}

/// One node in the normalized preorder list. Unknown platform values remain
/// `null`; Drivers must not manufacture optimistic enabled/hittable states.
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct UiNode {
    #[cfg_attr(feature = "schema", schemars(length(min = 1, max = 4_096)))]
    pub stable_node_id: String,
    #[cfg_attr(feature = "schema", schemars(length(min = 1, max = 4_096)))]
    pub parent_stable_node_id: Option<String>,
    #[cfg_attr(feature = "schema", schemars(length(min = 1, max = 256)))]
    pub role: String,
    #[cfg_attr(feature = "schema", schemars(length(max = 65_536)))]
    pub name: Option<String>,
    #[cfg_attr(feature = "schema", schemars(length(max = 65_536)))]
    pub value: Option<String>,
    #[cfg_attr(feature = "schema", schemars(length(max = 4_096)))]
    pub identifier: Option<String>,
    #[cfg_attr(feature = "schema", schemars(length(max = 65_536)))]
    pub text: Option<String>,
    pub bounds: Option<UiRect>,
    pub enabled: Option<bool>,
    pub hittable: Option<bool>,
}

impl UiNode {
    pub fn validate(&self) -> Result<(), UiContractError> {
        validate_required(
            &self.stable_node_id,
            "stableNodeId",
            MAX_UI_IDENTIFIER_LENGTH,
        )?;
        if let Some(parent) = &self.parent_stable_node_id {
            validate_required(parent, "parentStableNodeId", MAX_UI_IDENTIFIER_LENGTH)?;
        }
        validate_required(&self.role, "role", MAX_UI_ROLE_LENGTH)?;
        validate_optional(self.name.as_deref(), "name", MAX_UI_TEXT_LENGTH)?;
        validate_optional(self.value.as_deref(), "value", MAX_UI_TEXT_LENGTH)?;
        validate_optional(
            self.identifier.as_deref(),
            "identifier",
            MAX_UI_IDENTIFIER_LENGTH,
        )?;
        validate_optional(self.text.as_deref(), "text", MAX_UI_TEXT_LENGTH)?;
        if self.bounds.is_some_and(|bounds| !bounds.is_valid()) {
            return Err(UiContractError::InvalidBounds(self.stable_node_id.clone()));
        }
        Ok(())
    }
}

/// Canonical UI Tree. `nodes` is preorder and every parent must precede its
/// contiguous descendants. The serialized payload is bounded to 768 KiB.
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct UiSnapshot {
    #[cfg_attr(feature = "schema", schemars(range(min = 1_u16, max = 1_u16)))]
    pub format_version: u16,
    pub observation_id: Uuid,
    pub context: UiContextRef,
    #[cfg_attr(feature = "schema", schemars(length(min = 1, max = 10_000)))]
    pub root_stable_node_ids: Vec<String>,
    #[cfg_attr(feature = "schema", schemars(length(min = 1, max = 10_000)))]
    pub nodes: Vec<UiNode>,
}

impl UiSnapshot {
    pub fn validate(&self) -> Result<(), UiContractError> {
        if self.format_version != UI_SNAPSHOT_FORMAT_VERSION {
            return Err(UiContractError::UnsupportedSnapshotFormat(
                self.format_version,
            ));
        }
        self.context.validate()?;
        if self.nodes.is_empty() || self.root_stable_node_ids.is_empty() {
            return Err(UiContractError::EmptySnapshot);
        }
        if self.nodes.len() > MAX_UI_SNAPSHOT_NODES {
            return Err(UiContractError::TooManyNodes(self.nodes.len()));
        }

        let mut seen = HashSet::with_capacity(self.nodes.len());
        let mut stack: Vec<&str> = Vec::new();
        let mut actual_roots = Vec::new();
        for node in &self.nodes {
            node.validate()?;
            if !seen.insert(node.stable_node_id.as_str()) {
                return Err(UiContractError::DuplicateStableNodeId(
                    node.stable_node_id.clone(),
                ));
            }
            match node.parent_stable_node_id.as_deref() {
                None => {
                    actual_roots.push(node.stable_node_id.as_str());
                    stack.clear();
                }
                Some(parent) => {
                    if !seen.contains(parent) {
                        return Err(UiContractError::MissingOrLateParent(
                            node.stable_node_id.clone(),
                        ));
                    }
                    let Some(parent_index) = stack.iter().rposition(|id| *id == parent) else {
                        return Err(UiContractError::InvalidPreorder(
                            node.stable_node_id.clone(),
                        ));
                    };
                    stack.truncate(parent_index + 1);
                }
            }
            stack.push(node.stable_node_id.as_str());
        }

        if actual_roots
            != self
                .root_stable_node_ids
                .iter()
                .map(String::as_str)
                .collect::<Vec<_>>()
        {
            return Err(UiContractError::InvalidRootOrder);
        }
        Ok(())
    }

    pub fn validate_against(
        &self,
        observation_id: Uuid,
        reference: &UiSnapshotRef,
    ) -> Result<(), UiContractError> {
        self.validate()?;
        reference.validate()?;
        if self.observation_id != observation_id
            || self.format_version != reference.format_version
            || self.context != reference.context
            || self.nodes.len() != reference.node_count as usize
        {
            return Err(UiContractError::InvalidNodeCount(reference.node_count));
        }
        Ok(())
    }
}

/// Small Observation-side reference to a UI Tree Evidence object.
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct UiSnapshotRef {
    #[cfg_attr(feature = "schema", schemars(range(min = 1_u16, max = 1_u16)))]
    pub format_version: u16,
    pub context: UiContextRef,
    #[cfg_attr(feature = "schema", schemars(range(min = 1_u32, max = 10_000_u32)))]
    pub node_count: u32,
    #[serde(
        serialize_with = "crate::wire_integer::serialize_js_safe_u64",
        deserialize_with = "crate::wire_integer::deserialize_js_safe_u64"
    )]
    #[cfg_attr(feature = "schema", schemars(range(min = 1_u64, max = 786_432_u64)))]
    pub byte_length: u64,
    pub evidence: AssetRef,
}

impl UiSnapshotRef {
    pub fn validate(&self) -> Result<(), UiContractError> {
        if self.format_version != UI_SNAPSHOT_FORMAT_VERSION {
            return Err(UiContractError::UnsupportedSnapshotFormat(
                self.format_version,
            ));
        }
        self.context.validate()?;
        if self.node_count == 0 || self.node_count as usize > MAX_UI_SNAPSHOT_NODES {
            return Err(UiContractError::InvalidNodeCount(self.node_count));
        }
        if self.byte_length == 0 || self.byte_length > MAX_UI_SNAPSHOT_BYTES {
            return Err(UiContractError::InvalidByteLength(self.byte_length));
        }
        if self.evidence.media_type != UI_SNAPSHOT_MEDIA_TYPE {
            return Err(UiContractError::InvalidSnapshotMediaType(
                self.evidence.media_type.clone(),
            ));
        }
        Ok(())
    }
}

#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum UiSnapshotOmissionReason {
    DriverUnsupported,
    Policy,
    ProtectedAction,
}

/// Durable reference to one node in one observed UI Tree.
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct UiNodeRef {
    pub observation_id: Uuid,
    pub context: UiContextRef,
    #[cfg_attr(feature = "schema", schemars(length(min = 1, max = 4_096)))]
    pub stable_node_id: String,
}

impl UiNodeRef {
    pub fn validate(&self) -> Result<(), UiContractError> {
        self.context.validate()?;
        validate_required(
            &self.stable_node_id,
            "stableNodeId",
            MAX_UI_IDENTIFIER_LENGTH,
        )
    }
}

#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum TextMatchMode {
    #[default]
    Exact,
    Contains,
}

#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TextMatch {
    #[cfg_attr(feature = "schema", schemars(length(min = 1, max = 65_536)))]
    pub value: String,
    #[serde(default)]
    pub mode: TextMatchMode,
    #[serde(default = "default_true", skip_serializing_if = "is_true")]
    pub case_sensitive: bool,
}

/// Cross-channel selector. Native contexts use accessibility fields; web
/// contexts may additionally use CSS. Context selection never carries a stale
/// document epoch; resolved node references always carry the full context.
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ElementSelector {
    pub context: Option<UiContextSelector>,
    #[cfg_attr(feature = "schema", schemars(length(min = 1, max = 256)))]
    pub role: Option<String>,
    #[cfg_attr(feature = "schema", schemars(length(min = 1, max = 65_536)))]
    pub name: Option<String>,
    #[cfg_attr(feature = "schema", schemars(length(max = 65_536)))]
    pub value: Option<String>,
    #[cfg_attr(feature = "schema", schemars(length(min = 1, max = 4_096)))]
    pub identifier: Option<String>,
    pub text: Option<TextMatch>,
    #[cfg_attr(feature = "schema", schemars(length(min = 1, max = 65_536)))]
    pub css: Option<String>,
}

impl ElementSelector {
    pub fn is_empty(&self) -> bool {
        self.role.is_none()
            && self.name.is_none()
            && self.value.is_none()
            && self.identifier.is_none()
            && self.text.is_none()
            && self.css.is_none()
    }

    pub fn validate(&self) -> Result<(), UiContractError> {
        if self.is_empty() {
            return Err(UiContractError::EmptySelector);
        }
        if let Some(context) = &self.context {
            context.validate()?;
            if self.css.is_some() && context.context_kind == UiContextKind::Native {
                return Err(UiContractError::InvalidContextForCss);
            }
        }
        if let Some(role) = &self.role {
            validate_required(role, "role", MAX_UI_ROLE_LENGTH)?;
        }
        if let Some(name) = &self.name {
            validate_required(name, "name", MAX_UI_TEXT_LENGTH)?;
        }
        validate_optional(self.value.as_deref(), "value", MAX_UI_TEXT_LENGTH)?;
        if let Some(identifier) = &self.identifier {
            validate_required(identifier, "identifier", MAX_UI_IDENTIFIER_LENGTH)?;
        }
        if let Some(text) = &self.text {
            validate_required(&text.value, "text.value", MAX_UI_TEXT_LENGTH)?;
        }
        if let Some(css) = &self.css {
            validate_required(css, "css", MAX_UI_TEXT_LENGTH)?;
            if self.role.is_some()
                || self.name.is_some()
                || self.value.is_some()
                || self.identifier.is_some()
                || self.text.is_some()
            {
                return Err(UiContractError::InvalidContextForCss);
            }
            if !matches!(
                self.context.as_ref().map(|context| context.context_kind),
                Some(UiContextKind::Web)
            ) {
                return Err(UiContractError::InvalidContextForCss);
            }
        }
        Ok(())
    }
}

#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum ElementTarget {
    Selector { selector: ElementSelector },
    Node { node: UiNodeRef },
}

impl ElementTarget {
    pub fn validate(&self) -> Result<(), UiContractError> {
        match self {
            Self::Selector { selector } => selector.validate(),
            Self::Node { node } => node.validate(),
        }
    }
}

#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum CoordinateFallbackReason {
    SemanticInteractionUnavailable,
    PlatformLimitation,
}

#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(
    tag = "mode",
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum ActionExecution {
    NativeSemantic {
        context: UiContextRef,
    },
    WebSemantic {
        context: UiContextRef,
    },
    CoordinateFallback {
        context: UiContextRef,
        fallback_reason: CoordinateFallbackReason,
    },
}

impl ActionExecution {
    pub fn validate(&self) -> Result<(), UiContractError> {
        match self {
            Self::NativeSemantic { context } if context.context_kind == UiContextKind::Native => {
                context.validate()
            }
            Self::WebSemantic { context } if context.context_kind == UiContextKind::Web => {
                context.validate()
            }
            Self::CoordinateFallback { context, .. } => context.validate(),
            Self::NativeSemantic { .. } | Self::WebSemantic { .. } => {
                Err(UiContractError::InvalidSemanticExecutionContext)
            }
        }
    }
}

#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct FindElementArguments {
    pub selector: ElementSelector,
}

impl FindElementArguments {
    pub fn validate(&self) -> Result<(), UiContractError> {
        self.selector.validate()
    }

    pub fn into_action_call(self, id: Uuid) -> Result<ActionCall, serde_json::Error> {
        semantic_action_call(id, FIND_ELEMENT_ACTION, self)
    }
}

#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct FindElementResult {
    pub element: UiNodeRef,
}

#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TapElementArguments {
    pub target: ElementTarget,
}

impl TapElementArguments {
    pub fn validate(&self) -> Result<(), UiContractError> {
        self.target.validate()
    }

    pub fn into_action_call(self, id: Uuid) -> Result<ActionCall, serde_json::Error> {
        semantic_action_call(id, TAP_ELEMENT_ACTION, self)
    }
}

#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ClearElementArguments {
    pub target: ElementTarget,
}

impl ClearElementArguments {
    pub fn validate(&self) -> Result<(), UiContractError> {
        self.target.validate()
    }

    pub fn into_action_call(self, id: Uuid) -> Result<ActionCall, serde_json::Error> {
        semantic_action_call(id, CLEAR_ELEMENT_ACTION, self)
    }
}

#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SetElementValueArguments {
    pub target: ElementTarget,
    #[cfg_attr(feature = "schema", schemars(length(max = 65_536)))]
    pub value: String,
}

impl SetElementValueArguments {
    pub fn validate(&self) -> Result<(), UiContractError> {
        self.target.validate()?;
        validate_optional(Some(&self.value), "value", MAX_ELEMENT_VALUE_LENGTH)
    }

    pub fn into_action_call(self, id: Uuid) -> Result<ActionCall, serde_json::Error> {
        semantic_action_call(id, SET_ELEMENT_VALUE_ACTION, self)
    }
}

#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum WaitForElementCondition {
    #[default]
    Present,
    Visible,
    Enabled,
    Absent,
}

#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WaitForElementArguments {
    pub selector: ElementSelector,
    #[serde(default)]
    pub condition: WaitForElementCondition,
}

impl WaitForElementArguments {
    pub fn validate(&self) -> Result<(), UiContractError> {
        self.selector.validate()
    }

    pub fn into_action_call(self, id: Uuid) -> Result<ActionCall, serde_json::Error> {
        semantic_action_call(id, WAIT_FOR_ELEMENT_ACTION, self)
    }
}

#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ElementActionOutput {
    pub element: UiNodeRef,
}

pub type TapElementResult = ElementActionOutput;
pub type ClearElementResult = ElementActionOutput;
pub type SetElementValueResult = ElementActionOutput;

#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WaitForElementResult {
    pub matched: bool,
    pub condition: WaitForElementCondition,
    pub element: Option<UiNodeRef>,
}

impl WaitForElementResult {
    pub fn validate(&self) -> Result<(), UiContractError> {
        let valid = matches!(
            (self.matched, self.condition, self.element.is_some()),
            (false, _, false)
                | (true, WaitForElementCondition::Absent, false)
                | (true, WaitForElementCondition::Present, true)
                | (true, WaitForElementCondition::Visible, true)
                | (true, WaitForElementCondition::Enabled, true)
        );
        if !valid {
            return Err(UiContractError::InvalidWaitResult);
        }
        if let Some(element) = &self.element {
            element.validate()?;
        }
        Ok(())
    }
}

fn semantic_action_call<T: Serialize>(
    id: Uuid,
    name: &'static str,
    arguments: T,
) -> Result<ActionCall, serde_json::Error> {
    Ok(ActionCall {
        id,
        name: name.to_owned(),
        arguments: serde_json::to_value(arguments)?,
    })
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use uuid::Uuid;

    use super::*;

    fn context(kind: UiContextKind) -> UiContextRef {
        UiContextRef {
            context_kind: kind,
            context_id: match kind {
                UiContextKind::Native => "NATIVE_APP",
                UiContextKind::Web => "WEBVIEW_1",
            }
            .to_owned(),
            document_epoch: "epoch-1".to_owned(),
        }
    }

    fn node_ref(kind: UiContextKind) -> UiNodeRef {
        UiNodeRef {
            observation_id: Uuid::nil(),
            context: context(kind),
            stable_node_id: "button-7".to_owned(),
        }
    }

    #[test]
    fn element_targets_use_full_context_and_stable_node_id() {
        assert_eq!(
            serde_json::to_value(ElementTarget::Node {
                node: node_ref(UiContextKind::Web),
            })
            .expect("node target"),
            json!({
                "kind": "node",
                "node": {
                    "observationId": Uuid::nil(),
                    "context": {
                        "contextKind": "web",
                        "contextId": "WEBVIEW_1",
                        "documentEpoch": "epoch-1"
                    },
                    "stableNodeId": "button-7"
                }
            })
        );
    }

    #[test]
    fn selectors_reject_empty_and_native_css() {
        assert_eq!(
            ElementSelector::default().validate(),
            Err(UiContractError::EmptySelector)
        );
        let selector = ElementSelector {
            context: Some(UiContextSelector {
                context_kind: UiContextKind::Native,
                context_id: None,
            }),
            css: Some("button".to_owned()),
            ..ElementSelector::default()
        };
        assert_eq!(
            selector.validate(),
            Err(UiContractError::InvalidContextForCss)
        );
    }

    #[test]
    fn snapshot_requires_unique_normalized_preorder_nodes() {
        let snapshot = UiSnapshot {
            format_version: UI_SNAPSHOT_FORMAT_VERSION,
            observation_id: Uuid::nil(),
            context: context(UiContextKind::Native),
            root_stable_node_ids: vec!["root".to_owned()],
            nodes: vec![
                UiNode {
                    stable_node_id: "root".to_owned(),
                    parent_stable_node_id: None,
                    role: "application".to_owned(),
                    name: None,
                    value: None,
                    identifier: None,
                    text: None,
                    bounds: None,
                    enabled: Some(true),
                    hittable: None,
                },
                UiNode {
                    stable_node_id: "button".to_owned(),
                    parent_stable_node_id: Some("root".to_owned()),
                    role: "button".to_owned(),
                    name: Some("Search".to_owned()),
                    value: None,
                    identifier: Some("search-button".to_owned()),
                    text: Some("Search".to_owned()),
                    bounds: Some(UiRect {
                        x: 1.0,
                        y: 2.0,
                        width: 3.0,
                        height: 4.0,
                    }),
                    enabled: Some(true),
                    hittable: Some(true),
                },
            ],
        };
        snapshot.validate().expect("valid snapshot");

        let mut duplicate = snapshot.clone();
        duplicate.nodes[1].stable_node_id = "root".to_owned();
        assert!(matches!(
            duplicate.validate(),
            Err(UiContractError::DuplicateStableNodeId(_))
        ));
    }

    #[test]
    fn wait_absent_success_has_no_element() {
        let result = WaitForElementResult {
            matched: true,
            condition: WaitForElementCondition::Absent,
            element: None,
        };
        result.validate().expect("absent success");
        let invalid = WaitForElementResult {
            condition: WaitForElementCondition::Visible,
            ..result
        };
        assert_eq!(invalid.validate(), Err(UiContractError::InvalidWaitResult));
    }

    #[test]
    fn semantic_action_helpers_lock_names_without_action_timeouts() {
        let arguments = FindElementArguments {
            selector: ElementSelector {
                role: Some("button".to_owned()),
                ..ElementSelector::default()
            },
        };
        arguments.validate().expect("valid arguments");
        let call = arguments
            .into_action_call(Uuid::nil())
            .expect("build action call");
        assert_eq!(call.name, FIND_ELEMENT_ACTION);
        assert_eq!(
            call.arguments,
            json!({ "selector": { "context": null, "role": "button", "name": null, "value": null, "identifier": null, "text": null, "css": null } })
        );
        assert!(call.arguments.get("timeoutMs").is_none());
    }

    #[test]
    fn semantic_execution_mode_must_match_context_kind() {
        assert_eq!(
            ActionExecution::NativeSemantic {
                context: context(UiContextKind::Web),
            }
            .validate(),
            Err(UiContractError::InvalidSemanticExecutionContext)
        );
    }
}
