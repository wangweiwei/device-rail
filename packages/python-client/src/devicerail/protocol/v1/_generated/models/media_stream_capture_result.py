# Generated from protocol/schema/v1. DO NOT EDIT.
# Run `python scripts/generate.py` from packages/python-client.
from __future__ import annotations

from typing import Any, Literal, Never, NotRequired, TypeAlias, TypedDict

# Source: protocol/schema/v1/media-stream-capture-result.schema.json

class AssetRef(TypedDict):
    id: str
    mediaType: str
    sha256: NotRequired[str | None]
    uri: str

class MediaFrame(TypedDict):
    durationMs: NotRequired[int | None]
    evidence: AssetRef
    frameIndex: EventSequence
    keyFrame: NotRequired[bool]
    streamId: str

class MediaStreamCaptureResult(TypedDict):
    frame: MediaFrame

EventSequence: TypeAlias = int

__all__ = ['MediaStreamCaptureResult']
