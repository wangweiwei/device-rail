# Generated from protocol/schema/v1. DO NOT EDIT.
# Run `python scripts/generate.py` from packages/python-client.
from __future__ import annotations

from typing import Any, Literal, Never, NotRequired, TypeAlias, TypedDict

# Source: protocol/schema/v1/media-stream-end-result.schema.json

class MediaStreamEndResult(TypedDict):
    frameCount: int
    streamId: str

__all__ = ['MediaStreamEndResult']
