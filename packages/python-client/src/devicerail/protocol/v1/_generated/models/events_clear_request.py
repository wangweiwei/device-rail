# Generated from protocol/schema/v1. DO NOT EDIT.
# Run `python scripts/generate.py` from packages/python-client.
from __future__ import annotations

from typing import Any, Literal, Never, NotRequired, TypeAlias, TypedDict

# Source: protocol/schema/v1/events-clear-request.schema.json

class EventsClearRequest(TypedDict):
    id: RpcIdSchema
    jsonrpc: JsonRpcVersion
    method: EventsClearMethodSchema
    params: NotRequired[SessionTargetParams]

class SessionTargetParams(TypedDict):
    sessionId: NotRequired[str | None]

EventsClearMethodSchema: TypeAlias = Literal['events.clear']

JsonRpcVersion: TypeAlias = Literal['2.0']

RpcIdSchema: TypeAlias = str | int

__all__ = ['EventsClearRequest']
