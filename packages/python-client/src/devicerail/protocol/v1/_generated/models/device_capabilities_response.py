# Generated from protocol/schema/v1. DO NOT EDIT.
# Run `python scripts/generate.py` from packages/python-client.
from __future__ import annotations

from typing import Any, Literal, Never, NotRequired, TypeAlias, TypedDict

# Source: protocol/schema/v1/device-capabilities-response.schema.json

class ActionDefinition(TypedDict):
    description: str
    inputSchema: Any
    name: str
    protection: NotRequired[ActionProtection]

class DeviceCapabilitiesSuccessSchema(TypedDict):
    id: RpcIdSchema
    jsonrpc: JsonRpcVersion
    result: list[ActionDefinition]

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

ActionProtection: TypeAlias = Literal['standard', 'protected']

JsonRpcVersion: TypeAlias = Literal['2.0']

RpcIdSchema: TypeAlias = str | int

NullableRpcIdSchema: TypeAlias = RpcIdSchema | None

DeviceCapabilitiesResponse: TypeAlias = DeviceCapabilitiesSuccessSchema | SystemHelloFailureSchema

__all__ = ['DeviceCapabilitiesResponse']
