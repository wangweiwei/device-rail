# Generated from protocol/schema/v1. DO NOT EDIT.
# Run `python scripts/generate.py` from packages/python-client.
from __future__ import annotations

from typing import Any, Literal, Never, NotRequired, TypeAlias, TypedDict

# Source: protocol/schema/v1/media-stream-start-request.schema.json

class MediaStreamStartParams(TypedDict):
    kind: MediaStreamKind
    streamId: str

class MediaStreamStartRequest(TypedDict):
    id: RpcIdSchema
    jsonrpc: JsonRpcVersion
    method: MediaStreamStartMethodSchema
    params: MediaStreamStartParams

JsonRpcVersion: TypeAlias = Literal['2.0']

MediaStreamKind: TypeAlias = Literal['screenshot', 'video']

MediaStreamStartMethodSchema: TypeAlias = Literal['media.stream.start']

RpcIdSchema: TypeAlias = str | int

__all__ = ['MediaStreamStartRequest']
