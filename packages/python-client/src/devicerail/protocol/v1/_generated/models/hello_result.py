# Generated from protocol/schema/v1. DO NOT EDIT.
# Run `python scripts/generate.py` from packages/python-client.
from __future__ import annotations

from typing import Any, Literal, Never, NotRequired, TypeAlias, TypedDict

# Source: protocol/schema/v1/hello-result.schema.json

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

class TransportInfo(TypedDict):
    framing: str
    kind: str

__all__ = ['HelloResult']
