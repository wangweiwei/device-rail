# Generated from protocol/schema/v1. DO NOT EDIT.
# Run `python scripts/generate.py` from packages/python-client.
from __future__ import annotations

from typing import Any, Literal, Never, NotRequired, TypeAlias, TypedDict

# Source: protocol/schema/v1/events-stream-terminal-params.schema.json

class ErrorInfo(TypedDict):
    code: str
    details: NotRequired[Any]
    message: str
    retryable: bool

class EventStreamCursor(TypedDict):
    sequence: EventSequence
    sessionId: str
    streamEpoch: EventStreamEpoch

class EventsStreamTerminalParams(TypedDict):
    lastEmittedCursor: NotRequired[EventStreamCursor | None]
    sessionId: str
    subscriptionId: str
    termination: EventsStreamTermination

class EventsStreamTerminationVariant1(TypedDict):
    reason: Literal['sessionEnded']

class EventsStreamTerminationVariant2(TypedDict):
    reason: Literal['cancelled']

class EventsStreamTerminationVariant3(TypedDict):
    error: ErrorInfo
    reason: Literal['slowConsumer']

class EventsStreamTerminationVariant4(TypedDict):
    error: ErrorInfo
    reason: Literal['sessionDeleted']

class EventsStreamTerminationVariant5(TypedDict):
    error: ErrorInfo
    reason: Literal['serverShutdown']

class EventsStreamTerminationVariant6(TypedDict):
    error: ErrorInfo
    reason: Literal['sequenceGap']

class EventsStreamTerminationVariant7(TypedDict):
    error: ErrorInfo
    reason: Literal['eventTooLarge']

class EventsStreamTerminationVariant8(TypedDict):
    error: ErrorInfo
    reason: Literal['internalError']

EventSequence: TypeAlias = int

EventStreamEpoch: TypeAlias = str

EventsStreamTermination: TypeAlias = EventsStreamTerminationVariant1 | EventsStreamTerminationVariant2 | EventsStreamTerminationVariant3 | EventsStreamTerminationVariant4 | EventsStreamTerminationVariant5 | EventsStreamTerminationVariant6 | EventsStreamTerminationVariant7 | EventsStreamTerminationVariant8

__all__ = ['EventsStreamTerminalParams']
