# Generated from protocol/schema/v1. DO NOT EDIT.
# Run `python scripts/generate.py` from packages/python-client.
from __future__ import annotations

from typing import Any, Literal, Never, NotRequired, TypeAlias, TypedDict

# Source: protocol/schema/v1/system-describe-result.schema.json

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

class SystemDescribeResult(TypedDict):
    activeSessionId: NotRequired[str | None]
    client: PeerInfo
    connection: HelloResult
    deviceId: NotRequired[str | None]

class TransportInfo(TypedDict):
    framing: str
    kind: str

__all__ = ['SystemDescribeResult']
