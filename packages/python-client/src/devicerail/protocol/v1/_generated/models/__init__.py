# Generated from protocol/schema/v1. DO NOT EDIT.
# Run `python scripts/generate.py` from packages/python-client.

from .action_call import ActionCall as ActionCall
from .action_definition import ActionDefinition as ActionDefinition
from .action_execution import ActionExecution as ActionExecution
from .action_outcome import ActionOutcome as ActionOutcome
from .action_protection import ActionProtection as ActionProtection
from .action_result import ActionResult as ActionResult
from .asset_ref import AssetRef as AssetRef
from .clear_element_arguments import ClearElementArguments as ClearElementArguments
from .clear_element_result import ClearElementResult as ClearElementResult
from .coordinate_fallback_reason import CoordinateFallbackReason as CoordinateFallbackReason
from .device_capabilities_request import DeviceCapabilitiesRequest as DeviceCapabilitiesRequest
from .device_capabilities_response import DeviceCapabilitiesResponse as DeviceCapabilitiesResponse
from .device_capabilities_result import DeviceCapabilitiesResult as DeviceCapabilitiesResult
from .device_connect_request import DeviceConnectRequest as DeviceConnectRequest
from .device_connect_response import DeviceConnectResponse as DeviceConnectResponse
from .device_connect_result import DeviceConnectResult as DeviceConnectResult
from .device_disconnect_request import DeviceDisconnectRequest as DeviceDisconnectRequest
from .device_disconnect_response import DeviceDisconnectResponse as DeviceDisconnectResponse
from .device_disconnect_result import DeviceDisconnectResult as DeviceDisconnectResult
from .device_execute_params import DeviceExecuteParams as DeviceExecuteParams
from .device_execute_request import DeviceExecuteRequest as DeviceExecuteRequest
from .device_execute_response import DeviceExecuteResponse as DeviceExecuteResponse
from .device_execute_result import DeviceExecuteResult as DeviceExecuteResult
from .device_id import DeviceId as DeviceId
from .device_info import DeviceInfo as DeviceInfo
from .device_observe_request import DeviceObserveRequest as DeviceObserveRequest
from .device_observe_response import DeviceObserveResponse as DeviceObserveResponse
from .device_observe_result import DeviceObserveResult as DeviceObserveResult
from .device_select_params import DeviceSelectParams as DeviceSelectParams
from .device_select_request import DeviceSelectRequest as DeviceSelectRequest
from .device_select_response import DeviceSelectResponse as DeviceSelectResponse
from .device_select_result import DeviceSelectResult as DeviceSelectResult
from .devices_list_request import DevicesListRequest as DevicesListRequest
from .devices_list_response import DevicesListResponse as DevicesListResponse
from .devices_list_result import DevicesListResult as DevicesListResult
from .element_selector import ElementSelector as ElementSelector
from .element_target import ElementTarget as ElementTarget
from .error_info import ErrorInfo as ErrorInfo
from .event_id import EventId as EventId
from .event_sequence import EventSequence as EventSequence
from .event_stream_cursor import EventStreamCursor as EventStreamCursor
from .event_stream_endpoint import EventStreamEndpoint as EventStreamEndpoint
from .event_stream_epoch import EventStreamEpoch as EventStreamEpoch
from .event_stream_origin_policy import EventStreamOriginPolicy as EventStreamOriginPolicy
from .event_subscription_id import EventSubscriptionId as EventSubscriptionId
from .events_clear_request import EventsClearRequest as EventsClearRequest
from .events_clear_response import EventsClearResponse as EventsClearResponse
from .events_clear_result import EventsClearResult as EventsClearResult
from .events_list_params import EventsListParams as EventsListParams
from .events_list_request import EventsListRequest as EventsListRequest
from .events_list_response import EventsListResponse as EventsListResponse
from .events_list_result import EventsListResult as EventsListResult
from .events_stream_event_notification import EventsStreamEventNotification as EventsStreamEventNotification
from .events_stream_event_params import EventsStreamEventParams as EventsStreamEventParams
from .events_stream_open_params import EventsStreamOpenParams as EventsStreamOpenParams
from .events_stream_open_request import EventsStreamOpenRequest as EventsStreamOpenRequest
from .events_stream_open_response import EventsStreamOpenResponse as EventsStreamOpenResponse
from .events_stream_open_result import EventsStreamOpenResult as EventsStreamOpenResult
from .events_stream_terminal_notification import EventsStreamTerminalNotification as EventsStreamTerminalNotification
from .events_stream_terminal_params import EventsStreamTerminalParams as EventsStreamTerminalParams
from .events_stream_termination import EventsStreamTermination as EventsStreamTermination
from .events_subscribe_params import EventsSubscribeParams as EventsSubscribeParams
from .events_subscribe_request import EventsSubscribeRequest as EventsSubscribeRequest
from .events_subscribe_response import EventsSubscribeResponse as EventsSubscribeResponse
from .events_subscribe_result import EventsSubscribeResult as EventsSubscribeResult
from .feature_offer import FeatureOffer as FeatureOffer
from .feature_selection import FeatureSelection as FeatureSelection
from .find_element_arguments import FindElementArguments as FindElementArguments
from .find_element_result import FindElementResult as FindElementResult
from .hello_params import HelloParams as HelloParams
from .hello_result import HelloResult as HelloResult
from .json_rpc_version import JsonRpcVersion as JsonRpcVersion
from .manual_action_arguments import ManualActionArguments as ManualActionArguments
from .manual_action_step import ManualActionStep as ManualActionStep
from .manual_recording import ManualRecording as ManualRecording
from .media_frame import MediaFrame as MediaFrame
from .media_stream_capture_params import MediaStreamCaptureParams as MediaStreamCaptureParams
from .media_stream_capture_request import MediaStreamCaptureRequest as MediaStreamCaptureRequest
from .media_stream_capture_response import MediaStreamCaptureResponse as MediaStreamCaptureResponse
from .media_stream_capture_result import MediaStreamCaptureResult as MediaStreamCaptureResult
from .media_stream_end_params import MediaStreamEndParams as MediaStreamEndParams
from .media_stream_end_request import MediaStreamEndRequest as MediaStreamEndRequest
from .media_stream_end_response import MediaStreamEndResponse as MediaStreamEndResponse
from .media_stream_end_result import MediaStreamEndResult as MediaStreamEndResult
from .media_stream_id import MediaStreamId as MediaStreamId
from .media_stream_info import MediaStreamInfo as MediaStreamInfo
from .media_stream_kind import MediaStreamKind as MediaStreamKind
from .media_stream_start_params import MediaStreamStartParams as MediaStreamStartParams
from .media_stream_start_request import MediaStreamStartRequest as MediaStreamStartRequest
from .media_stream_start_response import MediaStreamStartResponse as MediaStreamStartResponse
from .media_stream_start_result import MediaStreamStartResult as MediaStreamStartResult
from .observation import Observation as Observation
from .peer_info import PeerInfo as PeerInfo
from .platform import Platform as Platform
from .protocol_incompatibility_reason import ProtocolIncompatibilityReason as ProtocolIncompatibilityReason
from .protocol_offer import ProtocolOffer as ProtocolOffer
from .protocol_range import ProtocolRange as ProtocolRange
from .protocol_selection import ProtocolSelection as ProtocolSelection
from .protocol_version import ProtocolVersion as ProtocolVersion
from .recorded_action_call import RecordedActionCall as RecordedActionCall
from .request_cancel_params import RequestCancelParams as RequestCancelParams
from .request_cancel_request import RequestCancelRequest as RequestCancelRequest
from .request_cancel_response import RequestCancelResponse as RequestCancelResponse
from .request_cancel_result import RequestCancelResult as RequestCancelResult
from .request_cancel_status import RequestCancelStatus as RequestCancelStatus
from .request_timeout_ms import RequestTimeoutMs as RequestTimeoutMs
from .rpc_error import RpcError as RpcError
from .rpc_id import RpcId as RpcId
from .rpc_params import RpcParams as RpcParams
from .rpc_request import RpcRequest as RpcRequest
from .rpc_response import RpcResponse as RpcResponse
from .rpc_server_message import RpcServerMessage as RpcServerMessage
from .rpc_server_notification import RpcServerNotification as RpcServerNotification
from .screenshot_omission_reason import ScreenshotOmissionReason as ScreenshotOmissionReason
from .session_current_request import SessionCurrentRequest as SessionCurrentRequest
from .session_current_response import SessionCurrentResponse as SessionCurrentResponse
from .session_current_result import SessionCurrentResult as SessionCurrentResult
from .session_end_params import SessionEndParams as SessionEndParams
from .session_end_request import SessionEndRequest as SessionEndRequest
from .session_end_response import SessionEndResponse as SessionEndResponse
from .session_end_result import SessionEndResult as SessionEndResult
from .session_export import SessionExport as SessionExport
from .session_export_params import SessionExportParams as SessionExportParams
from .session_export_request import SessionExportRequest as SessionExportRequest
from .session_export_response import SessionExportResponse as SessionExportResponse
from .session_export_result import SessionExportResult as SessionExportResult
from .session_id import SessionId as SessionId
from .session_info import SessionInfo as SessionInfo
from .session_outcome import SessionOutcome as SessionOutcome
from .session_start_request import SessionStartRequest as SessionStartRequest
from .session_start_response import SessionStartResponse as SessionStartResponse
from .session_start_result import SessionStartResult as SessionStartResult
from .session_state import SessionState as SessionState
from .session_target_params import SessionTargetParams as SessionTargetParams
from .sessions_list_request import SessionsListRequest as SessionsListRequest
from .sessions_list_response import SessionsListResponse as SessionsListResponse
from .sessions_list_result import SessionsListResult as SessionsListResult
from .set_element_value_arguments import SetElementValueArguments as SetElementValueArguments
from .set_element_value_result import SetElementValueResult as SetElementValueResult
from .system_describe_request import SystemDescribeRequest as SystemDescribeRequest
from .system_describe_response import SystemDescribeResponse as SystemDescribeResponse
from .system_describe_result import SystemDescribeResult as SystemDescribeResult
from .system_hello_request import SystemHelloRequest as SystemHelloRequest
from .system_hello_response import SystemHelloResponse as SystemHelloResponse
from .tap_element_arguments import TapElementArguments as TapElementArguments
from .tap_element_result import TapElementResult as TapElementResult
from .test_event import TestEvent as TestEvent
from .test_event_payload import TestEventPayload as TestEventPayload
from .text_match import TextMatch as TextMatch
from .text_match_mode import TextMatchMode as TextMatchMode
from .transport_info import TransportInfo as TransportInfo
from .ui_context_kind import UiContextKind as UiContextKind
from .ui_context_ref import UiContextRef as UiContextRef
from .ui_context_selector import UiContextSelector as UiContextSelector
from .ui_node import UiNode as UiNode
from .ui_node_ref import UiNodeRef as UiNodeRef
from .ui_rect import UiRect as UiRect
from .ui_snapshot import UiSnapshot as UiSnapshot
from .ui_snapshot_get_params import UiSnapshotGetParams as UiSnapshotGetParams
from .ui_snapshot_get_request import UiSnapshotGetRequest as UiSnapshotGetRequest
from .ui_snapshot_get_response import UiSnapshotGetResponse as UiSnapshotGetResponse
from .ui_snapshot_get_result import UiSnapshotGetResult as UiSnapshotGetResult
from .ui_snapshot_omission_reason import UiSnapshotOmissionReason as UiSnapshotOmissionReason
from .ui_snapshot_ref import UiSnapshotRef as UiSnapshotRef
from .verdict import Verdict as Verdict
from .verdict_record_params import VerdictRecordParams as VerdictRecordParams
from .verdict_record_request import VerdictRecordRequest as VerdictRecordRequest
from .verdict_record_response import VerdictRecordResponse as VerdictRecordResponse
from .verdict_record_result import VerdictRecordResult as VerdictRecordResult
from .verdict_status import VerdictStatus as VerdictStatus
from .viewport import Viewport as Viewport
from .wait_for_element_arguments import WaitForElementArguments as WaitForElementArguments
from .wait_for_element_condition import WaitForElementCondition as WaitForElementCondition
from .wait_for_element_result import WaitForElementResult as WaitForElementResult

__all__ = [
    'ActionCall',
    'ActionDefinition',
    'ActionExecution',
    'ActionOutcome',
    'ActionProtection',
    'ActionResult',
    'AssetRef',
    'ClearElementArguments',
    'ClearElementResult',
    'CoordinateFallbackReason',
    'DeviceCapabilitiesRequest',
    'DeviceCapabilitiesResponse',
    'DeviceCapabilitiesResult',
    'DeviceConnectRequest',
    'DeviceConnectResponse',
    'DeviceConnectResult',
    'DeviceDisconnectRequest',
    'DeviceDisconnectResponse',
    'DeviceDisconnectResult',
    'DeviceExecuteParams',
    'DeviceExecuteRequest',
    'DeviceExecuteResponse',
    'DeviceExecuteResult',
    'DeviceId',
    'DeviceInfo',
    'DeviceObserveRequest',
    'DeviceObserveResponse',
    'DeviceObserveResult',
    'DeviceSelectParams',
    'DeviceSelectRequest',
    'DeviceSelectResponse',
    'DeviceSelectResult',
    'DevicesListRequest',
    'DevicesListResponse',
    'DevicesListResult',
    'ElementSelector',
    'ElementTarget',
    'ErrorInfo',
    'EventId',
    'EventSequence',
    'EventStreamCursor',
    'EventStreamEndpoint',
    'EventStreamEpoch',
    'EventStreamOriginPolicy',
    'EventSubscriptionId',
    'EventsClearRequest',
    'EventsClearResponse',
    'EventsClearResult',
    'EventsListParams',
    'EventsListRequest',
    'EventsListResponse',
    'EventsListResult',
    'EventsStreamEventNotification',
    'EventsStreamEventParams',
    'EventsStreamOpenParams',
    'EventsStreamOpenRequest',
    'EventsStreamOpenResponse',
    'EventsStreamOpenResult',
    'EventsStreamTerminalNotification',
    'EventsStreamTerminalParams',
    'EventsStreamTermination',
    'EventsSubscribeParams',
    'EventsSubscribeRequest',
    'EventsSubscribeResponse',
    'EventsSubscribeResult',
    'FeatureOffer',
    'FeatureSelection',
    'FindElementArguments',
    'FindElementResult',
    'HelloParams',
    'HelloResult',
    'JsonRpcVersion',
    'ManualActionArguments',
    'ManualActionStep',
    'ManualRecording',
    'MediaFrame',
    'MediaStreamCaptureParams',
    'MediaStreamCaptureRequest',
    'MediaStreamCaptureResponse',
    'MediaStreamCaptureResult',
    'MediaStreamEndParams',
    'MediaStreamEndRequest',
    'MediaStreamEndResponse',
    'MediaStreamEndResult',
    'MediaStreamId',
    'MediaStreamInfo',
    'MediaStreamKind',
    'MediaStreamStartParams',
    'MediaStreamStartRequest',
    'MediaStreamStartResponse',
    'MediaStreamStartResult',
    'Observation',
    'PeerInfo',
    'Platform',
    'ProtocolIncompatibilityReason',
    'ProtocolOffer',
    'ProtocolRange',
    'ProtocolSelection',
    'ProtocolVersion',
    'RecordedActionCall',
    'RequestCancelParams',
    'RequestCancelRequest',
    'RequestCancelResponse',
    'RequestCancelResult',
    'RequestCancelStatus',
    'RequestTimeoutMs',
    'RpcError',
    'RpcId',
    'RpcParams',
    'RpcRequest',
    'RpcResponse',
    'RpcServerMessage',
    'RpcServerNotification',
    'ScreenshotOmissionReason',
    'SessionCurrentRequest',
    'SessionCurrentResponse',
    'SessionCurrentResult',
    'SessionEndParams',
    'SessionEndRequest',
    'SessionEndResponse',
    'SessionEndResult',
    'SessionExport',
    'SessionExportParams',
    'SessionExportRequest',
    'SessionExportResponse',
    'SessionExportResult',
    'SessionId',
    'SessionInfo',
    'SessionOutcome',
    'SessionStartRequest',
    'SessionStartResponse',
    'SessionStartResult',
    'SessionState',
    'SessionTargetParams',
    'SessionsListRequest',
    'SessionsListResponse',
    'SessionsListResult',
    'SetElementValueArguments',
    'SetElementValueResult',
    'SystemDescribeRequest',
    'SystemDescribeResponse',
    'SystemDescribeResult',
    'SystemHelloRequest',
    'SystemHelloResponse',
    'TapElementArguments',
    'TapElementResult',
    'TestEvent',
    'TestEventPayload',
    'TextMatch',
    'TextMatchMode',
    'TransportInfo',
    'UiContextKind',
    'UiContextRef',
    'UiContextSelector',
    'UiNode',
    'UiNodeRef',
    'UiRect',
    'UiSnapshot',
    'UiSnapshotGetParams',
    'UiSnapshotGetRequest',
    'UiSnapshotGetResponse',
    'UiSnapshotGetResult',
    'UiSnapshotOmissionReason',
    'UiSnapshotRef',
    'Verdict',
    'VerdictRecordParams',
    'VerdictRecordRequest',
    'VerdictRecordResponse',
    'VerdictRecordResult',
    'VerdictStatus',
    'Viewport',
    'WaitForElementArguments',
    'WaitForElementCondition',
    'WaitForElementResult',
]
