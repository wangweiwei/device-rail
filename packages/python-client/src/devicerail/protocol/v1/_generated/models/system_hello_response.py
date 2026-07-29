# Generated from protocol/schema/v1. DO NOT EDIT.
# Run `python scripts/generate.py` from packages/python-client.
from __future__ import annotations

from typing import Any, Literal, Never, NotRequired, TypeAlias, TypedDict

# Source: protocol/schema/v1/system-hello-response.schema.json

class ErrorInfo(TypedDict):
    code: str
    details: NotRequired[Any]
    message: str
    retryable: bool

class FeatureSelection(TypedDict):
    enabled: list[str]

class HelloResult(TypedDict):
    connectionId: str
    features: FeatureSelection
    protocol: ProtocolSelection
    server: PeerInfo
    transport: TransportInfo

class PeerInfo(TypedDict):
    name: str
    version: str

class ProtocolSelection(TypedDict):
    selected: ProtocolVersion

class ProtocolVersion(TypedDict):
    major: int
    minor: int

class RpcError(TypedDict):
    code: int
    data: ErrorInfo
    message: str

class SystemHelloFailureSchema(TypedDict):
    error: RpcError
    id: NullableRpcIdSchema
    jsonrpc: JsonRpcVersion

class SystemHelloSuccessSchema(TypedDict):
    id: RpcIdSchema
    jsonrpc: JsonRpcVersion
    result: HelloResult

class TransportInfo(TypedDict):
    framing: str
    kind: str

JsonRpcVersion: TypeAlias = Literal['2.0']

RpcIdSchema: TypeAlias = str | int

NullableRpcIdSchema: TypeAlias = RpcIdSchema | None

SystemHelloResponse: TypeAlias = SystemHelloSuccessSchema | SystemHelloFailureSchema

__all__ = ['SystemHelloResponse']
