# Generated from protocol/schema/v1. DO NOT EDIT.
# Run `python scripts/generate.py` from packages/python-client.
from __future__ import annotations

from typing import Any, Literal, Never, NotRequired, TypeAlias, TypedDict

# Source: protocol/schema/v1/manual-recording.schema.json

class ManualActionArgumentsVariant1(TypedDict):
    kind: Literal['captured']
    value: Any

class ManualActionArgumentsVariant2(TypedDict):
    kind: Literal['protected']
    secretRef: str

class ManualActionStep(TypedDict):
    arguments: ManualActionArguments
    callId: str
    capturedAtMs: int
    name: str
    sequence: EventSequence

class ManualRecording(TypedDict):
    actionSpaceSha256: str
    endedAtMs: int
    formatVersion: int
    recordingId: str
    sourceDeviceId: str
    startedAtMs: int
    steps: list[ManualActionStep]

EventSequence: TypeAlias = int

ManualActionArguments: TypeAlias = ManualActionArgumentsVariant1 | ManualActionArgumentsVariant2

__all__ = ['ManualRecording']
