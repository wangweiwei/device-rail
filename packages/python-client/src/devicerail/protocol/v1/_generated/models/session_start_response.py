# Generated from protocol/schema/v1. DO NOT EDIT.
# Run `python scripts/generate.py` from packages/python-client.
from __future__ import annotations

from typing import Any, Literal, Never, NotRequired, TypeAlias, TypedDict

# Source: protocol/schema/v1/session-start-response.schema.json

class ErrorInfo(TypedDict):
    code: str
    details: NotRequired[Any]
    message: str
    retryable: bool

class RpcError(TypedDict):
    code: int
    data: ErrorInfo
    message: str

class SessionInfo(TypedDict):
    endedAtMs: NotRequired[int | None]
    eventCount: EventSequence
    id: str
    lastSequence: EventSequence
    startedAtMs: int
    state: SessionState

class SessionStartSuccessSchema(TypedDict):
    id: RpcIdSchema
    jsonrpc: JsonRpcVersion
    result: SessionInfo

class SystemHelloFailureSchema(TypedDict):
    error: RpcError
    id: NullableRpcIdSchema
    jsonrpc: JsonRpcVersion

EventSequence: TypeAlias = int

JsonRpcVersion: TypeAlias = Literal['2.0']

RpcIdSchema: TypeAlias = str | int

SessionState: TypeAlias = Literal['active', 'ended']

NullableRpcIdSchema: TypeAlias = RpcIdSchema | None

SessionStartResponse: TypeAlias = SessionStartSuccessSchema | SystemHelloFailureSchema

__all__ = ['SessionStartResponse']
