# Generated from protocol/schema/v1. DO NOT EDIT.
# Run `python scripts/generate.py` from packages/python-client.
from __future__ import annotations

from typing import Any, Literal, Never, NotRequired, TypeAlias, TypedDict

# Source: protocol/schema/v1/recorded-action-call.schema.json

class RecordedActionCall(TypedDict):
    arguments: NotRequired[Any]
    argumentsRedacted: NotRequired[bool]
    id: str
    name: str

__all__ = ['RecordedActionCall']
