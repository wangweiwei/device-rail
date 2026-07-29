# Generated from protocol/schema/v1. DO NOT EDIT.
# Run `python scripts/generate.py` from packages/python-client.
from __future__ import annotations

from typing import Any, Literal, Never, NotRequired, TypeAlias, TypedDict

# Source: protocol/schema/v1/events-subscribe-request.schema.json

class EventStreamCursor(TypedDict):
    sequence: EventSequence
    sessionId: str
    streamEpoch: EventStreamEpoch

class EventsSubscribeParams(TypedDict):
    afterCursor: NotRequired[EventStreamCursor | None]
    sessionId: str

class EventsSubscribeRequest(TypedDict):
    id: RpcIdSchema
    jsonrpc: JsonRpcVersion
    method: EventsSubscribeMethodSchema
    params: EventsSubscribeParams

EventSequence: TypeAlias = int

EventStreamEpoch: TypeAlias = str

EventsSubscribeMethodSchema: TypeAlias = Literal['events.subscribe']

JsonRpcVersion: TypeAlias = Literal['2.0']

RpcIdSchema: TypeAlias = str | int

__all__ = ['EventsSubscribeRequest']
