# Generated from protocol/schema/v1. DO NOT EDIT.
# Run `python scripts/generate.py` from packages/python-client.
from __future__ import annotations

from typing import Any, Literal, Never, NotRequired, TypeAlias, TypedDict

# Source: protocol/schema/v1/wait-for-element-arguments.schema.json

class ElementSelector(TypedDict):
    context: NotRequired[UiContextSelector | None]
    css: NotRequired[str | None]
    identifier: NotRequired[str | None]
    name: NotRequired[str | None]
    role: NotRequired[str | None]
    text: NotRequired[TextMatch | None]
    value: NotRequired[str | None]

class TextMatch(TypedDict):
    caseSensitive: NotRequired[bool]
    mode: NotRequired[TextMatchMode]
    value: str

class UiContextSelector(TypedDict):
    contextId: NotRequired[str | None]
    contextKind: UiContextKind

class WaitForElementArguments(TypedDict):
    condition: NotRequired[WaitForElementCondition]
    selector: ElementSelector

TextMatchMode: TypeAlias = Literal['exact', 'contains']

UiContextKind: TypeAlias = Literal['native', 'web']

WaitForElementCondition: TypeAlias = Literal['present', 'visible', 'enabled', 'absent']

__all__ = ['WaitForElementArguments']
