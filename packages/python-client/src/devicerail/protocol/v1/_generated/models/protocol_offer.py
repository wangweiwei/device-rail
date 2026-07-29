# Generated from protocol/schema/v1. DO NOT EDIT.
# Run `python scripts/generate.py` from packages/python-client.
from __future__ import annotations

from typing import Any, Literal, Never, NotRequired, TypeAlias, TypedDict

# Source: protocol/schema/v1/protocol-offer.schema.json

class ProtocolOffer(TypedDict):
    ranges: list[ProtocolRange]

class ProtocolRange(TypedDict):
    major: int
    maxMinor: int
    minMinor: int

__all__ = ['ProtocolOffer']
