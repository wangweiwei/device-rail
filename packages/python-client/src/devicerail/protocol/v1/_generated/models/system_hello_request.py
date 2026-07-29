# Generated from protocol/schema/v1. DO NOT EDIT.
# Run `python scripts/generate.py` from packages/python-client.
from __future__ import annotations

from typing import Any, Literal, Never, NotRequired, TypeAlias, TypedDict

# Source: protocol/schema/v1/system-hello-request.schema.json

class FeatureOffer(TypedDict):
    optional: NotRequired[list[str]]
    required: NotRequired[list[str]]

class HelloParams(TypedDict):
    client: PeerInfo
    features: NotRequired[FeatureOffer]
    protocol: ProtocolOffer

class PeerInfo(TypedDict):
    name: str
    version: str

class ProtocolOffer(TypedDict):
    ranges: list[ProtocolRange]

class ProtocolRange(TypedDict):
    major: int
    maxMinor: int
    minMinor: int

class SystemHelloRequest(TypedDict):
    id: RpcIdSchema
    jsonrpc: JsonRpcVersion
    method: SystemHelloMethodSchema
    params: HelloParams

JsonRpcVersion: TypeAlias = Literal['2.0']

RpcIdSchema: TypeAlias = str | int

SystemHelloMethodSchema: TypeAlias = Literal['system.hello']

__all__ = ['SystemHelloRequest']
