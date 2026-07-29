# Generated from protocol/schema/v1. DO NOT EDIT.
# Run `python scripts/generate.py` from packages/python-client.
from __future__ import annotations

from typing import Any, Literal, Never, NotRequired, TypeAlias, TypedDict

# Source: protocol/schema/v1/events-subscribe-result.schema.json

class EventStreamCursor(TypedDict):
    sequence: EventSequence
    sessionId: str
    streamEpoch: EventStreamEpoch

class EventsSubscribeResult(TypedDict):
    replayThrough: EventStreamCursor
    sessionId: str
    sessionState: SessionState
    subscriptionId: str

EventSequence: TypeAlias = int

EventStreamEpoch: TypeAlias = str

SessionState: TypeAlias = Literal['active', 'ended']

__all__ = ['EventsSubscribeResult']
