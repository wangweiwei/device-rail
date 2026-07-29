# Generated from protocol/schema/v1. DO NOT EDIT.
# Run `python scripts/generate.py` from packages/python-client.
from __future__ import annotations

from typing import Any, Literal, Never, NotRequired, TypeAlias, TypedDict

# Source: protocol/schema/v1/sessions-list-result.schema.json

class SessionInfo(TypedDict):
    endedAtMs: NotRequired[int | None]
    eventCount: EventSequence
    id: str
    lastSequence: EventSequence
    startedAtMs: int
    state: SessionState

EventSequence: TypeAlias = int

SessionState: TypeAlias = Literal['active', 'ended']

SessionsListResult: TypeAlias = list[SessionInfo]

__all__ = ['SessionsListResult']
