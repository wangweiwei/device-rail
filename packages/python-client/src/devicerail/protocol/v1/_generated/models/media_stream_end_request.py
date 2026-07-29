# Generated from protocol/schema/v1. DO NOT EDIT.
# Run `python scripts/generate.py` from packages/python-client.
from __future__ import annotations

from typing import Any, Literal, Never, NotRequired, TypeAlias, TypedDict

# Source: protocol/schema/v1/media-stream-end-request.schema.json

class MediaStreamEndParams(TypedDict):
    streamId: str

class MediaStreamEndRequest(TypedDict):
    id: RpcIdSchema
    jsonrpc: JsonRpcVersion
    method: MediaStreamEndMethodSchema
    params: MediaStreamEndParams

JsonRpcVersion: TypeAlias = Literal['2.0']

MediaStreamEndMethodSchema: TypeAlias = Literal['media.stream.end']

RpcIdSchema: TypeAlias = str | int

__all__ = ['MediaStreamEndRequest']
