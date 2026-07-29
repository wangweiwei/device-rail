# Generated from protocol/schema/v1. DO NOT EDIT.
# Run `python scripts/generate.py` from packages/python-client.
from __future__ import annotations

from typing import Any, Literal, Never, NotRequired, TypeAlias, TypedDict

# Source: protocol/schema/v1/protocol-selection.schema.json

class ProtocolSelection(TypedDict):
    selected: ProtocolVersion

class ProtocolVersion(TypedDict):
    major: int
    minor: int

__all__ = ['ProtocolSelection']
