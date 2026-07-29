# Generated from protocol/schema/v1. DO NOT EDIT.
# Run `python scripts/generate.py` from packages/python-client.
from __future__ import annotations

from typing import Any, Literal, Never, NotRequired, TypeAlias, TypedDict

# Source: protocol/schema/v1/device-execute-request.schema.json

class DeviceExecuteParams(TypedDict):
    actionTimeoutMs: NotRequired[RequestTimeoutMs]
    arguments: NotRequired[Any]
    id: str
    name: str

class DeviceExecuteRequest(TypedDict):
    id: RpcIdSchema
    jsonrpc: JsonRpcVersion
    method: DeviceExecuteMethodSchema
    params: DeviceExecuteParams
    timeoutMs: NotRequired[RequestTimeoutMs]

DeviceExecuteMethodSchema: TypeAlias = Literal['device.execute']

JsonRpcVersion: TypeAlias = Literal['2.0']

RequestTimeoutMs: TypeAlias = int

RpcIdSchema: TypeAlias = str | int

__all__ = ['DeviceExecuteRequest']
