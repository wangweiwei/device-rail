# Generated from protocol/schema/v1. DO NOT EDIT.
# Run `python scripts/generate.py` from packages/python-client.
from __future__ import annotations

from typing import Any, Literal, Never, NotRequired, TypeAlias, TypedDict

# Source: protocol/schema/v1/events-list-request.schema.json

class EventsListParams(TypedDict):
    afterSequence: NotRequired[EventSequence | None]
    limit: NotRequired[int | None]
    sessionId: NotRequired[str | None]

class EventsListRequest(TypedDict):
    id: RpcIdSchema
    jsonrpc: JsonRpcVersion
    method: EventsListMethodSchema
    params: NotRequired[EventsListParams]

EventSequence: TypeAlias = int

EventsListMethodSchema: TypeAlias = Literal['events.list']

JsonRpcVersion: TypeAlias = Literal['2.0']

RpcIdSchema: TypeAlias = str | int

__all__ = ['EventsListRequest']
