# Generated from protocol/schema/v1. DO NOT EDIT.
# Run `python scripts/generate.py` from packages/python-client.
from __future__ import annotations

from typing import Any, Literal, Never, NotRequired, TypeAlias, TypedDict

# Source: protocol/schema/v1/tap-element-result.schema.json

class TapElementResult(TypedDict):
    element: UiNodeRef

class UiContextRef(TypedDict):
    contextId: str
    contextKind: UiContextKind
    documentEpoch: str

class UiNodeRef(TypedDict):
    context: UiContextRef
    observationId: str
    stableNodeId: str

UiContextKind: TypeAlias = Literal['native', 'web']

__all__ = ['TapElementResult']
