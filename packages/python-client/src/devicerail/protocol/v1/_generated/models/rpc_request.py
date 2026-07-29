# Generated from protocol/schema/v1. DO NOT EDIT.
# Run `python scripts/generate.py` from packages/python-client.
from __future__ import annotations

from typing import Any, Literal, Never, NotRequired, TypeAlias, TypedDict

# Source: protocol/schema/v1/rpc-request.schema.json

class RpcRequest(TypedDict):
    id: RpcIdSchema
    jsonrpc: JsonRpcVersion
    method: str
    params: NotRequired[RpcParams]
    timeoutMs: NotRequired[RequestTimeoutMs]

JsonRpcVersion: TypeAlias = Literal['2.0']

RequestTimeoutMs: TypeAlias = int

RpcIdSchema: TypeAlias = str | int

RpcParams: TypeAlias = dict[str, Any] | list[Any]

__all__ = ['RpcRequest']
