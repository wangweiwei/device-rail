# Generated from protocol/schema/v1. DO NOT EDIT.
# Run `python scripts/generate.py` from packages/python-client.
from __future__ import annotations

from typing import Any, Literal, Never, NotRequired, TypeAlias, TypedDict

# Source: protocol/schema/v1/session-current-request.schema.json

class EmptyParamsObjectSchema(TypedDict):
    pass

class SessionCurrentRequest(TypedDict):
    id: RpcIdSchema
    jsonrpc: JsonRpcVersion
    method: SessionCurrentMethodSchema
    params: NotRequired[NoParamsSchema]

JsonRpcVersion: TypeAlias = Literal['2.0']

NoParamsSchema: TypeAlias = EmptyParamsObjectSchema | list[Never]

RpcIdSchema: TypeAlias = str | int

SessionCurrentMethodSchema: TypeAlias = Literal['session.current']

__all__ = ['SessionCurrentRequest']
