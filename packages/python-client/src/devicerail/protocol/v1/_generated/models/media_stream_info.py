# Generated from protocol/schema/v1. DO NOT EDIT.
# Run `python scripts/generate.py` from packages/python-client.
from __future__ import annotations

from typing import Any, Literal, Never, NotRequired, TypeAlias, TypedDict

# Source: protocol/schema/v1/media-stream-info.schema.json

class MediaStreamInfo(TypedDict):
    id: str
    kind: MediaStreamKind
    mediaType: str
    viewport: NotRequired[Viewport | None]

class Viewport(TypedDict):
    height: int
    scaleFactor: int | float
    width: int

MediaStreamKind: TypeAlias = Literal['screenshot', 'video']

__all__ = ['MediaStreamInfo']
