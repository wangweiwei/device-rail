# Generated from protocol/schema/v1. DO NOT EDIT.
# Run `python scripts/generate.py` from packages/python-client.
from __future__ import annotations

from typing import Any, Literal, Never, NotRequired, TypeAlias, TypedDict

# Source: protocol/schema/v1/device-observe-response.schema.json

class AssetRef(TypedDict):
    id: str
    mediaType: str
    sha256: NotRequired[str | None]
    uri: str

class DeviceObserveSuccessSchema(TypedDict):
    id: RpcIdSchema
    jsonrpc: JsonRpcVersion
    result: Observation

class ErrorInfo(TypedDict):
    code: str
    details: NotRequired[Any]
    message: str
    retryable: bool

class Observation(TypedDict):
    capturedAtMs: int
    deviceId: str
    id: str
    metadata: NotRequired[dict[str, Any]]
    screenshot: NotRequired[AssetRef | None]
    screenshotOmission: NotRequired[ScreenshotOmissionReason | None]
    uiSnapshot: NotRequired[UiSnapshotRef | None]
    uiSnapshotOmission: NotRequired[UiSnapshotOmissionReason | None]
    viewport: Viewport

class RpcError(TypedDict):
    code: int
    data: ErrorInfo
    message: str

class SystemHelloFailureSchema(TypedDict):
    error: RpcError
    id: NullableRpcIdSchema
    jsonrpc: JsonRpcVersion

class UiContextRef(TypedDict):
    contextId: str
    contextKind: UiContextKind
    documentEpoch: str

class UiSnapshotRef(TypedDict):
    byteLength: int
    context: UiContextRef
    evidence: AssetRef
    formatVersion: int
    nodeCount: int

class Viewport(TypedDict):
    height: int
    scaleFactor: int | float
    width: int

JsonRpcVersion: TypeAlias = Literal['2.0']

RpcIdSchema: TypeAlias = str | int

ScreenshotOmissionReason: TypeAlias = Literal['policy', 'protectedAction']

UiContextKind: TypeAlias = Literal['native', 'web']

UiSnapshotOmissionReason: TypeAlias = Literal['driverUnsupported', 'policy', 'protectedAction']

NullableRpcIdSchema: TypeAlias = RpcIdSchema | None

DeviceObserveResponse: TypeAlias = DeviceObserveSuccessSchema | SystemHelloFailureSchema

__all__ = ['DeviceObserveResponse']
