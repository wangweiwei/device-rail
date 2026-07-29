# Generated from protocol/schema/v1. DO NOT EDIT.
# Run `python scripts/generate.py` from packages/python-client.
from __future__ import annotations

from typing import Any, Literal, Never, NotRequired, TypeAlias, TypedDict

# Source: protocol/schema/v1/events-subscribe-response.schema.json

class ErrorInfo(TypedDict):
    code: str
    details: NotRequired[Any]
    message: str
    retryable: bool

class EventStreamCursor(TypedDict):
    sequence: EventSequence
    sessionId: str
    streamEpoch: EventStreamEpoch

class EventsSubscribeResult(TypedDict):
    replayThrough: EventStreamCursor
    sessionId: str
    sessionState: SessionState
    subscriptionId: str

class EventsSubscribeSuccessSchema(TypedDict):
    id: RpcIdSchema
    jsonrpc: JsonRpcVersion
    result: EventsSubscribeResult

class RpcError(TypedDict):
    code: int
    data: ErrorInfo
    message: str

class SystemHelloFailureSchema(TypedDict):
    error: RpcError
    id: NullableRpcIdSchema
    jsonrpc: JsonRpcVersion

EventSequence: TypeAlias = int

EventStreamEpoch: TypeAlias = str

JsonRpcVersion: TypeAlias = Literal['2.0']

RpcIdSchema: TypeAlias = str | int

SessionState: TypeAlias = Literal['active', 'ended']

NullableRpcIdSchema: TypeAlias = RpcIdSchema | None

EventsSubscribeResponse: TypeAlias = EventsSubscribeSuccessSchema | SystemHelloFailureSchema

__all__ = ['EventsSubscribeResponse']
