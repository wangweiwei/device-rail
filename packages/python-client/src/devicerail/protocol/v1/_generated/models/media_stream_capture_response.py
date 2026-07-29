# Generated from protocol/schema/v1. DO NOT EDIT.
# Run `python scripts/generate.py` from packages/python-client.
from __future__ import annotations

from typing import Any, Literal, Never, NotRequired, TypeAlias, TypedDict

# Source: protocol/schema/v1/media-stream-capture-response.schema.json

class AssetRef(TypedDict):
    id: str
    mediaType: str
    sha256: NotRequired[str | None]
    uri: str

class ErrorInfo(TypedDict):
    code: str
    details: NotRequired[Any]
    message: str
    retryable: bool

class MediaFrame(TypedDict):
    durationMs: NotRequired[int | None]
    evidence: AssetRef
    frameIndex: EventSequence
    keyFrame: NotRequired[bool]
    streamId: str

class MediaStreamCaptureResult(TypedDict):
    frame: MediaFrame

class MediaStreamCaptureSuccessSchema(TypedDict):
    id: RpcIdSchema
    jsonrpc: JsonRpcVersion
    result: MediaStreamCaptureResult

class RpcError(TypedDict):
    code: int
    data: ErrorInfo
    message: str

class SystemHelloFailureSchema(TypedDict):
    error: RpcError
    id: NullableRpcIdSchema
    jsonrpc: JsonRpcVersion

EventSequence: TypeAlias = int

JsonRpcVersion: TypeAlias = Literal['2.0']

RpcIdSchema: TypeAlias = str | int

NullableRpcIdSchema: TypeAlias = RpcIdSchema | None

MediaStreamCaptureResponse: TypeAlias = MediaStreamCaptureSuccessSchema | SystemHelloFailureSchema

__all__ = ['MediaStreamCaptureResponse']
