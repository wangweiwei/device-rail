# Generated from protocol/schema/v1. DO NOT EDIT.
# Run `python scripts/generate.py` from packages/python-client.
from __future__ import annotations

from typing import Any, Literal, Never, NotRequired, TypeAlias, TypedDict

# Source: protocol/schema/v1/media-stream-capture-params.schema.json

class MediaStreamCaptureParams(TypedDict):
    durationMs: NotRequired[int | None]
    frameIndex: EventSequence
    streamId: str

EventSequence: TypeAlias = int

__all__ = ['MediaStreamCaptureParams']
