# Generated from protocol/schema/v1. DO NOT EDIT.
# Run `python scripts/generate.py` from packages/python-client.
from __future__ import annotations

from typing import Any, Literal, Never, NotRequired, TypeAlias, TypedDict

# Source: protocol/schema/v1/tap-element-arguments.schema.json

class ElementSelector(TypedDict):
    context: NotRequired[UiContextSelector | None]
    css: NotRequired[str | None]
    identifier: NotRequired[str | None]
    name: NotRequired[str | None]
    role: NotRequired[str | None]
    text: NotRequired[TextMatch | None]
    value: NotRequired[str | None]

class ElementTargetVariant1(TypedDict):
    kind: Literal['selector']
    selector: ElementSelector

class ElementTargetVariant2(TypedDict):
    kind: Literal['node']
    node: UiNodeRef

class TapElementArguments(TypedDict):
    target: ElementTarget

class TextMatch(TypedDict):
    caseSensitive: NotRequired[bool]
    mode: NotRequired[TextMatchMode]
    value: str

class UiContextRef(TypedDict):
    contextId: str
    contextKind: UiContextKind
    documentEpoch: str

class UiContextSelector(TypedDict):
    contextId: NotRequired[str | None]
    contextKind: UiContextKind

class UiNodeRef(TypedDict):
    context: UiContextRef
    observationId: str
    stableNodeId: str

ElementTarget: TypeAlias = ElementTargetVariant1 | ElementTargetVariant2

TextMatchMode: TypeAlias = Literal['exact', 'contains']

UiContextKind: TypeAlias = Literal['native', 'web']

__all__ = ['TapElementArguments']
