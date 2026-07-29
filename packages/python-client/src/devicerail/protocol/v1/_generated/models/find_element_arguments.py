# Generated from protocol/schema/v1. DO NOT EDIT.
# Run `python scripts/generate.py` from packages/python-client.
from __future__ import annotations

from typing import Any, Literal, Never, NotRequired, TypeAlias, TypedDict

# Source: protocol/schema/v1/find-element-arguments.schema.json

class ElementSelector(TypedDict):
    context: NotRequired[UiContextSelector | None]
    css: NotRequired[str | None]
    identifier: NotRequired[str | None]
    name: NotRequired[str | None]
    role: NotRequired[str | None]
    text: NotRequired[TextMatch | None]
    value: NotRequired[str | None]

class FindElementArguments(TypedDict):
    selector: ElementSelector

class TextMatch(TypedDict):
    caseSensitive: NotRequired[bool]
    mode: NotRequired[TextMatchMode]
    value: str

class UiContextSelector(TypedDict):
    contextId: NotRequired[str | None]
    contextKind: UiContextKind

TextMatchMode: TypeAlias = Literal['exact', 'contains']

UiContextKind: TypeAlias = Literal['native', 'web']

__all__ = ['FindElementArguments']
