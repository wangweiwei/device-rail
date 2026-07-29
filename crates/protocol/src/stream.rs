use std::fmt;

use serde::{Deserialize, Deserializer, Serialize};
use uuid::Uuid;

use crate::{
    ErrorInfo, EventSequence, JsonRpcVersion, RpcResponse, SessionId, SessionState, TestEvent,
};

/// Identifies one daemon process lifetime. A cursor from another epoch must
/// never be accepted as a resumable position.
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Deserialize, Serialize)]
#[serde(transparent)]
pub struct EventStreamEpoch(pub Uuid);

impl EventStreamEpoch {
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }
}

impl Default for EventStreamEpoch {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Display for EventStreamEpoch {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Deserialize, Serialize)]
#[serde(transparent)]
pub struct EventSubscriptionId(pub Uuid);

impl EventSubscriptionId {
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }
}

impl Default for EventSubscriptionId {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Display for EventSubscriptionId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

/// A Session-scoped, daemon-epoch-scoped application acknowledgement.
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EventStreamCursor {
    pub stream_epoch: EventStreamEpoch,
    pub session_id: SessionId,
    pub sequence: EventSequence,
}

/// Browser Origin policy bound to a single-use stream capability.
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(tag = "kind", rename_all = "camelCase", deny_unknown_fields)]
pub enum EventStreamOriginPolicy {
    /// Node and other non-browser clients must omit Origin entirely.
    Absent {},
    /// Browser clients must send exactly this Origin value.
    Exact { origin: String },
}

/// A short-lived bearer URL. Debug intentionally never exposes its contents.
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Clone, PartialEq, Eq, Serialize)]
#[serde(transparent)]
pub struct EventStreamEndpoint(
    #[cfg_attr(feature = "schema", schemars(length(min = 1, max = 2048)))] String,
);

impl EventStreamEndpoint {
    pub const MAX_BYTES: usize = 2_048;

    pub fn new(value: String) -> Option<Self> {
        (!value.is_empty() && value.len() <= Self::MAX_BYTES).then_some(Self(value))
    }

    /// Explicitly exposes the bearer URL to the transport that will consume it.
    pub fn expose_secret(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for EventStreamEndpoint {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("EventStreamEndpoint(<redacted>)")
    }
}

impl<'de> Deserialize<'de> for EventStreamEndpoint {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::new(value).ok_or_else(|| {
            serde::de::Error::custom(format!(
                "event stream endpoint must contain 1..={} bytes",
                Self::MAX_BYTES
            ))
        })
    }
}

#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EventsStreamOpenParams {
    pub session_id: SessionId,
    pub origin_policy: EventStreamOriginPolicy,
}

#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EventsStreamOpenResult {
    pub endpoint: EventStreamEndpoint,
    pub stream_epoch: EventStreamEpoch,
    #[serde(
        serialize_with = "crate::wire_integer::serialize_js_safe_u64",
        deserialize_with = "crate::wire_integer::deserialize_js_safe_u64"
    )]
    #[cfg_attr(
        feature = "schema",
        schemars(range(min = 0_u64, max = 9_007_199_254_740_991_u64))
    )]
    pub expires_at_ms: u64,
}

#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EventsSubscribeParams {
    pub session_id: SessionId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub after_cursor: Option<EventStreamCursor>,
}

#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EventsSubscribeResult {
    pub subscription_id: EventSubscriptionId,
    pub session_id: SessionId,
    pub replay_through: EventStreamCursor,
    pub session_state: SessionState,
}

#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize)]
pub enum EventsStreamEventMethod {
    #[serde(rename = "events.stream.event")]
    Event,
}

#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EventsStreamEventParams {
    pub subscription_id: EventSubscriptionId,
    pub cursor: EventStreamCursor,
    pub event: TestEvent,
}

#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EventsStreamEventNotification {
    pub jsonrpc: JsonRpcVersion,
    pub method: EventsStreamEventMethod,
    pub params: EventsStreamEventParams,
}

impl EventsStreamEventNotification {
    pub fn new(params: EventsStreamEventParams) -> Self {
        Self {
            jsonrpc: JsonRpcVersion::V2,
            method: EventsStreamEventMethod::Event,
            params,
        }
    }
}

/// The terminal reason is a closed tagged union so reason/error combinations
/// cannot become ambiguous on the wire.
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
#[serde(tag = "reason", rename_all = "camelCase", deny_unknown_fields)]
pub enum EventsStreamTermination {
    SessionEnded,
    Cancelled,
    SlowConsumer { error: ErrorInfo },
    SessionDeleted { error: ErrorInfo },
    ServerShutdown { error: ErrorInfo },
    SequenceGap { error: ErrorInfo },
    EventTooLarge { error: ErrorInfo },
    InternalError { error: ErrorInfo },
}

#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EventsStreamTerminalParams {
    pub subscription_id: EventSubscriptionId,
    pub session_id: SessionId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_emitted_cursor: Option<EventStreamCursor>,
    pub termination: EventsStreamTermination,
}

#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize)]
pub enum EventsStreamTerminalMethod {
    #[serde(rename = "events.stream.terminal")]
    Terminal,
}

#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EventsStreamTerminalNotification {
    pub jsonrpc: JsonRpcVersion,
    pub method: EventsStreamTerminalMethod,
    pub params: EventsStreamTerminalParams,
}

impl EventsStreamTerminalNotification {
    pub fn new(params: EventsStreamTerminalParams) -> Self {
        Self {
            jsonrpc: JsonRpcVersion::V2,
            method: EventsStreamTerminalMethod::Terminal,
            params,
        }
    }
}

#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
#[serde(untagged)]
pub enum RpcServerNotification {
    Event(EventsStreamEventNotification),
    Terminal(EventsStreamTerminalNotification),
}

#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
#[serde(untagged)]
pub enum RpcServerMessage {
    Response(RpcResponse),
    Notification(RpcServerNotification),
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{
        EventStreamEndpoint, EventStreamEpoch, EventStreamOriginPolicy, EventsStreamOpenParams,
        EventsStreamTermination,
    };
    use crate::SessionId;

    #[test]
    fn endpoint_debug_is_redacted_and_input_is_bounded() {
        let endpoint =
            EventStreamEndpoint::new("ws://127.0.0.1:1234/v/secret-capability".to_owned())
                .expect("valid endpoint");
        assert_eq!(format!("{endpoint:?}"), "EventStreamEndpoint(<redacted>)");
        assert!(!format!("{endpoint:?}").contains("secret-capability"));
        assert!(EventStreamEndpoint::new(String::new()).is_none());
        assert!(EventStreamEndpoint::new("x".repeat(EventStreamEndpoint::MAX_BYTES + 1)).is_none());
    }

    #[test]
    fn origin_policy_and_termination_are_closed_tagged_unions() {
        let params: EventsStreamOpenParams = serde_json::from_value(json!({
            "sessionId": SessionId::new(),
            "originPolicy": { "kind": "absent" }
        }))
        .expect("absent origin");
        assert_eq!(params.origin_policy, EventStreamOriginPolicy::Absent {});
        assert!(
            serde_json::from_value::<EventsStreamOpenParams>(json!({
                "sessionId": SessionId::new(),
                "originPolicy": { "kind": "absent", "origin": "null" }
            }))
            .is_err()
        );

        assert_eq!(
            serde_json::to_value(EventsStreamTermination::SessionEnded)
                .expect("serialize termination"),
            json!({ "reason": "sessionEnded" })
        );
        let _ = EventStreamEpoch::new();
    }
}
