use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

use crate::{DeviceId, EventSequence};

/// Version of the portable manual Action recording document.
pub const MANUAL_RECORDING_VERSION: u16 = 1;

/// A bounded, Driver-neutral record of human-selected Actions.
///
/// Protected Action arguments are represented only by an opaque host-owned
/// secret reference. The secret value is never part of this durable DTO.
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ManualRecording {
    pub format_version: u16,
    pub recording_id: Uuid,
    pub source_device_id: DeviceId,
    #[cfg_attr(feature = "schema", schemars(length(min = 64, max = 64)))]
    pub action_space_sha256: String,
    #[serde(
        serialize_with = "crate::wire_integer::serialize_js_safe_u64",
        deserialize_with = "crate::wire_integer::deserialize_js_safe_u64"
    )]
    #[cfg_attr(
        feature = "schema",
        schemars(range(min = 0_u64, max = 9_007_199_254_740_991_u64))
    )]
    pub started_at_ms: u64,
    #[serde(
        serialize_with = "crate::wire_integer::serialize_js_safe_u64",
        deserialize_with = "crate::wire_integer::deserialize_js_safe_u64"
    )]
    #[cfg_attr(
        feature = "schema",
        schemars(range(min = 0_u64, max = 9_007_199_254_740_991_u64))
    )]
    pub ended_at_ms: u64,
    #[cfg_attr(feature = "schema", schemars(length(max = 10_000)))]
    pub steps: Vec<ManualActionStep>,
}

#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ManualActionStep {
    pub sequence: EventSequence,
    #[serde(
        serialize_with = "crate::wire_integer::serialize_js_safe_u64",
        deserialize_with = "crate::wire_integer::deserialize_js_safe_u64"
    )]
    #[cfg_attr(
        feature = "schema",
        schemars(range(min = 0_u64, max = 9_007_199_254_740_991_u64))
    )]
    pub captured_at_ms: u64,
    pub call_id: Uuid,
    #[cfg_attr(feature = "schema", schemars(length(min = 1, max = 128)))]
    pub name: String,
    pub arguments: ManualActionArguments,
}

#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
#[serde(tag = "kind", rename_all = "camelCase", deny_unknown_fields)]
pub enum ManualActionArguments {
    /// Arguments safe to persist in the recording document.
    Captured { value: Value },
    /// Complete arguments supplied by the replay host at execution time.
    Protected {
        #[serde(rename = "secretRef")]
        #[cfg_attr(feature = "schema", schemars(length(min = 1, max = 128)))]
        secret_ref: String,
    },
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use uuid::Uuid;

    use super::{
        MANUAL_RECORDING_VERSION, ManualActionArguments, ManualActionStep, ManualRecording,
    };
    use crate::{DeviceId, EventSequence};

    #[test]
    fn protected_recording_contains_only_an_opaque_reference() {
        let recording = ManualRecording {
            format_version: MANUAL_RECORDING_VERSION,
            recording_id: Uuid::nil(),
            source_device_id: DeviceId::new("web-1"),
            action_space_sha256: "a".repeat(64),
            started_at_ms: 100,
            ended_at_ms: 101,
            steps: vec![ManualActionStep {
                sequence: EventSequence::FIRST,
                captured_at_ms: 101,
                call_id: Uuid::nil(),
                name: "fillSecret".to_owned(),
                arguments: ManualActionArguments::Protected {
                    secret_ref: "login-password".to_owned(),
                },
            }],
        };
        let value = serde_json::to_value(recording).expect("serialize recording");
        assert_eq!(value["formatVersion"], MANUAL_RECORDING_VERSION);
        assert_eq!(
            value["steps"][0]["arguments"],
            json!({ "kind": "protected", "secretRef": "login-password" })
        );
        assert!(!value.to_string().contains("password-value"));
    }
}
