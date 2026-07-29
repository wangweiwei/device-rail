# Generated from protocol/schema/v1. DO NOT EDIT.
# Run `python scripts/generate.py` from packages/python-client.
from __future__ import annotations

from typing import Any, Literal, Never, NotRequired, TypeAlias, TypedDict

# Source: protocol/schema/v1/sessions-list-request.schema.json

class EmptyParamsObjectSchema(TypedDict):
    pass

class SessionsListRequest(TypedDict):
    id: RpcIdSchema
    jsonrpc: JsonRpcVersion
    method: SessionsListMethodSchema
    params: NotRequired[NoParamsSchema]

JsonRpcVersion: TypeAlias = Literal['2.0']

NoParamsSchema: TypeAlias = EmptyParamsObjectSchema | list[Never]

RpcIdSchema: TypeAlias = str | int

SessionsListMethodSchema: TypeAlias = Literal['sessions.list']

__all__ = ['SessionsListRequest']
