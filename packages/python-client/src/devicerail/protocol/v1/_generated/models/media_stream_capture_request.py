# Generated from protocol/schema/v1. DO NOT EDIT.
# Run `python scripts/generate.py` from packages/python-client.
from __future__ import annotations

from typing import Any, Literal, Never, NotRequired, TypeAlias, TypedDict

# Source: protocol/schema/v1/media-stream-capture-request.schema.json

class MediaStreamCaptureParams(TypedDict):
    durationMs: NotRequired[int | None]
    frameIndex: EventSequence
    streamId: str

class MediaStreamCaptureRequest(TypedDict):
    id: RpcIdSchema
    jsonrpc: JsonRpcVersion
    method: MediaStreamCaptureMethodSchema
    params: MediaStreamCaptureParams
    timeoutMs: NotRequired[RequestTimeoutMs]

EventSequence: TypeAlias = int

JsonRpcVersion: TypeAlias = Literal['2.0']

MediaStreamCaptureMethodSchema: TypeAlias = Literal['media.stream.capture']

RequestTimeoutMs: TypeAlias = int

RpcIdSchema: TypeAlias = str | int

__all__ = ['MediaStreamCaptureRequest']
