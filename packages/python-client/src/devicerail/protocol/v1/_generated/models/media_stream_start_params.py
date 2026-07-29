# Generated from protocol/schema/v1. DO NOT EDIT.
# Run `python scripts/generate.py` from packages/python-client.
from __future__ import annotations

from typing import Any, Literal, Never, NotRequired, TypeAlias, TypedDict

# Source: protocol/schema/v1/media-stream-start-params.schema.json

class MediaStreamStartParams(TypedDict):
    kind: MediaStreamKind
    streamId: str

MediaStreamKind: TypeAlias = Literal['screenshot', 'video']

__all__ = ['MediaStreamStartParams']
