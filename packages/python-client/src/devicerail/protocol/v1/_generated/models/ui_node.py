# Generated from protocol/schema/v1. DO NOT EDIT.
# Run `python scripts/generate.py` from packages/python-client.
from __future__ import annotations

from typing import Any, Literal, Never, NotRequired, TypeAlias, TypedDict

# Source: protocol/schema/v1/ui-node.schema.json

class UiNode(TypedDict):
    bounds: NotRequired[UiRect | None]
    enabled: NotRequired[bool | None]
    hittable: NotRequired[bool | None]
    identifier: NotRequired[str | None]
    name: NotRequired[str | None]
    parentStableNodeId: NotRequired[str | None]
    role: str
    stableNodeId: str
    text: NotRequired[str | None]
    value: NotRequired[str | None]

class UiRect(TypedDict):
    height: int | float
    width: int | float
    x: int | float
    y: int | float

__all__ = ['UiNode']
