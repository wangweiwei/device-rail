# Generated from protocol/schema/v1. DO NOT EDIT.
# Run `python scripts/generate.py` from packages/python-client.
from __future__ import annotations

from typing import Any, Literal, Never, NotRequired, TypeAlias, TypedDict

# Source: protocol/schema/v1/ui-snapshot-get-response.schema.json

class ErrorInfo(TypedDict):
    code: str
    details: NotRequired[Any]
    message: str
    retryable: bool

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

class UiNode(TypedDict):
    bounds: NotRequired[UiRect | None]
    enabled: NotRequired[bool | None]
    hittable: NotRequired[bool | None]
    identifier: NotRequired[str | None]
    name: NotRequired[str | None]
    parentStableNodeId: NotRequired[str | None]
    role: str
    stableNodeId: str
    text: NotRequired[str | None]
    value: NotRequired[str | None]

class UiRect(TypedDict):
    height: int | float
    width: int | float
    x: int | float
    y: int | float

class UiSnapshot(TypedDict):
    context: UiContextRef
    formatVersion: int
    nodes: list[UiNode]
    observationId: str
    rootStableNodeIds: list[str]

class UiSnapshotGetSuccessSchema(TypedDict):
    id: RpcIdSchema
    jsonrpc: JsonRpcVersion
    result: UiSnapshot

JsonRpcVersion: TypeAlias = Literal['2.0']

RpcIdSchema: TypeAlias = str | int

UiContextKind: TypeAlias = Literal['native', 'web']

NullableRpcIdSchema: TypeAlias = RpcIdSchema | None

UiSnapshotGetResponse: TypeAlias = UiSnapshotGetSuccessSchema | SystemHelloFailureSchema

__all__ = ['UiSnapshotGetResponse']
