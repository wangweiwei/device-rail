# Generated from protocol/schema/v1. DO NOT EDIT.
# Run `python scripts/generate.py` from packages/python-client.
from __future__ import annotations

from typing import Any, Literal, Never, NotRequired, TypeAlias, TypedDict

# Source: protocol/schema/v1/device-disconnect-response.schema.json

class DeviceDisconnectResult(TypedDict):
    disconnected: bool

class DeviceDisconnectSuccessSchema(TypedDict):
    id: RpcIdSchema
    jsonrpc: JsonRpcVersion
    result: DeviceDisconnectResult

class ErrorInfo(TypedDict):
    code: str
    details: NotRequired[Any]
    message: str
    retryable: bool

class RpcError(TypedDict):
    code: int
    data: ErrorInfo
    message: str

class SystemHelloFailureSchema(TypedDict):
    error: RpcError
    id: NullableRpcIdSchema
    jsonrpc: JsonRpcVersion

JsonRpcVersion: TypeAlias = Literal['2.0']

RpcIdSchema: TypeAlias = str | int

NullableRpcIdSchema: TypeAlias = RpcIdSchema | None

DeviceDisconnectResponse: TypeAlias = DeviceDisconnectSuccessSchema | SystemHelloFailureSchema

__all__ = ['DeviceDisconnectResponse']
