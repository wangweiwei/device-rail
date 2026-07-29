# Generated from protocol/schema/v1. DO NOT EDIT.
# Run `python scripts/generate.py` from packages/python-client.
from __future__ import annotations

from collections.abc import Mapping
from dataclasses import dataclass
from types import MappingProxyType
from typing import Any, Final, Literal, TypedDict, TypeAlias, overload

from devicerail.types import RequestHandle
from .models import device_capabilities_request as _device_capabilities_request
from .models import device_capabilities_response as _device_capabilities_response
from .models import device_capabilities_result as _device_capabilities_result
from .models import device_connect_request as _device_connect_request
from .models import device_connect_response as _device_connect_response
from .models import device_connect_result as _device_connect_result
from .models import device_disconnect_request as _device_disconnect_request
from .models import device_disconnect_response as _device_disconnect_response
from .models import device_disconnect_result as _device_disconnect_result
from .models import device_execute_request as _device_execute_request
from .models import device_execute_response as _device_execute_response
from .models import device_execute_result as _device_execute_result
from .models import device_execute_params as _device_execute_params
from .models import device_observe_request as _device_observe_request
from .models import device_observe_response as _device_observe_response
from .models import device_observe_result as _device_observe_result
from .models import device_select_request as _device_select_request
from .models import device_select_response as _device_select_response
from .models import device_select_result as _device_select_result
from .models import device_select_params as _device_select_params
from .models import devices_list_request as _devices_list_request
from .models import devices_list_response as _devices_list_response
from .models import devices_list_result as _devices_list_result
from .models import events_clear_request as _events_clear_request
from .models import events_clear_response as _events_clear_response
from .models import events_clear_result as _events_clear_result
from .models import session_target_params as _session_target_params
from .models import events_list_request as _events_list_request
from .models import events_list_response as _events_list_response
from .models import events_list_result as _events_list_result
from .models import events_list_params as _events_list_params
from .models import events_stream_open_request as _events_stream_open_request
from .models import events_stream_open_response as _events_stream_open_response
from .models import events_stream_open_result as _events_stream_open_result
from .models import events_stream_open_params as _events_stream_open_params
from .models import events_subscribe_request as _events_subscribe_request
from .models import events_subscribe_response as _events_subscribe_response
from .models import events_subscribe_result as _events_subscribe_result
from .models import events_subscribe_params as _events_subscribe_params
from .models import media_stream_capture_request as _media_stream_capture_request
from .models import media_stream_capture_response as _media_stream_capture_response
from .models import media_stream_capture_result as _media_stream_capture_result
from .models import media_stream_capture_params as _media_stream_capture_params
from .models import media_stream_end_request as _media_stream_end_request
from .models import media_stream_end_response as _media_stream_end_response
from .models import media_stream_end_result as _media_stream_end_result
from .models import media_stream_end_params as _media_stream_end_params
from .models import media_stream_start_request as _media_stream_start_request
from .models import media_stream_start_response as _media_stream_start_response
from .models import media_stream_start_result as _media_stream_start_result
from .models import media_stream_start_params as _media_stream_start_params
from .models import request_cancel_request as _request_cancel_request
from .models import request_cancel_response as _request_cancel_response
from .models import request_cancel_result as _request_cancel_result
from .models import request_cancel_params as _request_cancel_params
from .models import session_current_request as _session_current_request
from .models import session_current_response as _session_current_response
from .models import session_current_result as _session_current_result
from .models import session_end_request as _session_end_request
from .models import session_end_response as _session_end_response
from .models import session_end_result as _session_end_result
from .models import session_end_params as _session_end_params
from .models import session_export_request as _session_export_request
from .models import session_export_response as _session_export_response
from .models import session_export_result as _session_export_result
from .models import session_export_params as _session_export_params
from .models import session_start_request as _session_start_request
from .models import session_start_response as _session_start_response
from .models import session_start_result as _session_start_result
from .models import sessions_list_request as _sessions_list_request
from .models import sessions_list_response as _sessions_list_response
from .models import sessions_list_result as _sessions_list_result
from .models import system_describe_request as _system_describe_request
from .models import system_describe_response as _system_describe_response
from .models import system_describe_result as _system_describe_result
from .models import system_hello_request as _system_hello_request
from .models import system_hello_response as _system_hello_response
from .models import hello_result as _hello_result
from .models import hello_params as _hello_params
from .models import ui_snapshot_get_request as _ui_snapshot_get_request
from .models import ui_snapshot_get_response as _ui_snapshot_get_response
from .models import ui_snapshot_get_result as _ui_snapshot_get_result
from .models import ui_snapshot_get_params as _ui_snapshot_get_params
from .models import verdict_record_request as _verdict_record_request
from .models import verdict_record_response as _verdict_record_response
from .models import verdict_record_result as _verdict_record_result
from .models import verdict_record_params as _verdict_record_params


RpcMethod: TypeAlias = Literal['device.capabilities', 'device.connect', 'device.disconnect', 'device.execute', 'device.observe', 'device.select', 'devices.list', 'events.clear', 'events.list', 'events.stream.open', 'events.subscribe', 'media.stream.capture', 'media.stream.end', 'media.stream.start', 'request.cancel', 'session.current', 'session.end', 'session.export', 'session.start', 'sessions.list', 'system.describe', 'system.hello', 'ui.snapshot.get', 'verdict.record']
StdioRpcMethod: TypeAlias = Literal['device.capabilities', 'device.connect', 'device.disconnect', 'device.execute', 'device.observe', 'device.select', 'devices.list', 'events.clear', 'events.list', 'events.stream.open', 'media.stream.capture', 'media.stream.end', 'media.stream.start', 'request.cancel', 'session.current', 'session.end', 'session.export', 'session.start', 'sessions.list', 'system.describe', 'ui.snapshot.get', 'verdict.record']

@dataclass(frozen=True, slots=True)
class MethodSpec:
    request_schema: str
    response_schema: str
    params_required: bool
    timeout_supported: bool
    websocket_only: bool

METHOD_SPECS: Final[Mapping[RpcMethod, MethodSpec]] = MappingProxyType({
    'device.capabilities': MethodSpec('device-capabilities-request.schema.json', 'device-capabilities-response.schema.json', False, True, False),
    'device.connect': MethodSpec('device-connect-request.schema.json', 'device-connect-response.schema.json', False, True, False),
    'device.disconnect': MethodSpec('device-disconnect-request.schema.json', 'device-disconnect-response.schema.json', False, True, False),
    'device.execute': MethodSpec('device-execute-request.schema.json', 'device-execute-response.schema.json', True, True, False),
    'device.observe': MethodSpec('device-observe-request.schema.json', 'device-observe-response.schema.json', False, True, False),
    'device.select': MethodSpec('device-select-request.schema.json', 'device-select-response.schema.json', True, False, False),
    'devices.list': MethodSpec('devices-list-request.schema.json', 'devices-list-response.schema.json', False, False, False),
    'events.clear': MethodSpec('events-clear-request.schema.json', 'events-clear-response.schema.json', False, False, False),
    'events.list': MethodSpec('events-list-request.schema.json', 'events-list-response.schema.json', False, False, False),
    'events.stream.open': MethodSpec('events-stream-open-request.schema.json', 'events-stream-open-response.schema.json', True, False, False),
    'events.subscribe': MethodSpec('events-subscribe-request.schema.json', 'events-subscribe-response.schema.json', True, False, True),
    'media.stream.capture': MethodSpec('media-stream-capture-request.schema.json', 'media-stream-capture-response.schema.json', True, True, False),
    'media.stream.end': MethodSpec('media-stream-end-request.schema.json', 'media-stream-end-response.schema.json', True, False, False),
    'media.stream.start': MethodSpec('media-stream-start-request.schema.json', 'media-stream-start-response.schema.json', True, False, False),
    'request.cancel': MethodSpec('request-cancel-request.schema.json', 'request-cancel-response.schema.json', True, False, False),
    'session.current': MethodSpec('session-current-request.schema.json', 'session-current-response.schema.json', False, False, False),
    'session.end': MethodSpec('session-end-request.schema.json', 'session-end-response.schema.json', False, False, False),
    'session.export': MethodSpec('session-export-request.schema.json', 'session-export-response.schema.json', False, False, False),
    'session.start': MethodSpec('session-start-request.schema.json', 'session-start-response.schema.json', False, False, False),
    'sessions.list': MethodSpec('sessions-list-request.schema.json', 'sessions-list-response.schema.json', False, False, False),
    'system.describe': MethodSpec('system-describe-request.schema.json', 'system-describe-response.schema.json', False, False, False),
    'system.hello': MethodSpec('system-hello-request.schema.json', 'system-hello-response.schema.json', True, False, False),
    'ui.snapshot.get': MethodSpec('ui-snapshot-get-request.schema.json', 'ui-snapshot-get-response.schema.json', True, False, False),
    'verdict.record': MethodSpec('verdict-record-request.schema.json', 'verdict-record-response.schema.json', True, False, False),
})

class DeviceCapabilitiesMethod(TypedDict):
    request: _device_capabilities_request.DeviceCapabilitiesRequest
    response: _device_capabilities_response.DeviceCapabilitiesResponse

class DeviceConnectMethod(TypedDict):
    request: _device_connect_request.DeviceConnectRequest
    response: _device_connect_response.DeviceConnectResponse

class DeviceDisconnectMethod(TypedDict):
    request: _device_disconnect_request.DeviceDisconnectRequest
    response: _device_disconnect_response.DeviceDisconnectResponse

class DeviceExecuteMethod(TypedDict):
    request: _device_execute_request.DeviceExecuteRequest
    response: _device_execute_response.DeviceExecuteResponse

class DeviceObserveMethod(TypedDict):
    request: _device_observe_request.DeviceObserveRequest
    response: _device_observe_response.DeviceObserveResponse

class DeviceSelectMethod(TypedDict):
    request: _device_select_request.DeviceSelectRequest
    response: _device_select_response.DeviceSelectResponse

class DevicesListMethod(TypedDict):
    request: _devices_list_request.DevicesListRequest
    response: _devices_list_response.DevicesListResponse

class EventsClearMethod(TypedDict):
    request: _events_clear_request.EventsClearRequest
    response: _events_clear_response.EventsClearResponse

class EventsListMethod(TypedDict):
    request: _events_list_request.EventsListRequest
    response: _events_list_response.EventsListResponse

class EventsStreamOpenMethod(TypedDict):
    request: _events_stream_open_request.EventsStreamOpenRequest
    response: _events_stream_open_response.EventsStreamOpenResponse

class EventsSubscribeMethod(TypedDict):
    request: _events_subscribe_request.EventsSubscribeRequest
    response: _events_subscribe_response.EventsSubscribeResponse

class MediaStreamCaptureMethod(TypedDict):
    request: _media_stream_capture_request.MediaStreamCaptureRequest
    response: _media_stream_capture_response.MediaStreamCaptureResponse

class MediaStreamEndMethod(TypedDict):
    request: _media_stream_end_request.MediaStreamEndRequest
    response: _media_stream_end_response.MediaStreamEndResponse

class MediaStreamStartMethod(TypedDict):
    request: _media_stream_start_request.MediaStreamStartRequest
    response: _media_stream_start_response.MediaStreamStartResponse

class RequestCancelMethod(TypedDict):
    request: _request_cancel_request.RequestCancelRequest
    response: _request_cancel_response.RequestCancelResponse

class SessionCurrentMethod(TypedDict):
    request: _session_current_request.SessionCurrentRequest
    response: _session_current_response.SessionCurrentResponse

class SessionEndMethod(TypedDict):
    request: _session_end_request.SessionEndRequest
    response: _session_end_response.SessionEndResponse

class SessionExportMethod(TypedDict):
    request: _session_export_request.SessionExportRequest
    response: _session_export_response.SessionExportResponse

class SessionStartMethod(TypedDict):
    request: _session_start_request.SessionStartRequest
    response: _session_start_response.SessionStartResponse

class SessionsListMethod(TypedDict):
    request: _sessions_list_request.SessionsListRequest
    response: _sessions_list_response.SessionsListResponse

class SystemDescribeMethod(TypedDict):
    request: _system_describe_request.SystemDescribeRequest
    response: _system_describe_response.SystemDescribeResponse

class SystemHelloMethod(TypedDict):
    request: _system_hello_request.SystemHelloRequest
    response: _system_hello_response.SystemHelloResponse

class UiSnapshotGetMethod(TypedDict):
    request: _ui_snapshot_get_request.UiSnapshotGetRequest
    response: _ui_snapshot_get_response.UiSnapshotGetResponse

class VerdictRecordMethod(TypedDict):
    request: _verdict_record_request.VerdictRecordRequest
    response: _verdict_record_response.VerdictRecordResponse

RpcMethodMap = TypedDict(
    "RpcMethodMap",
    {
        'device.capabilities': DeviceCapabilitiesMethod,
        'device.connect': DeviceConnectMethod,
        'device.disconnect': DeviceDisconnectMethod,
        'device.execute': DeviceExecuteMethod,
        'device.observe': DeviceObserveMethod,
        'device.select': DeviceSelectMethod,
        'devices.list': DevicesListMethod,
        'events.clear': EventsClearMethod,
        'events.list': EventsListMethod,
        'events.stream.open': EventsStreamOpenMethod,
        'events.subscribe': EventsSubscribeMethod,
        'media.stream.capture': MediaStreamCaptureMethod,
        'media.stream.end': MediaStreamEndMethod,
        'media.stream.start': MediaStreamStartMethod,
        'request.cancel': RequestCancelMethod,
        'session.current': SessionCurrentMethod,
        'session.end': SessionEndMethod,
        'session.export': SessionExportMethod,
        'session.start': SessionStartMethod,
        'sessions.list': SessionsListMethod,
        'system.describe': SystemDescribeMethod,
        'system.hello': SystemHelloMethod,
        'ui.snapshot.get': UiSnapshotGetMethod,
        'verdict.record': VerdictRecordMethod,
    },
)

class GeneratedClientMethods:
    @overload
    async def call(
        self,
        method: Literal['device.capabilities'],
        params: _device_capabilities_request.NoParamsSchema | None = None,
        *,
        timeout_ms: int | None = None,
    ) -> _device_capabilities_result.DeviceCapabilitiesResult: ...

    @overload
    async def call(
        self,
        method: Literal['device.connect'],
        params: _device_connect_request.NoParamsSchema | None = None,
        *,
        timeout_ms: int | None = None,
    ) -> _device_connect_result.DeviceConnectResult: ...

    @overload
    async def call(
        self,
        method: Literal['device.disconnect'],
        params: _device_disconnect_request.NoParamsSchema | None = None,
        *,
        timeout_ms: int | None = None,
    ) -> _device_disconnect_result.DeviceDisconnectResult: ...

    @overload
    async def call(
        self,
        method: Literal['device.execute'],
        params: _device_execute_params.DeviceExecuteParams,
        *,
        timeout_ms: int | None = None,
    ) -> _device_execute_result.DeviceExecuteResult: ...

    @overload
    async def call(
        self,
        method: Literal['device.observe'],
        params: _device_observe_request.NoParamsSchema | None = None,
        *,
        timeout_ms: int | None = None,
    ) -> _device_observe_result.DeviceObserveResult: ...

    @overload
    async def call(
        self,
        method: Literal['device.select'],
        params: _device_select_params.DeviceSelectParams,
    ) -> _device_select_result.DeviceSelectResult: ...

    @overload
    async def call(
        self,
        method: Literal['devices.list'],
        params: _devices_list_request.NoParamsSchema | None = None,
    ) -> _devices_list_result.DevicesListResult: ...

    @overload
    async def call(
        self,
        method: Literal['events.clear'],
        params: _session_target_params.SessionTargetParams | None = None,
    ) -> _events_clear_result.EventsClearResult: ...

    @overload
    async def call(
        self,
        method: Literal['events.list'],
        params: _events_list_params.EventsListParams | None = None,
    ) -> _events_list_result.EventsListResult: ...

    @overload
    async def call(
        self,
        method: Literal['events.stream.open'],
        params: _events_stream_open_params.EventsStreamOpenParams,
    ) -> _events_stream_open_result.EventsStreamOpenResult: ...

    @overload
    async def call(
        self,
        method: Literal['events.subscribe'],
        params: _events_subscribe_params.EventsSubscribeParams,
    ) -> _events_subscribe_result.EventsSubscribeResult: ...

    @overload
    async def call(
        self,
        method: Literal['media.stream.capture'],
        params: _media_stream_capture_params.MediaStreamCaptureParams,
        *,
        timeout_ms: int | None = None,
    ) -> _media_stream_capture_result.MediaStreamCaptureResult: ...

    @overload
    async def call(
        self,
        method: Literal['media.stream.end'],
        params: _media_stream_end_params.MediaStreamEndParams,
    ) -> _media_stream_end_result.MediaStreamEndResult: ...

    @overload
    async def call(
        self,
        method: Literal['media.stream.start'],
        params: _media_stream_start_params.MediaStreamStartParams,
    ) -> _media_stream_start_result.MediaStreamStartResult: ...

    @overload
    async def call(
        self,
        method: Literal['request.cancel'],
        params: _request_cancel_params.RequestCancelParams,
    ) -> _request_cancel_result.RequestCancelResult: ...

    @overload
    async def call(
        self,
        method: Literal['session.current'],
        params: _session_current_request.NoParamsSchema | None = None,
    ) -> _session_current_result.SessionCurrentResult: ...

    @overload
    async def call(
        self,
        method: Literal['session.end'],
        params: _session_end_params.SessionEndParams | None = None,
    ) -> _session_end_result.SessionEndResult: ...

    @overload
    async def call(
        self,
        method: Literal['session.export'],
        params: _session_export_params.SessionExportParams | None = None,
    ) -> _session_export_result.SessionExportResult: ...

    @overload
    async def call(
        self,
        method: Literal['session.start'],
        params: _session_start_request.NoParamsSchema | None = None,
    ) -> _session_start_result.SessionStartResult: ...

    @overload
    async def call(
        self,
        method: Literal['sessions.list'],
        params: _sessions_list_request.NoParamsSchema | None = None,
    ) -> _sessions_list_result.SessionsListResult: ...

    @overload
    async def call(
        self,
        method: Literal['system.describe'],
        params: _system_describe_request.NoParamsSchema | None = None,
    ) -> _system_describe_result.SystemDescribeResult: ...

    @overload
    async def call(
        self,
        method: Literal['ui.snapshot.get'],
        params: _ui_snapshot_get_params.UiSnapshotGetParams,
    ) -> _ui_snapshot_get_result.UiSnapshotGetResult: ...

    @overload
    async def call(
        self,
        method: Literal['verdict.record'],
        params: _verdict_record_params.VerdictRecordParams,
    ) -> _verdict_record_result.VerdictRecordResult: ...

    async def call(
        self,
        method: RpcMethod,
        params: Any = None,
        *,
        timeout_ms: int | None = None,
    ) -> Any:
        return await self._call(method, params, timeout_ms=timeout_ms)

    @overload
    async def begin_call(
        self,
        method: Literal['device.capabilities'],
        params: _device_capabilities_request.NoParamsSchema | None = None,
        *,
        timeout_ms: int | None = None,
    ) -> RequestHandle[_device_capabilities_result.DeviceCapabilitiesResult]: ...

    @overload
    async def begin_call(
        self,
        method: Literal['device.connect'],
        params: _device_connect_request.NoParamsSchema | None = None,
        *,
        timeout_ms: int | None = None,
    ) -> RequestHandle[_device_connect_result.DeviceConnectResult]: ...

    @overload
    async def begin_call(
        self,
        method: Literal['device.disconnect'],
        params: _device_disconnect_request.NoParamsSchema | None = None,
        *,
        timeout_ms: int | None = None,
    ) -> RequestHandle[_device_disconnect_result.DeviceDisconnectResult]: ...

    @overload
    async def begin_call(
        self,
        method: Literal['device.execute'],
        params: _device_execute_params.DeviceExecuteParams,
        *,
        timeout_ms: int | None = None,
    ) -> RequestHandle[_device_execute_result.DeviceExecuteResult]: ...

    @overload
    async def begin_call(
        self,
        method: Literal['device.observe'],
        params: _device_observe_request.NoParamsSchema | None = None,
        *,
        timeout_ms: int | None = None,
    ) -> RequestHandle[_device_observe_result.DeviceObserveResult]: ...

    @overload
    async def begin_call(
        self,
        method: Literal['device.select'],
        params: _device_select_params.DeviceSelectParams,
    ) -> RequestHandle[_device_select_result.DeviceSelectResult]: ...

    @overload
    async def begin_call(
        self,
        method: Literal['devices.list'],
        params: _devices_list_request.NoParamsSchema | None = None,
    ) -> RequestHandle[_devices_list_result.DevicesListResult]: ...

    @overload
    async def begin_call(
        self,
        method: Literal['events.clear'],
        params: _session_target_params.SessionTargetParams | None = None,
    ) -> RequestHandle[_events_clear_result.EventsClearResult]: ...

    @overload
    async def begin_call(
        self,
        method: Literal['events.list'],
        params: _events_list_params.EventsListParams | None = None,
    ) -> RequestHandle[_events_list_result.EventsListResult]: ...

    @overload
    async def begin_call(
        self,
        method: Literal['events.stream.open'],
        params: _events_stream_open_params.EventsStreamOpenParams,
    ) -> RequestHandle[_events_stream_open_result.EventsStreamOpenResult]: ...

    @overload
    async def begin_call(
        self,
        method: Literal['events.subscribe'],
        params: _events_subscribe_params.EventsSubscribeParams,
    ) -> RequestHandle[_events_subscribe_result.EventsSubscribeResult]: ...

    @overload
    async def begin_call(
        self,
        method: Literal['media.stream.capture'],
        params: _media_stream_capture_params.MediaStreamCaptureParams,
        *,
        timeout_ms: int | None = None,
    ) -> RequestHandle[_media_stream_capture_result.MediaStreamCaptureResult]: ...

    @overload
    async def begin_call(
        self,
        method: Literal['media.stream.end'],
        params: _media_stream_end_params.MediaStreamEndParams,
    ) -> RequestHandle[_media_stream_end_result.MediaStreamEndResult]: ...

    @overload
    async def begin_call(
        self,
        method: Literal['media.stream.start'],
        params: _media_stream_start_params.MediaStreamStartParams,
    ) -> RequestHandle[_media_stream_start_result.MediaStreamStartResult]: ...

    @overload
    async def begin_call(
        self,
        method: Literal['request.cancel'],
        params: _request_cancel_params.RequestCancelParams,
    ) -> RequestHandle[_request_cancel_result.RequestCancelResult]: ...

    @overload
    async def begin_call(
        self,
        method: Literal['session.current'],
        params: _session_current_request.NoParamsSchema | None = None,
    ) -> RequestHandle[_session_current_result.SessionCurrentResult]: ...

    @overload
    async def begin_call(
        self,
        method: Literal['session.end'],
        params: _session_end_params.SessionEndParams | None = None,
    ) -> RequestHandle[_session_end_result.SessionEndResult]: ...

    @overload
    async def begin_call(
        self,
        method: Literal['session.export'],
        params: _session_export_params.SessionExportParams | None = None,
    ) -> RequestHandle[_session_export_result.SessionExportResult]: ...

    @overload
    async def begin_call(
        self,
        method: Literal['session.start'],
        params: _session_start_request.NoParamsSchema | None = None,
    ) -> RequestHandle[_session_start_result.SessionStartResult]: ...

    @overload
    async def begin_call(
        self,
        method: Literal['sessions.list'],
        params: _sessions_list_request.NoParamsSchema | None = None,
    ) -> RequestHandle[_sessions_list_result.SessionsListResult]: ...

    @overload
    async def begin_call(
        self,
        method: Literal['system.describe'],
        params: _system_describe_request.NoParamsSchema | None = None,
    ) -> RequestHandle[_system_describe_result.SystemDescribeResult]: ...

    @overload
    async def begin_call(
        self,
        method: Literal['ui.snapshot.get'],
        params: _ui_snapshot_get_params.UiSnapshotGetParams,
    ) -> RequestHandle[_ui_snapshot_get_result.UiSnapshotGetResult]: ...

    @overload
    async def begin_call(
        self,
        method: Literal['verdict.record'],
        params: _verdict_record_params.VerdictRecordParams,
    ) -> RequestHandle[_verdict_record_result.VerdictRecordResult]: ...

    async def begin_call(
        self,
        method: RpcMethod,
        params: Any = None,
        *,
        timeout_ms: int | None = None,
    ) -> Any:
        return await self._begin_call(method, params, timeout_ms=timeout_ms)

    async def _call(
        self, method: RpcMethod, params: Any, *, timeout_ms: int | None
    ) -> Any:
        raise NotImplementedError

    async def _begin_call(
        self, method: RpcMethod, params: Any, *, timeout_ms: int | None
    ) -> RequestHandle[Any]:
        raise NotImplementedError

__all__ = [
    "GeneratedClientMethods",
    "METHOD_SPECS",
    "MethodSpec",
    "RpcMethod",
    "RpcMethodMap",
    "StdioRpcMethod",
]
