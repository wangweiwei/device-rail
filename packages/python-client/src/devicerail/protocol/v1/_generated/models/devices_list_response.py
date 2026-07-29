# Generated from protocol/schema/v1. DO NOT EDIT.
# Run `python scripts/generate.py` from packages/python-client.
from __future__ import annotations

from typing import Any, Literal, Never, NotRequired, TypeAlias, TypedDict

# Source: protocol/schema/v1/devices-list-response.schema.json

class DeviceInfo(TypedDict):
    connected: bool
    id: str
    name: str
    osVersion: NotRequired[str | None]
    platform: Platform

class DevicesListResult(TypedDict):
    devices: list[DeviceInfo]
    selectedDeviceId: NotRequired[str | None]

class DevicesListSuccessSchema(TypedDict):
    id: RpcIdSchema
    jsonrpc: JsonRpcVersion
    result: DevicesListResult

class ErrorInfo(TypedDict):
    code: str
    details: NotRequired[Any]
    message: str
    retryable: bool

class PlatformVariant1(TypedDict):
    kind: Literal['web']

class PlatformVariant10(TypedDict):
    kind: Literal['other']
    value: str

class PlatformVariant2(TypedDict):
    kind: Literal['android']

class PlatformVariant3(TypedDict):
    kind: Literal['ios']

class PlatformVariant4(TypedDict):
    kind: Literal['harmonyOs']

class PlatformVariant5(TypedDict):
    kind: Literal['macOs']

class PlatformVariant6(TypedDict):
    kind: Literal['windows']

class PlatformVariant7(TypedDict):
    kind: Literal['linux']

class PlatformVariant8(TypedDict):
    kind: Literal['rdp']

class PlatformVariant9(TypedDict):
    kind: Literal['mock']

class RpcError(TypedDict):
    code: int
    data: ErrorInfo
    message: str

class SystemHelloFailureSchema(TypedDict):
    error: RpcError
    id: NullableRpcIdSchema
    jsonrpc: JsonRpcVersion

JsonRpcVersion: TypeAlias = Literal['2.0']

Platform: TypeAlias = PlatformVariant1 | PlatformVariant2 | PlatformVariant3 | PlatformVariant4 | PlatformVariant5 | PlatformVariant6 | PlatformVariant7 | PlatformVariant8 | PlatformVariant9 | PlatformVariant10

RpcIdSchema: TypeAlias = str | int

NullableRpcIdSchema: TypeAlias = RpcIdSchema | None

DevicesListResponse: TypeAlias = DevicesListSuccessSchema | SystemHelloFailureSchema

__all__ = ['DevicesListResponse']
