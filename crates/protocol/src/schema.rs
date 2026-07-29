//! Deterministic JSON Schema documents for the public wire DTOs.
//!
//! This module is only available with the opt-in `schema` feature. Runtime
//! protocol consumers do not need to compile the schema generator.

use schemars::{JsonSchema, generate::SchemaSettings};
use serde_json::Value;

use crate::{
    ActionCall, ActionDefinition, ActionExecution, ActionOutcome, ActionProtection, ActionResult,
    AssetRef, ClearElementArguments, ClearElementResult, CoordinateFallbackReason,
    DeviceCapabilitiesResult, DeviceConnectResult, DeviceDisconnectResult, DeviceExecuteParams,
    DeviceExecuteResult, DeviceId, DeviceInfo, DeviceObserveResult, DeviceSelectParams,
    DeviceSelectResult, DevicesListResult, ElementSelector, ElementTarget, ErrorInfo, EventId,
    EventSequence, EventStreamCursor, EventStreamEndpoint, EventStreamEpoch,
    EventStreamOriginPolicy, EventSubscriptionId, EventsClearResult, EventsListParams,
    EventsListResult, EventsStreamEventNotification, EventsStreamEventParams,
    EventsStreamOpenParams, EventsStreamOpenResult, EventsStreamTerminalNotification,
    EventsStreamTerminalParams, EventsStreamTermination, EventsSubscribeParams,
    EventsSubscribeResult, FeatureOffer, FeatureSelection, FindElementArguments, FindElementResult,
    HelloParams, HelloResult, JsonRpcVersion, ManualActionArguments, ManualActionStep,
    ManualRecording, MediaFrame, MediaStreamCaptureParams, MediaStreamCaptureResult,
    MediaStreamEndParams, MediaStreamEndResult, MediaStreamId, MediaStreamInfo, MediaStreamKind,
    MediaStreamStartParams, MediaStreamStartResult, Observation, PeerInfo, Platform,
    ProtocolIncompatibilityReason, ProtocolOffer, ProtocolRange, ProtocolSelection,
    ProtocolVersion, RecordedActionCall, RequestCancelParams, RequestCancelResult,
    RequestCancelStatus, RequestTimeoutMs, RpcError, RpcId, RpcParams, RpcRequest, RpcResponse,
    RpcServerMessage, RpcServerNotification, ScreenshotOmissionReason, SessionCurrentResult,
    SessionEndParams, SessionEndResult, SessionExport, SessionExportParams, SessionExportResult,
    SessionId, SessionInfo, SessionOutcome, SessionStartResult, SessionState, SessionTargetParams,
    SessionsListResult, SetElementValueArguments, SetElementValueResult, SystemDescribeResult,
    TapElementArguments, TapElementResult, TestEvent, TestEventPayload, TextMatch, TextMatchMode,
    TransportInfo, UiContextKind, UiContextRef, UiContextSelector, UiNode, UiNodeRef, UiRect,
    UiSnapshot, UiSnapshotGetParams, UiSnapshotGetResult, UiSnapshotOmissionReason, UiSnapshotRef,
    Verdict, VerdictRecordParams, VerdictRecordResult, VerdictStatus, Viewport,
    WaitForElementArguments, WaitForElementCondition, WaitForElementResult,
};

pub const SCHEMA_SET_VERSION: u16 = 1;
pub const SCHEMA_DIRECTORY: &str = "v1";
pub const JSON_SCHEMA_DRAFT: &str = "https://json-schema.org/draft/2020-12/schema";

#[derive(Clone, Debug, PartialEq)]
pub struct SchemaDocument {
    pub name: &'static str,
    pub file_name: &'static str,
    pub schema_id: String,
    pub schema: Value,
}

/// Generates every public protocol schema in stable file-name order.
pub fn documents() -> Vec<SchemaDocument> {
    vec![
        document::<ActionCall>("ActionCall", "action-call.schema.json"),
        document::<ActionDefinition>("ActionDefinition", "action-definition.schema.json"),
        document::<ActionExecution>("ActionExecution", "action-execution.schema.json"),
        document::<ActionOutcome>("ActionOutcome", "action-outcome.schema.json"),
        document::<ActionProtection>("ActionProtection", "action-protection.schema.json"),
        document::<ActionResult>("ActionResult", "action-result.schema.json"),
        document::<AssetRef>("AssetRef", "asset-ref.schema.json"),
        document::<ClearElementArguments>(
            "ClearElementArguments",
            "clear-element-arguments.schema.json",
        ),
        document::<ClearElementResult>("ClearElementResult", "clear-element-result.schema.json"),
        document::<CoordinateFallbackReason>(
            "CoordinateFallbackReason",
            "coordinate-fallback-reason.schema.json",
        ),
        document::<DeviceCapabilitiesRequestSchema>(
            "DeviceCapabilitiesRequest",
            "device-capabilities-request.schema.json",
        ),
        document::<DeviceCapabilitiesResponseSchema>(
            "DeviceCapabilitiesResponse",
            "device-capabilities-response.schema.json",
        ),
        document::<DeviceCapabilitiesResult>(
            "DeviceCapabilitiesResult",
            "device-capabilities-result.schema.json",
        ),
        document::<DeviceConnectRequestSchema>(
            "DeviceConnectRequest",
            "device-connect-request.schema.json",
        ),
        document::<DeviceConnectResponseSchema>(
            "DeviceConnectResponse",
            "device-connect-response.schema.json",
        ),
        document::<DeviceConnectResult>("DeviceConnectResult", "device-connect-result.schema.json"),
        document::<DeviceDisconnectRequestSchema>(
            "DeviceDisconnectRequest",
            "device-disconnect-request.schema.json",
        ),
        document::<DeviceDisconnectResponseSchema>(
            "DeviceDisconnectResponse",
            "device-disconnect-response.schema.json",
        ),
        document::<DeviceDisconnectResult>(
            "DeviceDisconnectResult",
            "device-disconnect-result.schema.json",
        ),
        document::<DeviceExecuteParams>("DeviceExecuteParams", "device-execute-params.schema.json"),
        document::<DeviceExecuteRequestSchema>(
            "DeviceExecuteRequest",
            "device-execute-request.schema.json",
        ),
        document::<DeviceExecuteResponseSchema>(
            "DeviceExecuteResponse",
            "device-execute-response.schema.json",
        ),
        document::<DeviceExecuteResult>("DeviceExecuteResult", "device-execute-result.schema.json"),
        document::<DeviceId>("DeviceId", "device-id.schema.json"),
        document::<DeviceInfo>("DeviceInfo", "device-info.schema.json"),
        document::<DeviceObserveRequestSchema>(
            "DeviceObserveRequest",
            "device-observe-request.schema.json",
        ),
        document::<DeviceObserveResponseSchema>(
            "DeviceObserveResponse",
            "device-observe-response.schema.json",
        ),
        document::<DeviceObserveResult>("DeviceObserveResult", "device-observe-result.schema.json"),
        document::<DeviceSelectParams>("DeviceSelectParams", "device-select-params.schema.json"),
        document::<DeviceSelectRequestSchema>(
            "DeviceSelectRequest",
            "device-select-request.schema.json",
        ),
        document::<DeviceSelectResponseSchema>(
            "DeviceSelectResponse",
            "device-select-response.schema.json",
        ),
        document::<DeviceSelectResult>("DeviceSelectResult", "device-select-result.schema.json"),
        document::<DevicesListRequestSchema>(
            "DevicesListRequest",
            "devices-list-request.schema.json",
        ),
        document::<DevicesListResponseSchema>(
            "DevicesListResponse",
            "devices-list-response.schema.json",
        ),
        document::<DevicesListResult>("DevicesListResult", "devices-list-result.schema.json"),
        document::<ElementSelector>("ElementSelector", "element-selector.schema.json"),
        document::<ElementTarget>("ElementTarget", "element-target.schema.json"),
        document::<ErrorInfo>("ErrorInfo", "error-info.schema.json"),
        document::<EventId>("EventId", "event-id.schema.json"),
        document::<EventSequence>("EventSequence", "event-sequence.schema.json"),
        document::<EventStreamCursor>("EventStreamCursor", "event-stream-cursor.schema.json"),
        document::<EventStreamEndpoint>("EventStreamEndpoint", "event-stream-endpoint.schema.json"),
        document::<EventStreamEpoch>("EventStreamEpoch", "event-stream-epoch.schema.json"),
        document::<EventStreamOriginPolicy>(
            "EventStreamOriginPolicy",
            "event-stream-origin-policy.schema.json",
        ),
        document::<EventSubscriptionId>("EventSubscriptionId", "event-subscription-id.schema.json"),
        document::<EventsClearRequestSchema>(
            "EventsClearRequest",
            "events-clear-request.schema.json",
        ),
        document::<EventsClearResponseSchema>(
            "EventsClearResponse",
            "events-clear-response.schema.json",
        ),
        document::<EventsClearResult>("EventsClearResult", "events-clear-result.schema.json"),
        document::<EventsListParams>("EventsListParams", "events-list-params.schema.json"),
        document::<EventsListRequestSchema>("EventsListRequest", "events-list-request.schema.json"),
        document::<EventsListResponseSchema>(
            "EventsListResponse",
            "events-list-response.schema.json",
        ),
        document::<EventsListResult>("EventsListResult", "events-list-result.schema.json"),
        document::<EventsStreamEventNotification>(
            "EventsStreamEventNotification",
            "events-stream-event-notification.schema.json",
        ),
        document::<EventsStreamEventParams>(
            "EventsStreamEventParams",
            "events-stream-event-params.schema.json",
        ),
        document::<EventsStreamOpenParams>(
            "EventsStreamOpenParams",
            "events-stream-open-params.schema.json",
        ),
        document::<EventsStreamOpenRequestSchema>(
            "EventsStreamOpenRequest",
            "events-stream-open-request.schema.json",
        ),
        document::<EventsStreamOpenResponseSchema>(
            "EventsStreamOpenResponse",
            "events-stream-open-response.schema.json",
        ),
        document::<EventsStreamOpenResult>(
            "EventsStreamOpenResult",
            "events-stream-open-result.schema.json",
        ),
        document::<EventsStreamTerminalNotification>(
            "EventsStreamTerminalNotification",
            "events-stream-terminal-notification.schema.json",
        ),
        document::<EventsStreamTerminalParams>(
            "EventsStreamTerminalParams",
            "events-stream-terminal-params.schema.json",
        ),
        document::<EventsStreamTermination>(
            "EventsStreamTermination",
            "events-stream-termination.schema.json",
        ),
        document::<EventsSubscribeParams>(
            "EventsSubscribeParams",
            "events-subscribe-params.schema.json",
        ),
        document::<EventsSubscribeRequestSchema>(
            "EventsSubscribeRequest",
            "events-subscribe-request.schema.json",
        ),
        document::<EventsSubscribeResponseSchema>(
            "EventsSubscribeResponse",
            "events-subscribe-response.schema.json",
        ),
        document::<EventsSubscribeResult>(
            "EventsSubscribeResult",
            "events-subscribe-result.schema.json",
        ),
        document::<FeatureOffer>("FeatureOffer", "feature-offer.schema.json"),
        document::<FeatureSelection>("FeatureSelection", "feature-selection.schema.json"),
        document::<FindElementArguments>(
            "FindElementArguments",
            "find-element-arguments.schema.json",
        ),
        document::<FindElementResult>("FindElementResult", "find-element-result.schema.json"),
        document::<HelloParams>("HelloParams", "hello-params.schema.json"),
        document::<HelloResult>("HelloResult", "hello-result.schema.json"),
        document::<JsonRpcVersion>("JsonRpcVersion", "json-rpc-version.schema.json"),
        document::<ManualActionArguments>(
            "ManualActionArguments",
            "manual-action-arguments.schema.json",
        ),
        document::<ManualActionStep>("ManualActionStep", "manual-action-step.schema.json"),
        document::<ManualRecording>("ManualRecording", "manual-recording.schema.json"),
        document::<MediaFrame>("MediaFrame", "media-frame.schema.json"),
        document::<MediaStreamCaptureParams>(
            "MediaStreamCaptureParams",
            "media-stream-capture-params.schema.json",
        ),
        document::<MediaStreamCaptureRequestSchema>(
            "MediaStreamCaptureRequest",
            "media-stream-capture-request.schema.json",
        ),
        document::<MediaStreamCaptureResponseSchema>(
            "MediaStreamCaptureResponse",
            "media-stream-capture-response.schema.json",
        ),
        document::<MediaStreamCaptureResult>(
            "MediaStreamCaptureResult",
            "media-stream-capture-result.schema.json",
        ),
        document::<MediaStreamEndParams>(
            "MediaStreamEndParams",
            "media-stream-end-params.schema.json",
        ),
        document::<MediaStreamEndRequestSchema>(
            "MediaStreamEndRequest",
            "media-stream-end-request.schema.json",
        ),
        document::<MediaStreamEndResponseSchema>(
            "MediaStreamEndResponse",
            "media-stream-end-response.schema.json",
        ),
        document::<MediaStreamEndResult>(
            "MediaStreamEndResult",
            "media-stream-end-result.schema.json",
        ),
        document::<MediaStreamId>("MediaStreamId", "media-stream-id.schema.json"),
        document::<MediaStreamInfo>("MediaStreamInfo", "media-stream-info.schema.json"),
        document::<MediaStreamKind>("MediaStreamKind", "media-stream-kind.schema.json"),
        document::<MediaStreamStartParams>(
            "MediaStreamStartParams",
            "media-stream-start-params.schema.json",
        ),
        document::<MediaStreamStartRequestSchema>(
            "MediaStreamStartRequest",
            "media-stream-start-request.schema.json",
        ),
        document::<MediaStreamStartResponseSchema>(
            "MediaStreamStartResponse",
            "media-stream-start-response.schema.json",
        ),
        document::<MediaStreamStartResult>(
            "MediaStreamStartResult",
            "media-stream-start-result.schema.json",
        ),
        document::<Observation>("Observation", "observation.schema.json"),
        document::<PeerInfo>("PeerInfo", "peer-info.schema.json"),
        document::<Platform>("Platform", "platform.schema.json"),
        document::<ProtocolIncompatibilityReason>(
            "ProtocolIncompatibilityReason",
            "protocol-incompatibility-reason.schema.json",
        ),
        document::<ProtocolOffer>("ProtocolOffer", "protocol-offer.schema.json"),
        document::<ProtocolRange>("ProtocolRange", "protocol-range.schema.json"),
        document::<ProtocolSelection>("ProtocolSelection", "protocol-selection.schema.json"),
        document::<ProtocolVersion>("ProtocolVersion", "protocol-version.schema.json"),
        document::<RecordedActionCall>("RecordedActionCall", "recorded-action-call.schema.json"),
        document::<RequestCancelParams>("RequestCancelParams", "request-cancel-params.schema.json"),
        document::<RequestCancelRequestSchema>(
            "RequestCancelRequest",
            "request-cancel-request.schema.json",
        ),
        document::<RequestCancelResponseSchema>(
            "RequestCancelResponse",
            "request-cancel-response.schema.json",
        ),
        document::<RequestCancelResult>("RequestCancelResult", "request-cancel-result.schema.json"),
        document::<RequestCancelStatus>("RequestCancelStatus", "request-cancel-status.schema.json"),
        document::<RequestTimeoutMs>("RequestTimeoutMs", "request-timeout-ms.schema.json"),
        document::<RpcError>("RpcError", "rpc-error.schema.json"),
        document::<RpcId>("RpcId", "rpc-id.schema.json"),
        document::<RpcParams>("RpcParams", "rpc-params.schema.json"),
        document::<RpcRequest>("RpcRequest", "rpc-request.schema.json"),
        document::<RpcResponse>("RpcResponse", "rpc-response.schema.json"),
        document::<RpcServerMessage>("RpcServerMessage", "rpc-server-message.schema.json"),
        document::<RpcServerNotification>(
            "RpcServerNotification",
            "rpc-server-notification.schema.json",
        ),
        document::<ScreenshotOmissionReason>(
            "ScreenshotOmissionReason",
            "screenshot-omission-reason.schema.json",
        ),
        document::<SessionCurrentRequestSchema>(
            "SessionCurrentRequest",
            "session-current-request.schema.json",
        ),
        document::<SessionCurrentResponseSchema>(
            "SessionCurrentResponse",
            "session-current-response.schema.json",
        ),
        document::<SessionCurrentResult>(
            "SessionCurrentResult",
            "session-current-result.schema.json",
        ),
        document::<SessionEndParams>("SessionEndParams", "session-end-params.schema.json"),
        document::<SessionEndRequestSchema>("SessionEndRequest", "session-end-request.schema.json"),
        document::<SessionEndResponseSchema>(
            "SessionEndResponse",
            "session-end-response.schema.json",
        ),
        document::<SessionEndResult>("SessionEndResult", "session-end-result.schema.json"),
        document::<SessionExportParams>("SessionExportParams", "session-export-params.schema.json"),
        document::<SessionExportRequestSchema>(
            "SessionExportRequest",
            "session-export-request.schema.json",
        ),
        document::<SessionExportResponseSchema>(
            "SessionExportResponse",
            "session-export-response.schema.json",
        ),
        document::<SessionExportResult>("SessionExportResult", "session-export-result.schema.json"),
        document::<SessionExport>("SessionExport", "session-export.schema.json"),
        document::<SessionId>("SessionId", "session-id.schema.json"),
        document::<SessionInfo>("SessionInfo", "session-info.schema.json"),
        document::<SessionOutcome>("SessionOutcome", "session-outcome.schema.json"),
        document::<SessionStartRequestSchema>(
            "SessionStartRequest",
            "session-start-request.schema.json",
        ),
        document::<SessionStartResponseSchema>(
            "SessionStartResponse",
            "session-start-response.schema.json",
        ),
        document::<SessionStartResult>("SessionStartResult", "session-start-result.schema.json"),
        document::<SessionState>("SessionState", "session-state.schema.json"),
        document::<SessionTargetParams>("SessionTargetParams", "session-target-params.schema.json"),
        document::<SessionsListRequestSchema>(
            "SessionsListRequest",
            "sessions-list-request.schema.json",
        ),
        document::<SessionsListResponseSchema>(
            "SessionsListResponse",
            "sessions-list-response.schema.json",
        ),
        document::<SessionsListResult>("SessionsListResult", "sessions-list-result.schema.json"),
        document::<SetElementValueArguments>(
            "SetElementValueArguments",
            "set-element-value-arguments.schema.json",
        ),
        document::<SetElementValueResult>(
            "SetElementValueResult",
            "set-element-value-result.schema.json",
        ),
        document::<SystemDescribeRequestSchema>(
            "SystemDescribeRequest",
            "system-describe-request.schema.json",
        ),
        document::<SystemDescribeResponseSchema>(
            "SystemDescribeResponse",
            "system-describe-response.schema.json",
        ),
        document::<SystemDescribeResult>(
            "SystemDescribeResult",
            "system-describe-result.schema.json",
        ),
        document::<SystemHelloRequestSchema>(
            "SystemHelloRequest",
            "system-hello-request.schema.json",
        ),
        document::<SystemHelloResponseSchema>(
            "SystemHelloResponse",
            "system-hello-response.schema.json",
        ),
        document::<TapElementArguments>("TapElementArguments", "tap-element-arguments.schema.json"),
        document::<TapElementResult>("TapElementResult", "tap-element-result.schema.json"),
        document::<TestEventPayload>("TestEventPayload", "test-event-payload.schema.json"),
        document::<TestEvent>("TestEvent", "test-event.schema.json"),
        document::<TextMatchMode>("TextMatchMode", "text-match-mode.schema.json"),
        document::<TextMatch>("TextMatch", "text-match.schema.json"),
        document::<TransportInfo>("TransportInfo", "transport-info.schema.json"),
        document::<UiContextKind>("UiContextKind", "ui-context-kind.schema.json"),
        document::<UiContextRef>("UiContextRef", "ui-context-ref.schema.json"),
        document::<UiContextSelector>("UiContextSelector", "ui-context-selector.schema.json"),
        document::<UiNodeRef>("UiNodeRef", "ui-node-ref.schema.json"),
        document::<UiNode>("UiNode", "ui-node.schema.json"),
        document::<UiRect>("UiRect", "ui-rect.schema.json"),
        document::<UiSnapshotGetParams>(
            "UiSnapshotGetParams",
            "ui-snapshot-get-params.schema.json",
        ),
        document::<UiSnapshotGetRequestSchema>(
            "UiSnapshotGetRequest",
            "ui-snapshot-get-request.schema.json",
        ),
        document::<UiSnapshotGetResponseSchema>(
            "UiSnapshotGetResponse",
            "ui-snapshot-get-response.schema.json",
        ),
        document::<UiSnapshotGetResult>(
            "UiSnapshotGetResult",
            "ui-snapshot-get-result.schema.json",
        ),
        document::<UiSnapshotOmissionReason>(
            "UiSnapshotOmissionReason",
            "ui-snapshot-omission-reason.schema.json",
        ),
        document::<UiSnapshotRef>("UiSnapshotRef", "ui-snapshot-ref.schema.json"),
        document::<UiSnapshot>("UiSnapshot", "ui-snapshot.schema.json"),
        document::<VerdictRecordParams>("VerdictRecordParams", "verdict-record-params.schema.json"),
        document::<VerdictRecordRequestSchema>(
            "VerdictRecordRequest",
            "verdict-record-request.schema.json",
        ),
        document::<VerdictRecordResponseSchema>(
            "VerdictRecordResponse",
            "verdict-record-response.schema.json",
        ),
        document::<VerdictRecordResult>("VerdictRecordResult", "verdict-record-result.schema.json"),
        document::<VerdictStatus>("VerdictStatus", "verdict-status.schema.json"),
        document::<Verdict>("Verdict", "verdict.schema.json"),
        document::<Viewport>("Viewport", "viewport.schema.json"),
        document::<WaitForElementArguments>(
            "WaitForElementArguments",
            "wait-for-element-arguments.schema.json",
        ),
        document::<WaitForElementCondition>(
            "WaitForElementCondition",
            "wait-for-element-condition.schema.json",
        ),
        document::<WaitForElementResult>(
            "WaitForElementResult",
            "wait-for-element-result.schema.json",
        ),
    ]
}

fn document<T: JsonSchema>(name: &'static str, file_name: &'static str) -> SchemaDocument {
    let schema_id = format!(
        "urn:devicerail:schema:protocol:v{SCHEMA_SET_VERSION}:{}",
        file_name.trim_end_matches(".schema.json")
    );
    let root = SchemaSettings::draft2020_12()
        .for_deserialize()
        .into_generator()
        .into_root_schema_for::<T>();
    let mut schema = serde_json::to_value(root).expect("schemars schema serializes to JSON");
    let root = schema
        .as_object_mut()
        .expect("schemars root schema is an object");
    root.insert("$id".to_owned(), Value::String(schema_id.clone()));
    root.insert("title".to_owned(), Value::String(name.to_owned()));

    SchemaDocument {
        name,
        file_name,
        schema_id,
        schema,
    }
}

#[allow(dead_code)]
#[derive(JsonSchema)]
#[schemars(rename = "SystemHelloRequest")]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct SystemHelloRequestSchema {
    jsonrpc: JsonRpcVersion,
    id: RpcId,
    method: SystemHelloMethodSchema,
    params: HelloParams,
}

#[allow(dead_code)]
#[derive(JsonSchema)]
enum SystemHelloMethodSchema {
    #[serde(rename = "system.hello")]
    SystemHello,
}

macro_rules! no_params_request_schema {
    ($request:ident, $method_type:ident, $method_variant:ident, $title:literal, $method:literal) => {
        #[allow(dead_code)]
        #[derive(JsonSchema)]
        #[schemars(rename = $title)]
        #[serde(rename_all = "camelCase", deny_unknown_fields)]
        struct $request {
            jsonrpc: JsonRpcVersion,
            id: RpcId,
            method: $method_type,
            #[serde(default, skip_serializing_if = "Option::is_none")]
            #[schemars(with = "NoParamsSchema")]
            params: Option<NoParamsSchema>,
        }

        #[allow(dead_code)]
        #[derive(JsonSchema)]
        enum $method_type {
            #[serde(rename = $method)]
            $method_variant,
        }
    };
}

macro_rules! timeout_no_params_request_schema {
    ($request:ident, $method_type:ident, $method_variant:ident, $title:literal, $method:literal) => {
        #[allow(dead_code)]
        #[derive(JsonSchema)]
        #[schemars(rename = $title)]
        #[serde(rename_all = "camelCase", deny_unknown_fields)]
        struct $request {
            jsonrpc: JsonRpcVersion,
            id: RpcId,
            method: $method_type,
            #[serde(default, skip_serializing_if = "Option::is_none")]
            #[schemars(with = "RequestTimeoutMs")]
            timeout_ms: Option<RequestTimeoutMs>,
            #[serde(default, skip_serializing_if = "Option::is_none")]
            #[schemars(with = "NoParamsSchema")]
            params: Option<NoParamsSchema>,
        }

        #[allow(dead_code)]
        #[derive(JsonSchema)]
        enum $method_type {
            #[serde(rename = $method)]
            $method_variant,
        }
    };
}

macro_rules! response_schema {
    ($response:ident, $success:ident, $title:literal, $result:ty) => {
        #[allow(dead_code)]
        #[derive(JsonSchema)]
        #[schemars(rename = $title)]
        #[serde(untagged)]
        enum $response {
            Success(Box<$success>),
            Failure(SystemHelloFailureSchema),
        }

        #[allow(dead_code)]
        #[derive(JsonSchema)]
        #[serde(rename_all = "camelCase", deny_unknown_fields)]
        struct $success {
            jsonrpc: JsonRpcVersion,
            id: RpcId,
            result: $result,
        }
    };
}

no_params_request_schema!(
    SystemDescribeRequestSchema,
    SystemDescribeMethodSchema,
    SystemDescribe,
    "SystemDescribeRequest",
    "system.describe"
);
timeout_no_params_request_schema!(
    DeviceConnectRequestSchema,
    DeviceConnectMethodSchema,
    DeviceConnect,
    "DeviceConnectRequest",
    "device.connect"
);
timeout_no_params_request_schema!(
    DeviceDisconnectRequestSchema,
    DeviceDisconnectMethodSchema,
    DeviceDisconnect,
    "DeviceDisconnectRequest",
    "device.disconnect"
);
timeout_no_params_request_schema!(
    DeviceCapabilitiesRequestSchema,
    DeviceCapabilitiesMethodSchema,
    DeviceCapabilities,
    "DeviceCapabilitiesRequest",
    "device.capabilities"
);
timeout_no_params_request_schema!(
    DeviceObserveRequestSchema,
    DeviceObserveMethodSchema,
    DeviceObserve,
    "DeviceObserveRequest",
    "device.observe"
);
no_params_request_schema!(
    SessionStartRequestSchema,
    SessionStartMethodSchema,
    SessionStart,
    "SessionStartRequest",
    "session.start"
);
no_params_request_schema!(
    SessionCurrentRequestSchema,
    SessionCurrentMethodSchema,
    SessionCurrent,
    "SessionCurrentRequest",
    "session.current"
);
no_params_request_schema!(
    SessionsListRequestSchema,
    SessionsListMethodSchema,
    SessionsList,
    "SessionsListRequest",
    "sessions.list"
);

response_schema!(
    SystemDescribeResponseSchema,
    SystemDescribeSuccessSchema,
    "SystemDescribeResponse",
    SystemDescribeResult
);
response_schema!(
    DeviceConnectResponseSchema,
    DeviceConnectSuccessSchema,
    "DeviceConnectResponse",
    DeviceConnectResult
);
response_schema!(
    DeviceDisconnectResponseSchema,
    DeviceDisconnectSuccessSchema,
    "DeviceDisconnectResponse",
    DeviceDisconnectResult
);
response_schema!(
    DeviceCapabilitiesResponseSchema,
    DeviceCapabilitiesSuccessSchema,
    "DeviceCapabilitiesResponse",
    DeviceCapabilitiesResult
);
response_schema!(
    DeviceObserveResponseSchema,
    DeviceObserveSuccessSchema,
    "DeviceObserveResponse",
    DeviceObserveResult
);
response_schema!(
    DeviceExecuteResponseSchema,
    DeviceExecuteSuccessSchema,
    "DeviceExecuteResponse",
    DeviceExecuteResult
);
response_schema!(
    UiSnapshotGetResponseSchema,
    UiSnapshotGetSuccessSchema,
    "UiSnapshotGetResponse",
    UiSnapshotGetResult
);
response_schema!(
    VerdictRecordResponseSchema,
    VerdictRecordSuccessSchema,
    "VerdictRecordResponse",
    VerdictRecordResult
);
response_schema!(
    SessionStartResponseSchema,
    SessionStartSuccessSchema,
    "SessionStartResponse",
    SessionStartResult
);
response_schema!(
    SessionCurrentResponseSchema,
    SessionCurrentSuccessSchema,
    "SessionCurrentResponse",
    SessionCurrentResult
);
response_schema!(
    SessionEndResponseSchema,
    SessionEndSuccessSchema,
    "SessionEndResponse",
    SessionEndResult
);
response_schema!(
    SessionsListResponseSchema,
    SessionsListSuccessSchema,
    "SessionsListResponse",
    SessionsListResult
);
response_schema!(
    SessionExportResponseSchema,
    SessionExportSuccessSchema,
    "SessionExportResponse",
    SessionExportResult
);
response_schema!(
    EventsListResponseSchema,
    EventsListSuccessSchema,
    "EventsListResponse",
    EventsListResult
);
response_schema!(
    EventsStreamOpenResponseSchema,
    EventsStreamOpenSuccessSchema,
    "EventsStreamOpenResponse",
    EventsStreamOpenResult
);
response_schema!(
    EventsSubscribeResponseSchema,
    EventsSubscribeSuccessSchema,
    "EventsSubscribeResponse",
    EventsSubscribeResult
);
response_schema!(
    EventsClearResponseSchema,
    EventsClearSuccessSchema,
    "EventsClearResponse",
    EventsClearResult
);
response_schema!(
    MediaStreamCaptureResponseSchema,
    MediaStreamCaptureSuccessSchema,
    "MediaStreamCaptureResponse",
    MediaStreamCaptureResult
);
response_schema!(
    MediaStreamEndResponseSchema,
    MediaStreamEndSuccessSchema,
    "MediaStreamEndResponse",
    MediaStreamEndResult
);
response_schema!(
    MediaStreamStartResponseSchema,
    MediaStreamStartSuccessSchema,
    "MediaStreamStartResponse",
    MediaStreamStartResult
);

#[allow(dead_code)]
#[derive(JsonSchema)]
#[schemars(rename = "MediaStreamCaptureRequest")]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct MediaStreamCaptureRequestSchema {
    jsonrpc: JsonRpcVersion,
    id: RpcId,
    method: MediaStreamCaptureMethodSchema,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(with = "RequestTimeoutMs")]
    timeout_ms: Option<RequestTimeoutMs>,
    params: MediaStreamCaptureParams,
}

#[allow(dead_code)]
#[derive(JsonSchema)]
enum MediaStreamCaptureMethodSchema {
    #[serde(rename = "media.stream.capture")]
    MediaStreamCapture,
}

#[allow(dead_code)]
#[derive(JsonSchema)]
#[schemars(rename = "MediaStreamEndRequest")]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct MediaStreamEndRequestSchema {
    jsonrpc: JsonRpcVersion,
    id: RpcId,
    method: MediaStreamEndMethodSchema,
    params: MediaStreamEndParams,
}

#[allow(dead_code)]
#[derive(JsonSchema)]
enum MediaStreamEndMethodSchema {
    #[serde(rename = "media.stream.end")]
    MediaStreamEnd,
}

#[allow(dead_code)]
#[derive(JsonSchema)]
#[schemars(rename = "MediaStreamStartRequest")]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct MediaStreamStartRequestSchema {
    jsonrpc: JsonRpcVersion,
    id: RpcId,
    method: MediaStreamStartMethodSchema,
    params: MediaStreamStartParams,
}

#[allow(dead_code)]
#[derive(JsonSchema)]
enum MediaStreamStartMethodSchema {
    #[serde(rename = "media.stream.start")]
    MediaStreamStart,
}

#[allow(dead_code)]
#[derive(JsonSchema)]
#[schemars(rename = "EventsStreamOpenRequest")]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct EventsStreamOpenRequestSchema {
    jsonrpc: JsonRpcVersion,
    id: RpcId,
    method: EventsStreamOpenMethodSchema,
    params: EventsStreamOpenParams,
}

#[allow(dead_code)]
#[derive(JsonSchema)]
enum EventsStreamOpenMethodSchema {
    #[serde(rename = "events.stream.open")]
    EventsStreamOpen,
}

#[allow(dead_code)]
#[derive(JsonSchema)]
#[schemars(rename = "EventsSubscribeRequest")]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct EventsSubscribeRequestSchema {
    jsonrpc: JsonRpcVersion,
    id: RpcId,
    method: EventsSubscribeMethodSchema,
    params: EventsSubscribeParams,
}

#[allow(dead_code)]
#[derive(JsonSchema)]
enum EventsSubscribeMethodSchema {
    #[serde(rename = "events.subscribe")]
    EventsSubscribe,
}

#[allow(dead_code)]
#[derive(JsonSchema)]
#[schemars(rename = "DeviceExecuteRequest")]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct DeviceExecuteRequestSchema {
    jsonrpc: JsonRpcVersion,
    id: RpcId,
    method: DeviceExecuteMethodSchema,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(with = "RequestTimeoutMs")]
    timeout_ms: Option<RequestTimeoutMs>,
    params: DeviceExecuteParams,
}

#[allow(dead_code)]
#[derive(JsonSchema)]
enum DeviceExecuteMethodSchema {
    #[serde(rename = "device.execute")]
    DeviceExecute,
}

#[allow(dead_code)]
#[derive(JsonSchema)]
#[schemars(rename = "UiSnapshotGetRequest")]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct UiSnapshotGetRequestSchema {
    jsonrpc: JsonRpcVersion,
    id: RpcId,
    method: UiSnapshotGetMethodSchema,
    params: UiSnapshotGetParams,
}

#[allow(dead_code)]
#[derive(JsonSchema)]
enum UiSnapshotGetMethodSchema {
    #[serde(rename = "ui.snapshot.get")]
    UiSnapshotGet,
}

#[allow(dead_code)]
#[derive(JsonSchema)]
#[schemars(rename = "VerdictRecordRequest")]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct VerdictRecordRequestSchema {
    jsonrpc: JsonRpcVersion,
    id: RpcId,
    method: VerdictRecordMethodSchema,
    params: VerdictRecordParams,
}

#[allow(dead_code)]
#[derive(JsonSchema)]
enum VerdictRecordMethodSchema {
    #[serde(rename = "verdict.record")]
    VerdictRecord,
}

#[allow(dead_code)]
#[derive(JsonSchema)]
#[schemars(rename = "SessionEndRequest")]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct SessionEndRequestSchema {
    jsonrpc: JsonRpcVersion,
    id: RpcId,
    method: SessionEndMethodSchema,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(with = "SessionEndParams")]
    params: Option<SessionEndParams>,
}

#[allow(dead_code)]
#[derive(JsonSchema)]
enum SessionEndMethodSchema {
    #[serde(rename = "session.end")]
    SessionEnd,
}

#[allow(dead_code)]
#[derive(JsonSchema)]
#[schemars(rename = "SessionExportRequest")]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct SessionExportRequestSchema {
    jsonrpc: JsonRpcVersion,
    id: RpcId,
    method: SessionExportMethodSchema,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(with = "SessionExportParams")]
    params: Option<SessionExportParams>,
}

#[allow(dead_code)]
#[derive(JsonSchema)]
enum SessionExportMethodSchema {
    #[serde(rename = "session.export")]
    SessionExport,
}

#[allow(dead_code)]
#[derive(JsonSchema)]
#[schemars(rename = "EventsListRequest")]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct EventsListRequestSchema {
    jsonrpc: JsonRpcVersion,
    id: RpcId,
    method: EventsListMethodSchema,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(with = "EventsListParams")]
    params: Option<EventsListParams>,
}

#[allow(dead_code)]
#[derive(JsonSchema)]
enum EventsListMethodSchema {
    #[serde(rename = "events.list")]
    EventsList,
}

#[allow(dead_code)]
#[derive(JsonSchema)]
#[schemars(rename = "EventsClearRequest")]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct EventsClearRequestSchema {
    jsonrpc: JsonRpcVersion,
    id: RpcId,
    method: EventsClearMethodSchema,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(with = "SessionTargetParams")]
    params: Option<SessionTargetParams>,
}

#[allow(dead_code)]
#[derive(JsonSchema)]
enum EventsClearMethodSchema {
    #[serde(rename = "events.clear")]
    EventsClear,
}

#[allow(dead_code)]
#[derive(JsonSchema)]
#[schemars(rename = "DeviceSelectRequest")]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct DeviceSelectRequestSchema {
    jsonrpc: JsonRpcVersion,
    id: RpcId,
    method: DeviceSelectMethodSchema,
    params: DeviceSelectParams,
}

#[allow(dead_code)]
#[derive(JsonSchema)]
enum DeviceSelectMethodSchema {
    #[serde(rename = "device.select")]
    DeviceSelect,
}

#[allow(dead_code)]
#[derive(JsonSchema)]
#[schemars(rename = "DeviceSelectResponse")]
#[serde(untagged)]
enum DeviceSelectResponseSchema {
    Success(DeviceSelectSuccessSchema),
    Failure(SystemHelloFailureSchema),
}

#[allow(dead_code)]
#[derive(JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct DeviceSelectSuccessSchema {
    jsonrpc: JsonRpcVersion,
    id: RpcId,
    result: DeviceSelectResult,
}

#[allow(dead_code)]
#[derive(JsonSchema)]
#[schemars(rename = "DevicesListRequest")]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct DevicesListRequestSchema {
    jsonrpc: JsonRpcVersion,
    id: RpcId,
    method: DevicesListMethodSchema,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(with = "NoParamsSchema")]
    params: Option<NoParamsSchema>,
}

#[allow(dead_code)]
#[derive(JsonSchema)]
enum DevicesListMethodSchema {
    #[serde(rename = "devices.list")]
    DevicesList,
}

#[allow(dead_code)]
#[derive(JsonSchema)]
#[serde(untagged)]
enum NoParamsSchema {
    Object(EmptyParamsObjectSchema),
    Array([Value; 0]),
}

#[allow(dead_code)]
#[derive(JsonSchema)]
#[serde(deny_unknown_fields)]
struct EmptyParamsObjectSchema {}

#[allow(dead_code)]
#[derive(JsonSchema)]
#[schemars(rename = "DevicesListResponse")]
#[serde(untagged)]
enum DevicesListResponseSchema {
    Success(DevicesListSuccessSchema),
    Failure(SystemHelloFailureSchema),
}

#[allow(dead_code)]
#[derive(JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct DevicesListSuccessSchema {
    jsonrpc: JsonRpcVersion,
    id: RpcId,
    result: DevicesListResult,
}

#[allow(dead_code)]
#[derive(JsonSchema)]
#[schemars(rename = "RequestCancelRequest")]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RequestCancelRequestSchema {
    jsonrpc: JsonRpcVersion,
    id: RpcId,
    method: RequestCancelMethodSchema,
    params: RequestCancelParams,
}

#[allow(dead_code)]
#[derive(JsonSchema)]
enum RequestCancelMethodSchema {
    #[serde(rename = "request.cancel")]
    RequestCancel,
}

#[allow(dead_code)]
#[derive(JsonSchema)]
#[schemars(rename = "RequestCancelResponse")]
#[serde(untagged)]
enum RequestCancelResponseSchema {
    Success(RequestCancelSuccessSchema),
    Failure(SystemHelloFailureSchema),
}

#[allow(dead_code)]
#[derive(JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RequestCancelSuccessSchema {
    jsonrpc: JsonRpcVersion,
    id: RpcId,
    result: RequestCancelResult,
}

#[allow(dead_code)]
#[derive(JsonSchema)]
#[schemars(rename = "SystemHelloResponse")]
#[serde(untagged)]
enum SystemHelloResponseSchema {
    Success(SystemHelloSuccessSchema),
    Failure(SystemHelloFailureSchema),
}

#[allow(dead_code)]
#[derive(JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct SystemHelloSuccessSchema {
    jsonrpc: JsonRpcVersion,
    id: RpcId,
    result: HelloResult,
}

#[allow(dead_code)]
#[derive(JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct SystemHelloFailureSchema {
    jsonrpc: JsonRpcVersion,
    id: NullableRpcIdSchema,
    error: RpcError,
}

#[allow(dead_code)]
#[derive(JsonSchema)]
#[serde(untagged)]
enum NullableRpcIdSchema {
    Id(RpcId),
    Null(()),
}

#[cfg(test)]
mod tests {
    use super::{JSON_SCHEMA_DRAFT, documents};

    const PUBLIC_SCHEMA_DOCUMENT_COUNT: usize = 174;

    const PUBLIC_METHODS: [&str; 24] = [
        "device.capabilities",
        "device.connect",
        "device.disconnect",
        "device.execute",
        "device.observe",
        "device.select",
        "devices.list",
        "events.clear",
        "events.list",
        "events.stream.open",
        "events.subscribe",
        "media.stream.capture",
        "media.stream.end",
        "media.stream.start",
        "request.cancel",
        "session.current",
        "session.end",
        "session.export",
        "session.start",
        "sessions.list",
        "system.describe",
        "system.hello",
        "ui.snapshot.get",
        "verdict.record",
    ];

    #[test]
    fn documents_have_unique_stable_names_and_ids() {
        let documents = documents();
        assert_eq!(documents.len(), PUBLIC_SCHEMA_DOCUMENT_COUNT);
        let mut names = documents
            .iter()
            .map(|document| document.file_name)
            .collect::<Vec<_>>();
        let original = names.clone();
        names.sort_unstable();
        names.dedup();

        assert_eq!(names.len(), documents.len());
        assert_eq!(original, names, "documents must stay sorted by file name");
        for document in documents {
            assert_eq!(document.schema["$schema"], JSON_SCHEMA_DRAFT);
            assert_eq!(document.schema["$id"], document.schema_id);
            assert_eq!(document.schema["title"], document.name);
        }
    }

    #[test]
    fn protected_action_types_have_independent_root_documents() {
        let documents = documents();
        for name in [
            "ActionProtection",
            "RecordedActionCall",
            "ScreenshotOmissionReason",
        ] {
            assert!(
                documents.iter().any(|document| document.name == name),
                "missing root schema for {name}"
            );
        }
    }

    #[test]
    fn action_input_schema_remains_an_unconstrained_json_value() {
        let document = documents()
            .into_iter()
            .find(|document| document.name == "ActionDefinition")
            .expect("action definition schema exists");

        assert_eq!(document.schema["properties"]["inputSchema"], true);
    }

    #[test]
    fn every_public_method_has_request_and_response_schemas() {
        let documents = documents();
        for method in PUBLIC_METHODS {
            let stem = method.replace('.', "-");
            let request_file = format!("{stem}-request.schema.json");
            let response_file = format!("{stem}-response.schema.json");

            let request = documents
                .iter()
                .find(|document| document.file_name == request_file)
                .unwrap_or_else(|| panic!("missing request schema for {method}"));
            assert!(
                request.schema.to_string().contains(method),
                "request schema for {method} must lock its method literal"
            );
            assert!(
                documents
                    .iter()
                    .any(|document| document.file_name == response_file),
                "missing response schema for {method}"
            );
        }
    }

    #[test]
    fn every_method_response_reuses_one_failure_envelope() {
        let documents = documents();
        let mut expected_failure = None;

        for method in PUBLIC_METHODS {
            let file_name = format!("{}-response.schema.json", method.replace('.', "-"));
            let response = documents
                .iter()
                .find(|document| document.file_name == file_name)
                .unwrap_or_else(|| panic!("missing response schema for {method}"));
            let failure = response.schema["$defs"]["SystemHelloFailureSchema"].clone();
            assert!(
                failure.is_object(),
                "{method} must define the failure branch"
            );
            if let Some(expected) = &expected_failure {
                assert_eq!(
                    &failure, expected,
                    "{method} must reuse the common failure envelope"
                );
            } else {
                expected_failure = Some(failure);
            }
            assert!(
                response.schema["anyOf"]
                    .as_array()
                    .expect("method response is a success/failure union")
                    .iter()
                    .any(|branch| { branch["$ref"] == "#/$defs/SystemHelloFailureSchema" }),
                "{method} response must expose the failure branch"
            );
        }
    }
}
