# Generated from protocol/schema/v1. DO NOT EDIT.
# Run `python scripts/generate.py` from packages/python-client.
from __future__ import annotations

from typing import Any, Literal, Never, NotRequired, TypeAlias, TypedDict

# Source: protocol/schema/v1/device-select-request.schema.json

class DeviceSelectParams(TypedDict):
    deviceId: str

class DeviceSelectRequest(TypedDict):
    id: RpcIdSchema
    jsonrpc: JsonRpcVersion
    method: DeviceSelectMethodSchema
    params: DeviceSelectParams

DeviceSelectMethodSchema: TypeAlias = Literal['device.select']

JsonRpcVersion: TypeAlias = Literal['2.0']

RpcIdSchema: TypeAlias = str | int

__all__ = ['DeviceSelectRequest']
