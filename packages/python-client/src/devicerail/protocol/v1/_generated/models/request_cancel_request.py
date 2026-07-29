# Generated from protocol/schema/v1. DO NOT EDIT.
# Run `python scripts/generate.py` from packages/python-client.
from __future__ import annotations

from typing import Any, Literal, Never, NotRequired, TypeAlias, TypedDict

# Source: protocol/schema/v1/request-cancel-request.schema.json

class RequestCancelParams(TypedDict):
    requestId: RpcIdSchema

class RequestCancelRequest(TypedDict):
    id: RpcIdSchema
    jsonrpc: JsonRpcVersion
    method: RequestCancelMethodSchema
    params: RequestCancelParams

JsonRpcVersion: TypeAlias = Literal['2.0']

RequestCancelMethodSchema: TypeAlias = Literal['request.cancel']

RpcIdSchema: TypeAlias = str | int

__all__ = ['RequestCancelRequest']
