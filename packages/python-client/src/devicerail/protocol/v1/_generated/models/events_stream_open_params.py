# Generated from protocol/schema/v1. DO NOT EDIT.
# Run `python scripts/generate.py` from packages/python-client.
from __future__ import annotations

from typing import Any, Literal, Never, NotRequired, TypeAlias, TypedDict

# Source: protocol/schema/v1/events-stream-open-params.schema.json

class EventStreamOriginPolicyVariant1(TypedDict):
    kind: Literal['absent']

class EventStreamOriginPolicyVariant2(TypedDict):
    kind: Literal['exact']
    origin: str

class EventsStreamOpenParams(TypedDict):
    originPolicy: EventStreamOriginPolicy
    sessionId: str

EventStreamOriginPolicy: TypeAlias = EventStreamOriginPolicyVariant1 | EventStreamOriginPolicyVariant2

__all__ = ['EventsStreamOpenParams']
