# Generated from protocol/schema/v1. DO NOT EDIT.
# Run `python scripts/generate.py` from packages/python-client.
from __future__ import annotations

from typing import Any, Literal, Never, NotRequired, TypeAlias, TypedDict

# Source: protocol/schema/v1/wait-for-element-result.schema.json

class UiContextRef(TypedDict):
    contextId: str
    contextKind: UiContextKind
    documentEpoch: str

class UiNodeRef(TypedDict):
    context: UiContextRef
    observationId: str
    stableNodeId: str

class WaitForElementResult(TypedDict):
    condition: WaitForElementCondition
    element: NotRequired[UiNodeRef | None]
    matched: bool

UiContextKind: TypeAlias = Literal['native', 'web']

WaitForElementCondition: TypeAlias = Literal['present', 'visible', 'enabled', 'absent']

__all__ = ['WaitForElementResult']
