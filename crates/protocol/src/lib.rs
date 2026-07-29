mod action;
mod device;
mod event;
#[cfg(feature = "fixtures")]
pub mod fixtures;
mod handshake;
mod manual;
mod methods;
mod rpc;
#[cfg(feature = "schema")]
pub mod schema;
mod stream;
mod ui;
mod wire_integer;

pub use action::{
    ActionCall, ActionDefinition, ActionProtection, ActionResult, DeviceExecuteParams,
    RecordedActionCall,
};
pub use device::{
    AssetRef, DeviceId, DeviceInfo, DeviceSelectParams, DeviceSelectResult, DevicesListResult,
    Observation, Platform, ScreenshotOmissionReason, Viewport,
};
pub use event::{
    ActionOutcome, ErrorInfo, EventId, EventSequence, MAX_VERDICT_EVIDENCE_REFERENCES,
    MAX_VERDICT_SUMMARY_LENGTH, MediaFrame, MediaStreamId, MediaStreamInfo, MediaStreamKind,
    SessionExport, SessionId, SessionInfo, SessionOutcome, SessionState, TestEvent,
    TestEventPayload, Verdict, VerdictStatus, VerdictValidationError,
};
pub use handshake::{
    FeatureNegotiationError, FeatureOffer, FeatureSelection, HelloParams, HelloResult,
    PROTOCOL_VERSION, PeerInfo, ProtocolIncompatibilityReason, ProtocolNegotiationError,
    ProtocolOffer, ProtocolRange, ProtocolSelection, ProtocolVersion, TransportInfo, feature,
    negotiate_features, negotiate_protocol, supported_protocol_offer,
};
pub use manual::{
    MANUAL_RECORDING_VERSION, ManualActionArguments, ManualActionStep, ManualRecording,
};
pub use methods::{
    DeviceCapabilitiesResult, DeviceConnectResult, DeviceDisconnectResult, DeviceExecuteResult,
    DeviceObserveResult, EventsClearResult, EventsListParams, EventsListResult,
    MAX_EVENTS_LIST_PAGE_SIZE, MediaStreamCaptureParams, MediaStreamCaptureResult,
    MediaStreamEndParams, MediaStreamEndResult, MediaStreamStartParams, MediaStreamStartResult,
    SessionCurrentResult, SessionEndParams, SessionEndResult, SessionExportParams,
    SessionExportResult, SessionStartResult, SessionTargetParams, SessionsListResult,
    SystemDescribeResult, UiSnapshotGetParams, UiSnapshotGetResult, VerdictRecordParams,
    VerdictRecordResult,
};
pub use rpc::{
    JsonRpcVersion, MAX_SAFE_INTEGER_ID, RequestCancelParams, RequestCancelResult,
    RequestCancelStatus, RequestTimeoutMs, RpcError, RpcId, RpcParams, RpcRequest, RpcResponse,
};
pub use stream::{
    EventStreamCursor, EventStreamEndpoint, EventStreamEpoch, EventStreamOriginPolicy,
    EventSubscriptionId, EventsStreamEventMethod, EventsStreamEventNotification,
    EventsStreamEventParams, EventsStreamOpenParams, EventsStreamOpenResult,
    EventsStreamTerminalMethod, EventsStreamTerminalNotification, EventsStreamTerminalParams,
    EventsStreamTermination, EventsSubscribeParams, EventsSubscribeResult, RpcServerMessage,
    RpcServerNotification,
};
pub use ui::{
    ActionExecution, CLEAR_ELEMENT_ACTION, ClearElementArguments, ClearElementResult,
    CoordinateFallbackReason, ElementActionOutput, ElementSelector, ElementTarget,
    FIND_ELEMENT_ACTION, FindElementArguments, FindElementResult, MAX_UI_IDENTIFIER_LENGTH,
    MAX_UI_ROLE_LENGTH, MAX_UI_SNAPSHOT_BYTES, MAX_UI_SNAPSHOT_NODES, MAX_UI_TEXT_LENGTH,
    SEMANTIC_ACTION_NAMES, SET_ELEMENT_VALUE_ACTION, SetElementValueArguments,
    SetElementValueResult, TAP_ELEMENT_ACTION, TapElementArguments, TapElementResult, TextMatch,
    TextMatchMode, UI_SNAPSHOT_FORMAT_VERSION, UI_SNAPSHOT_MEDIA_TYPE, UiContextKind, UiContextRef,
    UiContextSelector, UiContractError, UiNode, UiNodeRef, UiRect, UiSnapshot,
    UiSnapshotOmissionReason, UiSnapshotRef, WAIT_FOR_ELEMENT_ACTION, WaitForElementArguments,
    WaitForElementCondition, WaitForElementResult, is_semantic_action_name,
};
pub use wire_integer::{MAX_SAFE_INTEGER, json_integer_as_i32, json_integer_as_u32};

#[cfg(test)]
mod tests {
    use serde_json::json;
    use uuid::Uuid;

    use super::{
        ActionCall, EventId, EventSequence, HelloParams, HelloResult, JsonRpcVersion, RpcId,
        RpcParams, RpcRequest, RpcResponse, SessionId, TestEvent, TestEventPayload,
    };

    #[test]
    fn action_call_uses_camel_case_json() {
        let call = ActionCall {
            id: Uuid::nil(),
            name: "tap".to_owned(),
            arguments: json!({ "x": 10, "y": 20 }),
        };

        let value = serde_json::to_value(call).expect("serialize action call");
        assert_eq!(value["id"], Uuid::nil().to_string());
        assert_eq!(value["arguments"]["x"], 10);
    }

    #[test]
    fn rpc_request_round_trips() {
        let request = RpcRequest {
            jsonrpc: JsonRpcVersion::V2,
            id: RpcId::Number(42),
            method: "device.observe".to_owned(),
            timeout_ms: None,
            params: Some(RpcParams::Object(Default::default())),
        };

        let serialized = serde_json::to_string(&request).expect("serialize request");
        let restored: RpcRequest = serde_json::from_str(&serialized).expect("restore request");
        assert_eq!(restored, request);
    }

    #[test]
    fn event_type_is_explicit() {
        let event = TestEvent {
            event_id: EventId::from(Uuid::nil()),
            session_id: SessionId::from(Uuid::nil()),
            sequence: EventSequence::FIRST,
            request_id: None,
            device_id: None,
            at_ms: 1,
            payload: TestEventPayload::SessionStarted,
        };

        let value = serde_json::to_value(event).expect("serialize event");
        assert_eq!(value["payload"]["type"], "sessionStarted");
        assert_eq!(value["atMs"], 1);
    }

    #[test]
    fn system_hello_golden_fixtures_round_trip() {
        let request_value: serde_json::Value =
            serde_json::from_str(include_str!("../fixtures/system-hello-v1.request.json"))
                .expect("read hello request fixture");
        let request: RpcRequest = serde_json::from_value(request_value.clone())
            .expect("deserialize hello request fixture");
        let hello_params: HelloParams =
            serde_json::from_value(request.params.clone().expect("hello params").into_value())
                .expect("deserialize typed hello params");
        assert_eq!(
            serde_json::to_value(hello_params).expect("serialize typed hello params"),
            request_value["params"]
        );
        assert_eq!(
            serde_json::to_value(request).expect("serialize hello request fixture"),
            request_value
        );

        let response_value: serde_json::Value =
            serde_json::from_str(include_str!("../fixtures/system-hello-v1.response.json"))
                .expect("read hello response fixture");
        let response: RpcResponse = serde_json::from_value(response_value.clone())
            .expect("deserialize hello response fixture");
        let hello_result: HelloResult =
            serde_json::from_value(response.result().expect("hello result").clone())
                .expect("deserialize typed hello result");
        assert_eq!(
            serde_json::to_value(hello_result).expect("serialize typed hello result"),
            response_value["result"]
        );
        assert_eq!(
            serde_json::to_value(response).expect("serialize hello response fixture"),
            response_value
        );
    }
}
