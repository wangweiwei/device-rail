# Generated from protocol/schema/v1. DO NOT EDIT.
# Run `python scripts/generate.py` from packages/python-client.
from __future__ import annotations

from typing import Any, Literal, Never, NotRequired, TypeAlias, TypedDict

# Source: protocol/schema/v1/system-describe-response.schema.json

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

class SystemDescribeResult(TypedDict):
    activeSessionId: NotRequired[str | None]
    client: PeerInfo
    connection: HelloResult
    deviceId: NotRequired[str | None]

class SystemDescribeSuccessSchema(TypedDict):
    id: RpcIdSchema
    jsonrpc: JsonRpcVersion
    result: SystemDescribeResult

class SystemHelloFailureSchema(TypedDict):
    error: RpcError
    id: NullableRpcIdSchema
    jsonrpc: JsonRpcVersion

class TransportInfo(TypedDict):
    framing: str
    kind: str

JsonRpcVersion: TypeAlias = Literal['2.0']

RpcIdSchema: TypeAlias = str | int

NullableRpcIdSchema: TypeAlias = RpcIdSchema | None

SystemDescribeResponse: TypeAlias = SystemDescribeSuccessSchema | SystemHelloFailureSchema

__all__ = ['SystemDescribeResponse']
