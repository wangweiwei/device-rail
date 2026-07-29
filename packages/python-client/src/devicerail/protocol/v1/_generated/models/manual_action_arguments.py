# Generated from protocol/schema/v1. DO NOT EDIT.
# Run `python scripts/generate.py` from packages/python-client.
from __future__ import annotations

from typing import Any, Literal, Never, NotRequired, TypeAlias, TypedDict

# Source: protocol/schema/v1/manual-action-arguments.schema.json

class ManualActionArgumentsVariant1(TypedDict):
    kind: Literal['captured']
    value: Any

class ManualActionArgumentsVariant2(TypedDict):
    kind: Literal['protected']
    secretRef: str

ManualActionArguments: TypeAlias = ManualActionArgumentsVariant1 | ManualActionArgumentsVariant2

__all__ = ['ManualActionArguments']
