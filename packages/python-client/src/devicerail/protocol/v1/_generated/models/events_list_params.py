# Generated from protocol/schema/v1. DO NOT EDIT.
# Run `python scripts/generate.py` from packages/python-client.
from __future__ import annotations

from typing import Any, Literal, Never, NotRequired, TypeAlias, TypedDict

# Source: protocol/schema/v1/events-list-params.schema.json

class EventsListParams(TypedDict):
    afterSequence: NotRequired[EventSequence | None]
    limit: NotRequired[int | None]
    sessionId: NotRequired[str | None]

EventSequence: TypeAlias = int

__all__ = ['EventsListParams']
