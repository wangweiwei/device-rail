# Generated from protocol/schema/v1. DO NOT EDIT.
# Run `python scripts/generate.py` from packages/python-client.
from __future__ import annotations

from typing import Any, Literal, Never, NotRequired, TypeAlias, TypedDict

# Source: protocol/schema/v1/events-stream-open-result.schema.json

class EventsStreamOpenResult(TypedDict):
    endpoint: EventStreamEndpoint
    expiresAtMs: int
    streamEpoch: EventStreamEpoch

EventStreamEndpoint: TypeAlias = str

EventStreamEpoch: TypeAlias = str

__all__ = ['EventsStreamOpenResult']
