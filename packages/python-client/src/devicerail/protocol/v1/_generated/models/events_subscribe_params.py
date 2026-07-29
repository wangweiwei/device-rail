# Generated from protocol/schema/v1. DO NOT EDIT.
# Run `python scripts/generate.py` from packages/python-client.
from __future__ import annotations

from typing import Any, Literal, Never, NotRequired, TypeAlias, TypedDict

# Source: protocol/schema/v1/events-subscribe-params.schema.json

class EventStreamCursor(TypedDict):
    sequence: EventSequence
    sessionId: str
    streamEpoch: EventStreamEpoch

class EventsSubscribeParams(TypedDict):
    afterCursor: NotRequired[EventStreamCursor | None]
    sessionId: str

EventSequence: TypeAlias = int

EventStreamEpoch: TypeAlias = str

__all__ = ['EventsSubscribeParams']
