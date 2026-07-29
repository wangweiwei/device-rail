# Generated from protocol/schema/v1. DO NOT EDIT.
# Run `python scripts/generate.py` from packages/python-client.
from __future__ import annotations

from typing import Any, Literal, Never, NotRequired, TypeAlias, TypedDict

# Source: protocol/schema/v1/device-disconnect-request.schema.json

class DeviceDisconnectRequest(TypedDict):
    id: RpcIdSchema
    jsonrpc: JsonRpcVersion
    method: DeviceDisconnectMethodSchema
    params: NotRequired[NoParamsSchema]
    timeoutMs: NotRequired[RequestTimeoutMs]

class EmptyParamsObjectSchema(TypedDict):
    pass

DeviceDisconnectMethodSchema: TypeAlias = Literal['device.disconnect']

JsonRpcVersion: TypeAlias = Literal['2.0']

NoParamsSchema: TypeAlias = EmptyParamsObjectSchema | list[Never]

RequestTimeoutMs: TypeAlias = int

RpcIdSchema: TypeAlias = str | int

__all__ = ['DeviceDisconnectRequest']
