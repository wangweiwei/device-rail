# Generated from protocol/schema/v1. DO NOT EDIT.
# Run `python scripts/generate.py` from packages/python-client.
from __future__ import annotations

from typing import Any, Literal, Never, NotRequired, TypeAlias, TypedDict

# Source: protocol/schema/v1/media-stream-start-response.schema.json

class ErrorInfo(TypedDict):
    code: str
    details: NotRequired[Any]
    message: str
    retryable: bool

class MediaStreamInfo(TypedDict):
    id: str
    kind: MediaStreamKind
    mediaType: str
    viewport: NotRequired[Viewport | None]

class MediaStreamStartResult(TypedDict):
    stream: MediaStreamInfo

class MediaStreamStartSuccessSchema(TypedDict):
    id: RpcIdSchema
    jsonrpc: JsonRpcVersion
    result: MediaStreamStartResult

class RpcError(TypedDict):
    code: int
    data: ErrorInfo
    message: str

class SystemHelloFailureSchema(TypedDict):
    error: RpcError
    id: NullableRpcIdSchema
    jsonrpc: JsonRpcVersion

class Viewport(TypedDict):
    height: int
    scaleFactor: int | float
    width: int

JsonRpcVersion: TypeAlias = Literal['2.0']

MediaStreamKind: TypeAlias = Literal['screenshot', 'video']

RpcIdSchema: TypeAlias = str | int

NullableRpcIdSchema: TypeAlias = RpcIdSchema | None

MediaStreamStartResponse: TypeAlias = MediaStreamStartSuccessSchema | SystemHelloFailureSchema

__all__ = ['MediaStreamStartResponse']
