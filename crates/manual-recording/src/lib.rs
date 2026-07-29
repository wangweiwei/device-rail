//! Validation and deterministic replay compilation for portable human Action
//! recordings. This layer depends only on the wire protocol and never on a
//! concrete Driver, browser runtime, recorder UI, or Evidence Store.

use std::collections::{BTreeMap, BTreeSet};

use devicerail_protocol::{
    ActionCall, ActionDefinition, ActionProtection, MANUAL_RECORDING_VERSION,
    ManualActionArguments, ManualRecording,
};
use serde_json::Value;
use sha2::{Digest as _, Sha256};
use thiserror::Error;

const MAX_ACTIONS: usize = 256;
const MAX_ACTION_SPACE_BYTES: usize = 1024 * 1024;
const MAX_RECORDING_BYTES: usize = 16 * 1024 * 1024;
const MAX_ARGUMENT_BYTES: usize = 64 * 1024;

#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum ManualRecordingError {
    #[error("manual recording format version is unsupported")]
    UnsupportedVersion,
    #[error("manual recording structure is invalid ({0})")]
    InvalidRecording(&'static str),
    #[error("manual recording action space is invalid ({0})")]
    InvalidActionSpace(&'static str),
    #[error("manual recording action space changed")]
    ActionSpaceChanged,
    #[error("manual recording references an unknown action")]
    UnknownAction,
    #[error("manual recording argument protection does not match the action")]
    ProtectionMismatch,
    #[error("manual recording protected arguments are unavailable")]
    SecretUnavailable,
    #[error("manual recording arguments do not match the advertised schema")]
    InvalidArguments,
}

/// Computes the stable digest embedded in a manual recording.
pub fn action_space_digest(
    action_space: &[ActionDefinition],
) -> Result<String, ManualRecordingError> {
    validate_action_space(action_space)?;
    let bytes = serde_json::to_vec(action_space)
        .map_err(|_| ManualRecordingError::InvalidActionSpace("serialization_failed"))?;
    if bytes.len() > MAX_ACTION_SPACE_BYTES {
        return Err(ManualRecordingError::InvalidActionSpace("size_limit"));
    }
    Ok(hex::encode(Sha256::digest(bytes)))
}

/// Converts a validated manual recording to the exact `ActionCall` stream
/// accepted by the current Driver capability set.
///
/// `resolve_secret` receives only opaque references and returns complete
/// protected argument objects in memory. Returned values are never written
/// back into the recording.
pub fn compile_replay<F>(
    recording: &ManualRecording,
    action_space: &[ActionDefinition],
    mut resolve_secret: F,
) -> Result<Vec<ActionCall>, ManualRecordingError>
where
    F: FnMut(&str) -> Option<Value>,
{
    validate_recording(recording)?;
    if action_space_digest(action_space)? != recording.action_space_sha256 {
        return Err(ManualRecordingError::ActionSpaceChanged);
    }
    let actions = action_space
        .iter()
        .map(|action| (action.name.as_str(), action))
        .collect::<BTreeMap<_, _>>();
    let mut calls = Vec::with_capacity(recording.steps.len());
    for step in &recording.steps {
        let action = actions
            .get(step.name.as_str())
            .ok_or(ManualRecordingError::UnknownAction)?;
        let arguments = match (&action.protection, &step.arguments) {
            (ActionProtection::Standard, ManualActionArguments::Captured { value }) => {
                value.clone()
            }
            (ActionProtection::Protected, ManualActionArguments::Protected { secret_ref }) => {
                resolve_secret(secret_ref).ok_or(ManualRecordingError::SecretUnavailable)?
            }
            _ => return Err(ManualRecordingError::ProtectionMismatch),
        };
        if !arguments.is_object()
            || serde_json::to_vec(&arguments).map_or(true, |bytes| bytes.len() > MAX_ARGUMENT_BYTES)
        {
            return Err(ManualRecordingError::InvalidArguments);
        }
        let validator = jsonschema::validator_for(&action.input_schema)
            .map_err(|_| ManualRecordingError::InvalidActionSpace("invalid_schema"))?;
        if !validator.is_valid(&arguments) {
            return Err(ManualRecordingError::InvalidArguments);
        }
        calls.push(ActionCall {
            id: step.call_id,
            name: step.name.clone(),
            arguments,
        });
    }
    Ok(calls)
}

fn validate_action_space(action_space: &[ActionDefinition]) -> Result<(), ManualRecordingError> {
    if action_space.is_empty() || action_space.len() > MAX_ACTIONS {
        return Err(ManualRecordingError::InvalidActionSpace("action_count"));
    }
    let mut names = BTreeSet::new();
    for action in action_space {
        if action.name.is_empty()
            || action.name.len() > 128
            || !names.insert(action.name.as_str())
            || !action.input_schema.is_object()
            || jsonschema::meta::validate(&action.input_schema).is_err()
        {
            return Err(ManualRecordingError::InvalidActionSpace(
                "action_definition",
            ));
        }
    }
    Ok(())
}

fn validate_recording(recording: &ManualRecording) -> Result<(), ManualRecordingError> {
    if recording.format_version != MANUAL_RECORDING_VERSION {
        return Err(ManualRecordingError::UnsupportedVersion);
    }
    if recording.source_device_id.0.trim().is_empty()
        || recording.started_at_ms > recording.ended_at_ms
        || recording.action_space_sha256.len() != 64
        || !recording
            .action_space_sha256
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        || recording.steps.len() > 10_000
        || serde_json::to_vec(recording).map_or(true, |bytes| bytes.len() > MAX_RECORDING_BYTES)
    {
        return Err(ManualRecordingError::InvalidRecording("document"));
    }
    let mut call_ids = BTreeSet::new();
    let mut previous_at = recording.started_at_ms;
    for (index, step) in recording.steps.iter().enumerate() {
        let expected = u64::try_from(index + 1).expect("step count is bounded");
        let valid_secret_ref = match &step.arguments {
            ManualActionArguments::Captured { .. } => true,
            ManualActionArguments::Protected { secret_ref } => {
                !secret_ref.is_empty()
                    && secret_ref.len() <= 128
                    && secret_ref.bytes().all(|byte| {
                        byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.')
                    })
            }
        };
        if step.sequence.get() != expected
            || step.captured_at_ms < previous_at
            || step.captured_at_ms > recording.ended_at_ms
            || step.name.is_empty()
            || step.name.len() > 128
            || !call_ids.insert(step.call_id)
            || !valid_secret_ref
        {
            return Err(ManualRecordingError::InvalidRecording("step"));
        }
        previous_at = step.captured_at_ms;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use devicerail_protocol::{
        ActionDefinition, ActionProtection, DeviceId, EventSequence, MANUAL_RECORDING_VERSION,
        ManualActionArguments, ManualActionStep, ManualRecording,
    };
    use serde_json::json;
    use uuid::Uuid;

    use super::{ManualRecordingError, action_space_digest, compile_replay};

    fn actions() -> Vec<ActionDefinition> {
        vec![
            ActionDefinition {
                name: "click".to_owned(),
                description: "click".to_owned(),
                input_schema: json!({
                    "$schema": "https://json-schema.org/draft/2020-12/schema",
                    "type": "object",
                    "additionalProperties": false,
                    "properties": { "selector": { "type": "string", "minLength": 1 } },
                    "required": ["selector"]
                }),
                protection: ActionProtection::Standard,
            },
            ActionDefinition {
                name: "fillSecret".to_owned(),
                description: "secret".to_owned(),
                input_schema: json!({
                    "$schema": "https://json-schema.org/draft/2020-12/schema",
                    "type": "object",
                    "additionalProperties": false,
                    "properties": {
                        "selector": { "type": "string" },
                        "text": { "type": "string" }
                    },
                    "required": ["selector", "text"]
                }),
                protection: ActionProtection::Protected,
            },
        ]
    }

    fn recording_fixture() -> ManualRecording {
        let actions = actions();
        ManualRecording {
            format_version: MANUAL_RECORDING_VERSION,
            recording_id: Uuid::new_v4(),
            source_device_id: DeviceId::new("playwright-page"),
            action_space_sha256: action_space_digest(&actions).expect("digest"),
            started_at_ms: 10,
            ended_at_ms: 20,
            steps: vec![
                ManualActionStep {
                    sequence: EventSequence::FIRST,
                    captured_at_ms: 11,
                    call_id: Uuid::new_v4(),
                    name: "click".to_owned(),
                    arguments: ManualActionArguments::Captured {
                        value: json!({ "selector": "#submit" }),
                    },
                },
                ManualActionStep {
                    sequence: EventSequence::new(2).expect("sequence"),
                    captured_at_ms: 12,
                    call_id: Uuid::new_v4(),
                    name: "fillSecret".to_owned(),
                    arguments: ManualActionArguments::Protected {
                        secret_ref: "login.password".to_owned(),
                    },
                },
            ],
        }
    }

    #[test]
    fn compiles_standard_and_host_resolved_protected_actions() {
        const SECRET_VALUE: &str = "RESOLVED_DO_NOT_PERSIST";
        let recording = recording_fixture();
        let calls = compile_replay(&recording, &actions(), |key| {
            (key == "login.password")
                .then(|| json!({ "selector": "#password", "text": SECRET_VALUE }))
        })
        .expect("compile replay");
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0].name, "click");
        assert_eq!(calls[1].arguments["text"], SECRET_VALUE);
        assert!(
            !serde_json::to_string(&recording)
                .expect("recording JSON")
                .contains(SECRET_VALUE)
        );
    }

    #[test]
    fn fails_closed_on_action_space_drift_missing_secrets_and_bad_sequence() {
        let mut recording = recording_fixture();
        recording.action_space_sha256 = "0".repeat(64);
        assert_eq!(
            compile_replay(&recording, &actions(), |_| None),
            Err(ManualRecordingError::ActionSpaceChanged)
        );

        let mut recording = recording_fixture();
        assert_eq!(
            compile_replay(&recording, &actions(), |_| None),
            Err(ManualRecordingError::SecretUnavailable)
        );
        recording.steps[1].sequence = EventSequence::new(3).expect("sequence");
        assert_eq!(
            compile_replay(&recording, &actions(), |_| None),
            Err(ManualRecordingError::InvalidRecording("step"))
        );
    }

    #[test]
    fn rejects_protection_mismatch_and_arguments_outside_schema() {
        let mut recording = recording_fixture();
        recording.steps[0].arguments = ManualActionArguments::Protected {
            secret_ref: "not-secret-action".to_owned(),
        };
        assert_eq!(
            compile_replay(&recording, &actions(), |_| Some(json!({}))),
            Err(ManualRecordingError::ProtectionMismatch)
        );

        let mut recording = recording_fixture();
        recording.steps.truncate(1);
        recording.steps[0].arguments = ManualActionArguments::Captured { value: json!({}) };
        assert_eq!(
            compile_replay(&recording, &actions(), |_| None),
            Err(ManualRecordingError::InvalidArguments)
        );
    }
}
