use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::{
    ActionDefinition, ActionResult, DeviceId, DeviceInfo, EventSequence, HelloResult, MediaFrame,
    MediaStreamId, MediaStreamInfo, MediaStreamKind, Observation, PeerInfo, SessionExport,
    SessionId, SessionInfo, SessionOutcome, TestEvent, UiSnapshot, Verdict,
};

/// Largest page accepted by the snapshot-style `events.list` RPC.
///
/// Omitting `limit` preserves the pre-pagination response shape and behavior.
/// Clients that need bounded recovery should provide a limit and advance
/// `afterSequence` to the last returned event until a short or empty page is
/// observed.
pub const MAX_EVENTS_LIST_PAGE_SIZE: u32 = 1_000;

/// Result returned by `system.describe`.
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SystemDescribeResult {
    pub connection: HelloResult,
    pub client: PeerInfo,
    pub device_id: Option<DeviceId>,
    pub active_session_id: Option<SessionId>,
}

/// Result returned by `device.connect`.
pub type DeviceConnectResult = DeviceInfo;

/// Result returned by `device.disconnect`.
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DeviceDisconnectResult {
    pub disconnected: bool,
}

/// Result returned by `device.capabilities`.
pub type DeviceCapabilitiesResult = Vec<ActionDefinition>;

/// Result returned by `device.observe`.
pub type DeviceObserveResult = Observation;

/// Result returned by `device.execute`.
pub type DeviceExecuteResult = ActionResult;

/// Parameters accepted by `ui.snapshot.get`.
///
/// The active Session owns the Observation lookup; callers cannot name a
/// different Session or provide an arbitrary Evidence reference.
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct UiSnapshotGetParams {
    pub observation_id: uuid::Uuid,
}

/// Result returned by `ui.snapshot.get`.
pub type UiSnapshotGetResult = UiSnapshot;

/// Parameters accepted by `verdict.record`.
///
/// DeviceRail persists this caller-supplied Verdict; it does not infer or
/// upgrade the verdict status.
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct VerdictRecordParams {
    pub verdict: Verdict,
}

/// Result returned by `verdict.record` after the durable event append.
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct VerdictRecordResult {
    pub event: TestEvent,
}

/// Parameters accepted by `media.stream.start`.
///
/// The caller chooses only a lifetime-unique stream identifier and the logical
/// stream kind. The daemon assigns the media type from its selected, leased
/// device producer; viewport metadata may be absent. This control method never
/// accepts frame bytes, Evidence references, or filesystem paths.
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MediaStreamStartParams {
    pub stream_id: MediaStreamId,
    pub kind: MediaStreamKind,
}

/// Result returned by `media.stream.start`.
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MediaStreamStartResult {
    pub stream: MediaStreamInfo,
}

/// Parameters accepted by `media.stream.capture`.
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MediaStreamCaptureParams {
    pub stream_id: MediaStreamId,
    /// Caller-declared one-based frame index. Retrying the same accepted index
    /// is idempotent; advancing it is the caller's acknowledgement boundary.
    pub frame_index: EventSequence,
    #[serde(
        default,
        serialize_with = "crate::wire_integer::serialize_optional_js_safe_u64",
        deserialize_with = "crate::wire_integer::deserialize_optional_js_safe_u64",
        skip_serializing_if = "Option::is_none"
    )]
    #[cfg_attr(
        feature = "schema",
        schemars(range(min = 0_u64, max = 9_007_199_254_740_991_u64))
    )]
    pub duration_ms: Option<u64>,
}

/// Result returned by `media.stream.capture`.
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MediaStreamCaptureResult {
    pub frame: MediaFrame,
}

/// Parameters accepted by `media.stream.end`.
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MediaStreamEndParams {
    pub stream_id: MediaStreamId,
}

/// Result returned by `media.stream.end`.
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MediaStreamEndResult {
    pub stream_id: MediaStreamId,
    #[serde(
        serialize_with = "crate::wire_integer::serialize_js_safe_u64",
        deserialize_with = "crate::wire_integer::deserialize_js_safe_u64"
    )]
    #[cfg_attr(
        feature = "schema",
        schemars(range(min = 0_u64, max = 9_007_199_254_740_991_u64))
    )]
    pub frame_count: u64,
}

/// Result returned by `session.start`.
pub type SessionStartResult = SessionInfo;

/// Result returned by `session.current`.
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SessionCurrentResult {
    pub session_id: SessionId,
}

/// Optional parameters accepted by `session.end`.
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SessionEndParams {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outcome: Option<SessionOutcome>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

/// Result returned by `session.end`.
pub type SessionEndResult = SessionInfo;

/// Result returned by `sessions.list`.
pub type SessionsListResult = Vec<SessionInfo>;

/// Optional session selector accepted by `events.clear`.
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SessionTargetParams {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<SessionId>,
}

/// Optional parameters accepted by `session.export`.
///
/// Omitting both cursor fields preserves the original complete-export
/// behavior. Supplying `limit` requests a bounded page after
/// `afterSequence`; `afterSequence` without `limit` is invalid at dispatch.
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SessionExportParams {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<SessionId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub after_sequence: Option<EventSequence>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        serialize_with = "serialize_optional_event_page_limit",
        deserialize_with = "deserialize_optional_event_page_limit"
    )]
    #[cfg_attr(feature = "schema", schemars(range(min = 1_u32, max = 1_000_u32)))]
    pub limit: Option<u32>,
}

/// Result returned by `session.export`.
///
/// `nextAfterSequence` is absent for a legacy complete export and for the
/// final page. A paged response includes it only when another page remains.
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SessionExportResult {
    pub session: SessionInfo,
    pub events: Vec<TestEvent>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_after_sequence: Option<EventSequence>,
}

impl From<SessionExport> for SessionExportResult {
    fn from(export: SessionExport) -> Self {
        Self {
            session: export.session,
            events: export.events,
            next_after_sequence: None,
        }
    }
}

/// Optional parameters accepted by `events.list`.
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EventsListParams {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<SessionId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub after_sequence: Option<EventSequence>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        serialize_with = "serialize_optional_event_page_limit",
        deserialize_with = "deserialize_optional_event_page_limit"
    )]
    #[cfg_attr(feature = "schema", schemars(range(min = 1_u32, max = 1_000_u32)))]
    pub limit: Option<u32>,
}

/// Result returned by `events.list`.
pub type EventsListResult = Vec<TestEvent>;

fn serialize_optional_event_page_limit<S>(
    value: &Option<u32>,
    serializer: S,
) -> Result<S::Ok, S::Error>
where
    S: Serializer,
{
    match value {
        Some(value) if (1..=MAX_EVENTS_LIST_PAGE_SIZE).contains(value) => {
            serializer.serialize_some(value)
        }
        Some(_) => Err(serde::ser::Error::custom(format!(
            "event page limit must be between 1 and {MAX_EVENTS_LIST_PAGE_SIZE}"
        ))),
        None => serializer.serialize_none(),
    }
}

fn deserialize_optional_event_page_limit<'de, D>(deserializer: D) -> Result<Option<u32>, D::Error>
where
    D: Deserializer<'de>,
{
    let value = Option::<u32>::deserialize(deserializer)?;
    match value {
        Some(value) if !(1..=MAX_EVENTS_LIST_PAGE_SIZE).contains(&value) => {
            Err(serde::de::Error::custom(format!(
                "event page limit must be between 1 and {MAX_EVENTS_LIST_PAGE_SIZE}"
            )))
        }
        value => Ok(value),
    }
}

/// Result returned by `events.clear`.
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EventsClearResult {
    pub deleted: bool,
    pub session_id: SessionId,
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use uuid::Uuid;

    use super::{
        EventsClearResult, EventsListParams, MAX_EVENTS_LIST_PAGE_SIZE, MediaStreamCaptureParams,
        MediaStreamEndParams, MediaStreamEndResult, MediaStreamStartParams, SessionEndParams,
        SessionExportParams, SessionExportResult, SessionTargetParams,
    };
    use crate::{EventSequence, MediaStreamId, MediaStreamKind, SessionId, SessionOutcome};

    #[test]
    fn optional_method_params_are_strict_and_use_camel_case() {
        let session_id = SessionId::from(Uuid::nil());
        let end: SessionEndParams = serde_json::from_value(json!({
            "outcome": "cancelled",
            "reason": "stopped by client"
        }))
        .expect("session.end params");
        assert_eq!(end.outcome, Some(SessionOutcome::Cancelled));
        assert_eq!(
            serde_json::to_value(SessionTargetParams {
                session_id: Some(session_id.clone()),
            })
            .expect("session target params"),
            json!({ "sessionId": Uuid::nil() })
        );
        assert_eq!(
            serde_json::to_value(SessionExportParams {
                session_id: Some(session_id.clone()),
                after_sequence: EventSequence::new(4),
                limit: Some(25),
            })
            .expect("session.export params"),
            json!({ "sessionId": Uuid::nil(), "afterSequence": 4, "limit": 25 })
        );
        assert_eq!(
            serde_json::to_value(SessionExportParams {
                session_id: Some(session_id.clone()),
                after_sequence: None,
                limit: None,
            })
            .expect("legacy session.export params"),
            json!({ "sessionId": Uuid::nil() })
        );
        assert_eq!(
            serde_json::to_value(EventsListParams {
                session_id: Some(session_id.clone()),
                after_sequence: EventSequence::new(4),
                limit: Some(25),
            })
            .expect("events.list params"),
            json!({ "sessionId": Uuid::nil(), "afterSequence": 4, "limit": 25 })
        );

        assert!(serde_json::from_value::<SessionEndParams>(json!({ "unexpected": true })).is_err());
        assert!(
            serde_json::from_value::<SessionTargetParams>(json!({ "session_id": Uuid::nil() }))
                .is_err()
        );
        assert!(serde_json::from_value::<EventsListParams>(json!({ "afterSequence": 0 })).is_err());
        assert!(serde_json::from_value::<EventsListParams>(json!({ "limit": 0 })).is_err());
        assert!(serde_json::from_value::<SessionExportParams>(json!({ "limit": 0 })).is_err());
        assert!(
            serde_json::from_value::<EventsListParams>(
                json!({ "limit": MAX_EVENTS_LIST_PAGE_SIZE + 1 })
            )
            .is_err()
        );
        assert!(
            serde_json::from_value::<SessionExportParams>(
                json!({ "limit": MAX_EVENTS_LIST_PAGE_SIZE + 1 })
            )
            .is_err()
        );

        let stream_id = MediaStreamId::from(Uuid::nil());
        assert_eq!(
            serde_json::to_value(MediaStreamStartParams {
                stream_id: stream_id.clone(),
                kind: MediaStreamKind::Screenshot,
            })
            .expect("media.stream.start params"),
            json!({ "streamId": Uuid::nil(), "kind": "screenshot" })
        );
        assert_eq!(
            serde_json::to_value(MediaStreamCaptureParams {
                stream_id: stream_id.clone(),
                frame_index: EventSequence::FIRST,
                duration_ms: Some(crate::MAX_SAFE_INTEGER),
            })
            .expect("media.stream.capture params"),
            json!({
                "streamId": Uuid::nil(),
                "frameIndex": 1,
                "durationMs": crate::MAX_SAFE_INTEGER
            })
        );
        assert_eq!(
            serde_json::to_value(MediaStreamEndParams {
                stream_id: stream_id.clone(),
            })
            .expect("media.stream.end params"),
            json!({ "streamId": Uuid::nil() })
        );
        assert!(
            serde_json::from_value::<MediaStreamCaptureParams>(json!({
                "streamId": Uuid::nil(),
                "frameIndex": 1,
                "durationMs": crate::MAX_SAFE_INTEGER + 1
            }))
            .is_err()
        );
        assert!(
            serde_json::from_value::<MediaStreamStartParams>(json!({
                "streamId": Uuid::nil(),
                "kind": "screenshot",
                "mediaType": "image/png"
            }))
            .is_err()
        );
    }

    #[test]
    fn method_results_are_strict_and_use_camel_case() {
        let result = EventsClearResult {
            deleted: true,
            session_id: SessionId::from(Uuid::nil()),
        };
        assert_eq!(
            serde_json::to_value(result).expect("events.clear result"),
            json!({ "deleted": true, "sessionId": Uuid::nil() })
        );
        assert!(
            serde_json::from_value::<EventsClearResult>(json!({
                "deleted": true,
                "sessionId": Uuid::nil(),
                "unexpected": true
            }))
            .is_err()
        );

        let export = SessionExportResult {
            session: crate::SessionInfo {
                id: SessionId::from(Uuid::nil()),
                state: crate::SessionState::Ended,
                started_at_ms: 1,
                ended_at_ms: Some(2),
                event_count: EventSequence::new(2).expect("event count"),
                last_sequence: EventSequence::new(2).expect("last sequence"),
            },
            events: Vec::new(),
            next_after_sequence: None,
        };
        let value = serde_json::to_value(export).expect("legacy session.export result");
        let object = value.as_object().expect("export object");
        assert_eq!(object.len(), 2);
        assert!(object.contains_key("session"));
        assert!(object.contains_key("events"));
        assert!(!object.contains_key("nextAfterSequence"));

        let stream_id = MediaStreamId::from(Uuid::nil());
        assert_eq!(
            serde_json::to_value(MediaStreamEndResult {
                stream_id: stream_id.clone(),
                frame_count: 0,
            })
            .expect("zero-frame media.stream.end result"),
            json!({ "streamId": Uuid::nil(), "frameCount": 0 })
        );
        assert!(
            serde_json::to_value(MediaStreamEndResult {
                stream_id,
                frame_count: crate::MAX_SAFE_INTEGER + 1,
            })
            .is_err()
        );
    }
}
