use std::fmt;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

use crate::{ActionResult, AssetRef, DeviceId, Observation, RecordedActionCall, RpcId, Viewport};

/// Maximum Unicode code points in a persisted Verdict summary.
pub const MAX_VERDICT_SUMMARY_LENGTH: usize = 16 * 1024;
/// Maximum typed Evidence references attached to one Verdict.
pub const MAX_VERDICT_EVIDENCE_REFERENCES: usize = 64;

const fn is_false(value: &bool) -> bool {
    !*value
}

#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Deserialize, Serialize)]
#[serde(transparent)]
pub struct SessionId(pub Uuid);

impl SessionId {
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }
}

impl Default for SessionId {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Display for SessionId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

impl From<Uuid> for SessionId {
    fn from(value: Uuid) -> Self {
        Self(value)
    }
}

#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Deserialize, Serialize)]
#[serde(transparent)]
pub struct EventId(pub Uuid);

impl EventId {
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }
}

impl Default for EventId {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Display for EventId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

impl From<Uuid> for EventId {
    fn from(value: Uuid) -> Self {
        Self(value)
    }
}

#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Deserialize, Serialize)]
#[serde(transparent)]
pub struct MediaStreamId(pub Uuid);

impl MediaStreamId {
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }
}

impl Default for MediaStreamId {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Display for MediaStreamId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

impl From<Uuid> for MediaStreamId {
    fn from(value: Uuid) -> Self {
        Self(value)
    }
}

/// A one-based sequence number within one session.
///
/// The wire value is capped at JavaScript's maximum safe integer so generated
/// clients can sort and resume event streams without losing precision.
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Deserialize, Serialize)]
#[serde(transparent)]
pub struct EventSequence(
    #[serde(
        serialize_with = "crate::wire_integer::serialize_js_safe_u64",
        deserialize_with = "deserialize_event_sequence"
    )]
    #[cfg_attr(
        feature = "schema",
        schemars(range(min = 1_u64, max = 9_007_199_254_740_991_u64))
    )]
    u64,
);

fn deserialize_event_sequence<'de, D>(deserializer: D) -> Result<u64, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = crate::wire_integer::deserialize_js_safe_u64(deserializer)?;
    if value == 0 {
        Err(serde::de::Error::custom(
            "event sequence must be a one-based integer",
        ))
    } else {
        Ok(value)
    }
}

impl EventSequence {
    pub const FIRST: Self = Self(1);

    pub fn new(value: u64) -> Option<Self> {
        (value > 0 && value <= crate::MAX_SAFE_INTEGER).then_some(Self(value))
    }

    pub const fn get(self) -> u64 {
        self.0
    }

    pub fn checked_next(self) -> Option<Self> {
        self.0.checked_add(1).and_then(Self::new)
    }
}

#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum VerdictStatus {
    Pass,
    Fail,
    Unknown,
}

#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Verdict {
    pub status: VerdictStatus,
    #[cfg_attr(feature = "schema", schemars(length(min = 1, max = 16_384)))]
    pub summary: String,
    #[serde(default)]
    #[cfg_attr(feature = "schema", schemars(length(max = 64)))]
    pub evidence: Vec<AssetRef>,
}

impl Verdict {
    pub fn validate(&self) -> Result<(), VerdictValidationError> {
        if self.summary.trim().is_empty() {
            return Err(VerdictValidationError::EmptySummary);
        }
        let summary_length = self.summary.chars().count();
        if summary_length > MAX_VERDICT_SUMMARY_LENGTH {
            return Err(VerdictValidationError::SummaryTooLong {
                actual: summary_length,
                maximum: MAX_VERDICT_SUMMARY_LENGTH,
            });
        }
        if self.evidence.len() > MAX_VERDICT_EVIDENCE_REFERENCES {
            return Err(VerdictValidationError::TooManyEvidenceReferences {
                actual: self.evidence.len(),
                maximum: MAX_VERDICT_EVIDENCE_REFERENCES,
            });
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VerdictValidationError {
    EmptySummary,
    SummaryTooLong { actual: usize, maximum: usize },
    TooManyEvidenceReferences { actual: usize, maximum: usize },
}

impl fmt::Display for VerdictValidationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptySummary => formatter.write_str("verdict summary must not be blank"),
            Self::SummaryTooLong { actual, maximum } => write!(
                formatter,
                "verdict summary contains {actual} Unicode code points; maximum is {maximum}"
            ),
            Self::TooManyEvidenceReferences { actual, maximum } => write!(
                formatter,
                "verdict contains {actual} Evidence references; maximum is {maximum}"
            ),
        }
    }
}

impl std::error::Error for VerdictValidationError {}

#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ErrorInfo {
    pub code: String,
    pub message: String,
    pub retryable: bool,
    pub details: Option<Value>,
}

#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum SessionState {
    Active,
    Ended,
}

#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum SessionOutcome {
    Completed,
    Failed,
    Cancelled,
    Shutdown,
}

#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SessionInfo {
    pub id: SessionId,
    pub state: SessionState,
    #[serde(
        serialize_with = "crate::wire_integer::serialize_js_safe_u64",
        deserialize_with = "crate::wire_integer::deserialize_js_safe_u64"
    )]
    #[cfg_attr(feature = "schema", schemars(range(max = 9_007_199_254_740_991_u64)))]
    pub started_at_ms: u64,
    #[serde(
        default,
        serialize_with = "crate::wire_integer::serialize_optional_js_safe_u64",
        deserialize_with = "crate::wire_integer::deserialize_optional_js_safe_u64"
    )]
    #[cfg_attr(feature = "schema", schemars(with = "Option<SafeWireIntegerSchema>"))]
    pub ended_at_ms: Option<u64>,
    pub event_count: EventSequence,
    pub last_sequence: EventSequence,
}

#[cfg(feature = "schema")]
#[allow(dead_code)]
#[derive(schemars::JsonSchema)]
#[schemars(inline)]
struct SafeWireIntegerSchema(#[schemars(range(max = 9_007_199_254_740_991_u64))] u64);

#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SessionExport {
    pub session: SessionInfo,
    pub events: Vec<TestEvent>,
}

#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum MediaStreamKind {
    Screenshot,
    Video,
}

#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MediaStreamInfo {
    pub id: MediaStreamId,
    pub kind: MediaStreamKind,
    #[cfg_attr(feature = "schema", schemars(length(min = 1, max = 255)))]
    pub media_type: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub viewport: Option<Viewport>,
}

#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MediaFrame {
    pub stream_id: MediaStreamId,
    pub frame_index: EventSequence,
    #[serde(default, skip_serializing_if = "is_false")]
    pub key_frame: bool,
    #[serde(
        default,
        serialize_with = "crate::wire_integer::serialize_optional_js_safe_u64",
        deserialize_with = "crate::wire_integer::deserialize_optional_js_safe_u64",
        skip_serializing_if = "Option::is_none"
    )]
    #[cfg_attr(feature = "schema", schemars(with = "Option<SafeWireIntegerSchema>"))]
    pub duration_ms: Option<u64>,
    pub evidence: AssetRef,
}

/// The terminal outcome for one action call.
///
/// Keeping the four outcomes structurally distinct prevents clients from
/// having to infer timeout or cancellation from human-readable error text.
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
#[serde(
    tag = "outcome",
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum ActionOutcome {
    Succeeded {
        result: Box<ActionResult>,
    },
    Failed {
        error: ErrorInfo,
    },
    Cancelled {
        error: ErrorInfo,
    },
    TimedOut {
        error: ErrorInfo,
        #[serde(
            serialize_with = "crate::wire_integer::serialize_js_safe_u64",
            deserialize_with = "crate::wire_integer::deserialize_js_safe_u64"
        )]
        #[cfg_attr(feature = "schema", schemars(range(max = 9_007_199_254_740_991_u64)))]
        timeout_ms: u64,
    },
}

#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum TestEventPayload {
    SessionStarted,
    SessionEnded {
        outcome: SessionOutcome,
        reason: Option<String>,
    },
    ObservationCaptured {
        observation: Box<Observation>,
    },
    ActionStarted {
        call: RecordedActionCall,
    },
    ActionCompleted {
        call_id: Uuid,
        outcome: ActionOutcome,
    },
    MediaStreamStarted {
        stream: MediaStreamInfo,
    },
    MediaFrameCaptured {
        frame: MediaFrame,
    },
    MediaStreamEnded {
        stream_id: MediaStreamId,
        #[serde(
            serialize_with = "crate::wire_integer::serialize_js_safe_u64",
            deserialize_with = "crate::wire_integer::deserialize_js_safe_u64"
        )]
        #[cfg_attr(feature = "schema", schemars(range(max = 9_007_199_254_740_991_u64)))]
        frame_count: u64,
    },
    VerdictRecorded {
        verdict: Verdict,
    },
    Error {
        error: ErrorInfo,
    },
}

impl TestEventPayload {
    /// Lowest Protocol 1.x minor that can represent this payload without
    /// silently dropping additive fields.
    pub fn required_protocol_minor(&self) -> u16 {
        match self {
            Self::MediaStreamStarted { .. }
            | Self::MediaFrameCaptured { .. }
            | Self::MediaStreamEnded { .. } => 4,
            Self::ObservationCaptured { observation }
                if observation.ui_snapshot.is_some()
                    || observation.ui_snapshot_omission.is_some() =>
            {
                5
            }
            Self::ActionCompleted {
                outcome: ActionOutcome::Succeeded { result },
                ..
            } if result.execution.is_some()
                || result
                    .before
                    .iter()
                    .chain(result.after.iter())
                    .any(|observation| {
                        observation.ui_snapshot.is_some()
                            || observation.ui_snapshot_omission.is_some()
                    }) =>
            {
                5
            }
            _ => 0,
        }
    }
}

#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TestEvent {
    pub event_id: EventId,
    pub session_id: SessionId,
    pub sequence: EventSequence,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request_id: Option<RpcId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub device_id: Option<DeviceId>,
    #[serde(
        serialize_with = "crate::wire_integer::serialize_js_safe_u64",
        deserialize_with = "crate::wire_integer::deserialize_js_safe_u64"
    )]
    #[cfg_attr(feature = "schema", schemars(range(max = 9_007_199_254_740_991_u64)))]
    pub at_ms: u64,
    pub payload: TestEventPayload,
}

impl TestEvent {
    pub fn required_protocol_minor(&self) -> u16 {
        self.payload.required_protocol_minor()
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use uuid::Uuid;

    use super::{
        ActionOutcome, ErrorInfo, EventId, EventSequence, MAX_VERDICT_EVIDENCE_REFERENCES,
        MAX_VERDICT_SUMMARY_LENGTH, SessionId, TestEvent, TestEventPayload, Verdict, VerdictStatus,
        VerdictValidationError,
    };
    use crate::AssetRef;

    #[test]
    fn envelope_nests_payload_and_has_js_safe_sequence() {
        let event = TestEvent {
            event_id: EventId::from(Uuid::nil()),
            session_id: SessionId::from(Uuid::nil()),
            sequence: EventSequence::FIRST,
            request_id: None,
            device_id: None,
            at_ms: 1,
            payload: TestEventPayload::SessionStarted,
        };

        assert_eq!(
            serde_json::to_value(event).expect("serialize event"),
            json!({
                "eventId": Uuid::nil(),
                "sessionId": Uuid::nil(),
                "sequence": 1,
                "atMs": 1,
                "payload": {
                    "type": "sessionStarted"
                }
            })
        );
        assert!(EventSequence::new(crate::MAX_SAFE_INTEGER).is_some());
        assert!(EventSequence::new(crate::MAX_SAFE_INTEGER + 1).is_none());
        assert!(serde_json::from_value::<EventSequence>(json!(0)).is_err());
        assert!(
            serde_json::from_value::<EventSequence>(json!(crate::MAX_SAFE_INTEGER + 1)).is_err()
        );
    }

    #[test]
    fn action_outcomes_have_explicit_wire_status() {
        let outcome = ActionOutcome::Cancelled {
            error: ErrorInfo {
                code: "action_cancelled".to_owned(),
                message: "cancelled".to_owned(),
                retryable: false,
                details: None,
            },
        };
        assert_eq!(
            serde_json::to_value(outcome).expect("serialize outcome")["outcome"],
            "cancelled"
        );
    }

    #[test]
    fn verdict_validation_uses_schema_aligned_unicode_and_evidence_bounds() {
        let mut verdict = Verdict {
            status: VerdictStatus::Unknown,
            summary: "🧪".repeat(MAX_VERDICT_SUMMARY_LENGTH),
            evidence: Vec::new(),
        };
        verdict.validate().expect("maximum Unicode length");

        verdict.summary.push('x');
        assert_eq!(
            verdict.validate(),
            Err(VerdictValidationError::SummaryTooLong {
                actual: MAX_VERDICT_SUMMARY_LENGTH + 1,
                maximum: MAX_VERDICT_SUMMARY_LENGTH,
            })
        );

        verdict.summary = " \n\t".to_owned();
        assert_eq!(
            verdict.validate(),
            Err(VerdictValidationError::EmptySummary)
        );

        verdict.summary = "bounded".to_owned();
        verdict.evidence = vec![
            AssetRef {
                id: "asset".to_owned(),
                media_type: "image/png".to_owned(),
                uri: "evidence://asset".to_owned(),
                sha256: None,
            };
            MAX_VERDICT_EVIDENCE_REFERENCES + 1
        ];
        assert_eq!(
            verdict.validate(),
            Err(VerdictValidationError::TooManyEvidenceReferences {
                actual: MAX_VERDICT_EVIDENCE_REFERENCES + 1,
                maximum: MAX_VERDICT_EVIDENCE_REFERENCES,
            })
        );
    }
}
