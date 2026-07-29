# Generated from protocol/schema/v1. DO NOT EDIT.
# Run `python scripts/generate.py` from packages/python-client.
from __future__ import annotations

from typing import Any, Literal, Never, NotRequired, TypeAlias, TypedDict

# Source: protocol/schema/v1/text-match.schema.json

class TextMatch(TypedDict):
    caseSensitive: NotRequired[bool]
    mode: NotRequired[TextMatchMode]
    value: str

TextMatchMode: TypeAlias = Literal['exact', 'contains']

__all__ = ['TextMatch']
