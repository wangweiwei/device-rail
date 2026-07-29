# Generated from protocol/schema/v1. DO NOT EDIT.
# Run `python scripts/generate.py` from packages/python-client.
from __future__ import annotations

from typing import Any, Literal, Never, NotRequired, TypeAlias, TypedDict

# Source: protocol/schema/v1/ui-snapshot-ref.schema.json

class AssetRef(TypedDict):
    id: str
    mediaType: str
    sha256: NotRequired[str | None]
    uri: str

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

UiContextKind: TypeAlias = Literal['native', 'web']

__all__ = ['UiSnapshotRef']
