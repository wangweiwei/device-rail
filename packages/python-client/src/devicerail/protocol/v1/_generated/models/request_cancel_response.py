# Generated from protocol/schema/v1. DO NOT EDIT.
# Run `python scripts/generate.py` from packages/python-client.
from __future__ import annotations

from typing import Any, Literal, Never, NotRequired, TypeAlias, TypedDict

# Source: protocol/schema/v1/request-cancel-response.schema.json

class ErrorInfo(TypedDict):
    code: str
    details: NotRequired[Any]
    message: str
    retryable: bool

class RequestCancelResult(TypedDict):
    requestId: RpcIdSchema
    status: RequestCancelStatus

class RequestCancelSuccessSchema(TypedDict):
    id: RpcIdSchema
    jsonrpc: JsonRpcVersion
    result: RequestCancelResult

class RpcError(TypedDict):
    code: int
    data: ErrorInfo
    message: str

class SystemHelloFailureSchema(TypedDict):
    error: RpcError
    id: NullableRpcIdSchema
    jsonrpc: JsonRpcVersion

JsonRpcVersion: TypeAlias = Literal['2.0']

RequestCancelStatus: TypeAlias = Literal['requested', 'alreadyRequested', 'notFound']

RpcIdSchema: TypeAlias = str | int

NullableRpcIdSchema: TypeAlias = RpcIdSchema | None

RequestCancelResponse: TypeAlias = RequestCancelSuccessSchema | SystemHelloFailureSchema

__all__ = ['RequestCancelResponse']
