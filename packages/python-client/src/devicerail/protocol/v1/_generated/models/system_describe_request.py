# Generated from protocol/schema/v1. DO NOT EDIT.
# Run `python scripts/generate.py` from packages/python-client.
from __future__ import annotations

from typing import Any, Literal, Never, NotRequired, TypeAlias, TypedDict

# Source: protocol/schema/v1/system-describe-request.schema.json

class EmptyParamsObjectSchema(TypedDict):
    pass

class SystemDescribeRequest(TypedDict):
    id: RpcIdSchema
    jsonrpc: JsonRpcVersion
    method: SystemDescribeMethodSchema
    params: NotRequired[NoParamsSchema]

JsonRpcVersion: TypeAlias = Literal['2.0']

NoParamsSchema: TypeAlias = EmptyParamsObjectSchema | list[Never]

RpcIdSchema: TypeAlias = str | int

SystemDescribeMethodSchema: TypeAlias = Literal['system.describe']

__all__ = ['SystemDescribeRequest']
