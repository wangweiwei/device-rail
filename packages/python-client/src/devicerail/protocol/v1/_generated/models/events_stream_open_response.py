# Generated from protocol/schema/v1. DO NOT EDIT.
# Run `python scripts/generate.py` from packages/python-client.
from __future__ import annotations

from typing import Any, Literal, Never, NotRequired, TypeAlias, TypedDict

# Source: protocol/schema/v1/events-stream-open-response.schema.json

class ErrorInfo(TypedDict):
    code: str
    details: NotRequired[Any]
    message: str
    retryable: bool

class EventsStreamOpenResult(TypedDict):
    endpoint: EventStreamEndpoint
    expiresAtMs: int
    streamEpoch: EventStreamEpoch

class EventsStreamOpenSuccessSchema(TypedDict):
    id: RpcIdSchema
    jsonrpc: JsonRpcVersion
    result: EventsStreamOpenResult

class RpcError(TypedDict):
    code: int
    data: ErrorInfo
    message: str

class SystemHelloFailureSchema(TypedDict):
    error: RpcError
    id: NullableRpcIdSchema
    jsonrpc: JsonRpcVersion

EventStreamEndpoint: TypeAlias = str

EventStreamEpoch: TypeAlias = str

JsonRpcVersion: TypeAlias = Literal['2.0']

RpcIdSchema: TypeAlias = str | int

NullableRpcIdSchema: TypeAlias = RpcIdSchema | None

EventsStreamOpenResponse: TypeAlias = EventsStreamOpenSuccessSchema | SystemHelloFailureSchema

__all__ = ['EventsStreamOpenResponse']
