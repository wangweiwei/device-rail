# Generated from protocol/schema/v1. DO NOT EDIT.
# Run `python scripts/generate.py` from packages/python-client.
from __future__ import annotations

from typing import Any, Literal, Never, NotRequired, TypeAlias, TypedDict

# Source: protocol/schema/v1/events-stream-open-request.schema.json

class EventStreamOriginPolicyVariant1(TypedDict):
    kind: Literal['absent']

class EventStreamOriginPolicyVariant2(TypedDict):
    kind: Literal['exact']
    origin: str

class EventsStreamOpenParams(TypedDict):
    originPolicy: EventStreamOriginPolicy
    sessionId: str

class EventsStreamOpenRequest(TypedDict):
    id: RpcIdSchema
    jsonrpc: JsonRpcVersion
    method: EventsStreamOpenMethodSchema
    params: EventsStreamOpenParams

EventStreamOriginPolicy: TypeAlias = EventStreamOriginPolicyVariant1 | EventStreamOriginPolicyVariant2

EventsStreamOpenMethodSchema: TypeAlias = Literal['events.stream.open']

JsonRpcVersion: TypeAlias = Literal['2.0']

RpcIdSchema: TypeAlias = str | int

__all__ = ['EventsStreamOpenRequest']
