# Generated from protocol/schema/v1. DO NOT EDIT.
# Run `python scripts/generate.py` from packages/python-client.
from __future__ import annotations

from typing import Any, Literal, Never, NotRequired, TypeAlias, TypedDict

# Source: protocol/schema/v1/devices-list-request.schema.json

class DevicesListRequest(TypedDict):
    id: RpcIdSchema
    jsonrpc: JsonRpcVersion
    method: DevicesListMethodSchema
    params: NotRequired[NoParamsSchema]

class EmptyParamsObjectSchema(TypedDict):
    pass

DevicesListMethodSchema: TypeAlias = Literal['devices.list']

JsonRpcVersion: TypeAlias = Literal['2.0']

NoParamsSchema: TypeAlias = EmptyParamsObjectSchema | list[Never]

RpcIdSchema: TypeAlias = str | int

__all__ = ['DevicesListRequest']
