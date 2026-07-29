use std::fmt;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

use crate::{ActionExecution, AssetRef, Observation, RequestTimeoutMs};

#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum ActionProtection {
    #[default]
    Standard,
    Protected,
}

impl ActionProtection {
    pub const fn is_standard(&self) -> bool {
        matches!(self, Self::Standard)
    }
}

const fn is_false(value: &bool) -> bool {
    !*value
}

#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ActionDefinition {
    pub name: String,
    pub description: String,
    // JSON Schema supplied by the driver for this action's argument object.
    // This stays structurally unconstrained on the wire because the driver
    // owns action-specific properties. Driver conformance validates the
    // declared dialect, self-contained references, and object root.
    pub input_schema: Value,
    #[serde(default, skip_serializing_if = "ActionProtection::is_standard")]
    pub protection: ActionProtection,
}

#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Clone, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ActionCall {
    pub id: Uuid,
    pub name: String,
    #[serde(default)]
    pub arguments: Value,
}

impl fmt::Debug for ActionCall {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ActionCall")
            .field("id", &self.id)
            .field("name", &self.name)
            .finish_non_exhaustive()
    }
}

/// Durable representation of an Action invocation.
///
/// Standard calls preserve the historical wire shape. Protected and unknown
/// calls retain only correlation fields and serialize `arguments` as `null`
/// with an explicit `argumentsRedacted` marker.
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Clone, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RecordedActionCall {
    pub id: Uuid,
    pub name: String,
    #[serde(default)]
    pub arguments: Value,
    #[serde(default, skip_serializing_if = "is_false")]
    pub arguments_redacted: bool,
}

impl RecordedActionCall {
    pub fn from_action_call(call: &ActionCall, protection: Option<ActionProtection>) -> Self {
        let is_standard = matches!(protection, Some(ActionProtection::Standard));
        Self {
            id: call.id,
            name: call.name.clone(),
            arguments: if is_standard {
                call.arguments.clone()
            } else {
                Value::Null
            },
            arguments_redacted: !is_standard,
        }
    }
}

impl fmt::Debug for RecordedActionCall {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RecordedActionCall")
            .field("id", &self.id)
            .field("name", &self.name)
            .field("arguments_redacted", &self.arguments_redacted)
            .finish_non_exhaustive()
    }
}

/// Parameters for `device.execute`.
///
/// The action fields intentionally remain flat on the wire. The optional
/// timeout controls only the Driver action, while the request envelope timeout
/// controls the request-scoped device-operation budget. Durable terminal event
/// finalization is shielded so cancellation cannot leave a half-open Action.
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Clone, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DeviceExecuteParams {
    pub id: Uuid,
    pub name: String,
    #[serde(default)]
    pub arguments: Value,
    #[serde(
        default,
        deserialize_with = "crate::rpc::deserialize_optional_timeout",
        skip_serializing_if = "Option::is_none"
    )]
    #[cfg_attr(feature = "schema", schemars(with = "RequestTimeoutMs"))]
    pub action_timeout_ms: Option<RequestTimeoutMs>,
}

impl fmt::Debug for DeviceExecuteParams {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("DeviceExecuteParams")
            .field("id", &self.id)
            .field("name", &self.name)
            .field("action_timeout_ms", &self.action_timeout_ms)
            .finish_non_exhaustive()
    }
}

impl DeviceExecuteParams {
    pub fn into_action_call(self) -> ActionCall {
        ActionCall {
            id: self.id,
            name: self.name,
            arguments: self.arguments,
        }
    }
}

#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ActionResult {
    pub call_id: Uuid,
    #[serde(
        serialize_with = "crate::wire_integer::serialize_js_safe_u64",
        deserialize_with = "crate::wire_integer::deserialize_js_safe_u64"
    )]
    #[cfg_attr(feature = "schema", schemars(range(max = 9_007_199_254_740_991_u64)))]
    pub started_at_ms: u64,
    #[serde(
        serialize_with = "crate::wire_integer::serialize_js_safe_u64",
        deserialize_with = "crate::wire_integer::deserialize_js_safe_u64"
    )]
    #[cfg_attr(feature = "schema", schemars(range(max = 9_007_199_254_740_991_u64)))]
    pub finished_at_ms: u64,
    pub output: Value,
    pub before: Option<Observation>,
    pub after: Option<Observation>,
    #[serde(default)]
    pub evidence: Vec<AssetRef>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub execution: Option<ActionExecution>,
}

impl ActionResult {
    /// Returns every typed Evidence reference reachable from this result.
    pub fn asset_refs(&self) -> impl Iterator<Item = &AssetRef> {
        self.evidence
            .iter()
            .chain(self.before.iter().flat_map(Observation::asset_refs))
            .chain(self.after.iter().flat_map(Observation::asset_refs))
    }
}

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};
    use uuid::Uuid;

    use super::{
        ActionCall, ActionDefinition, ActionProtection, DeviceExecuteParams, RecordedActionCall,
    };
    use crate::RequestTimeoutMs;

    #[test]
    fn execute_params_preserve_the_flat_action_wire_shape() {
        let value = json!({
            "id": "00000000-0000-0000-0000-000000000000",
            "name": "tap",
            "arguments": { "x": 10, "y": 20 }
        });
        let params: DeviceExecuteParams =
            serde_json::from_value(value.clone()).expect("legacy flat execute params");
        assert!(params.action_timeout_ms.is_none());
        let call = params.clone().into_action_call();
        assert_eq!(call.id, params.id);
        assert_eq!(call.name, params.name);
        assert_eq!(call.arguments, params.arguments);
        assert_eq!(
            serde_json::to_value(params).expect("serialize execute params"),
            value
        );
    }

    #[test]
    fn execute_params_validate_timeout_and_unknown_fields() {
        let valid: DeviceExecuteParams = serde_json::from_value(json!({
            "id": "00000000-0000-0000-0000-000000000000",
            "name": "tap",
            "actionTimeoutMs": RequestTimeoutMs::MAX
        }))
        .expect("maximum action timeout");
        assert_eq!(
            valid.action_timeout_ms.map(RequestTimeoutMs::get),
            Some(RequestTimeoutMs::MAX)
        );

        for timeout in [json!(null), json!(0), json!(RequestTimeoutMs::MAX + 1)] {
            assert!(
                serde_json::from_value::<DeviceExecuteParams>(json!({
                    "id": "00000000-0000-0000-0000-000000000000",
                    "name": "tap",
                    "actionTimeoutMs": timeout
                }))
                .is_err()
            );
        }
        assert!(
            serde_json::from_value::<DeviceExecuteParams>(json!({
                "id": "00000000-0000-0000-0000-000000000000",
                "name": "tap",
                "timeoutMs": 100
            }))
            .is_err()
        );
    }

    #[test]
    fn protection_is_additive_and_standard_preserves_the_legacy_wire_shape() {
        let standard = ActionDefinition {
            name: "tap".to_owned(),
            description: "Tap".to_owned(),
            input_schema: json!({ "type": "object" }),
            protection: ActionProtection::Standard,
        };
        assert_eq!(
            serde_json::to_value(&standard).expect("standard definition"),
            json!({
                "name": "tap",
                "description": "Tap",
                "inputSchema": { "type": "object" }
            })
        );
        let restored: ActionDefinition = serde_json::from_value(json!({
            "name": "tap",
            "description": "Tap",
            "inputSchema": { "type": "object" }
        }))
        .expect("legacy definition");
        assert_eq!(restored.protection, ActionProtection::Standard);

        let protected = ActionDefinition {
            protection: ActionProtection::Protected,
            ..standard
        };
        assert_eq!(
            serde_json::to_value(protected).expect("protected definition")["protection"],
            "protected"
        );
    }

    #[test]
    fn recorded_calls_preserve_standard_arguments_and_explicitly_redact_protected_or_unknown() {
        let call = ActionCall {
            id: Uuid::nil(),
            name: "inputSecret".to_owned(),
            arguments: json!({ "text": "DEVICERAIL_SECRET_SENTINEL" }),
        };
        let standard =
            RecordedActionCall::from_action_call(&call, Some(ActionProtection::Standard));
        assert_eq!(
            serde_json::to_value(standard).expect("standard call"),
            json!({
                "id": Uuid::nil(),
                "name": "inputSecret",
                "arguments": { "text": "DEVICERAIL_SECRET_SENTINEL" }
            })
        );

        for protection in [Some(ActionProtection::Protected), None] {
            let recorded = RecordedActionCall::from_action_call(&call, protection);
            assert!(recorded.arguments.is_null());
            assert!(recorded.arguments_redacted);
            let value = serde_json::to_value(recorded).expect("redacted call");
            assert_eq!(value["arguments"], json!(null));
            assert_eq!(value["argumentsRedacted"], true);
            assert!(!value.to_string().contains("DEVICERAIL_SECRET_SENTINEL"));
        }

        let standard_null = RecordedActionCall::from_action_call(
            &ActionCall {
                id: Uuid::nil(),
                name: "tap".to_owned(),
                arguments: Value::Null,
            },
            Some(ActionProtection::Standard),
        );
        let encoded = serde_json::to_value(&standard_null).expect("standard null call");
        assert_eq!(encoded["arguments"], Value::Null);
        assert!(encoded.get("argumentsRedacted").is_none());
        let decoded: RecordedActionCall =
            serde_json::from_value(encoded).expect("standard null call round trip");
        assert!(decoded.arguments.is_null());
        assert!(!decoded.arguments_redacted);

        let legacy_missing: RecordedActionCall = serde_json::from_value(json!({
            "id": Uuid::nil(),
            "name": "tap"
        }))
        .expect("legacy missing arguments remain accepted");
        assert!(legacy_missing.arguments.is_null());
        assert!(!legacy_missing.arguments_redacted);
        assert_eq!(
            serde_json::to_value(legacy_missing).expect("normalize legacy call")["arguments"],
            Value::Null
        );
    }

    #[test]
    fn action_debug_views_never_render_argument_values() {
        const SENTINEL: &str = "DEVICERAIL_SECRET_DEBUG_SENTINEL";
        let call = ActionCall {
            id: Uuid::nil(),
            name: "inputSecret".to_owned(),
            arguments: json!({ "text": SENTINEL }),
        };
        let params = DeviceExecuteParams {
            id: call.id,
            name: call.name.clone(),
            arguments: call.arguments.clone(),
            action_timeout_ms: None,
        };
        assert!(!format!("{call:?}").contains(SENTINEL));
        assert!(!format!("{params:?}").contains(SENTINEL));
    }
}
