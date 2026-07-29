# Generated from protocol/schema/v1. DO NOT EDIT.
# Run `python scripts/generate.py` from packages/python-client.
from __future__ import annotations

from typing import Any, Literal, Never, NotRequired, TypeAlias, TypedDict

# Source: protocol/schema/v1/verdict.schema.json

class AssetRef(TypedDict):
    id: str
    mediaType: str
    sha256: NotRequired[str | None]
    uri: str

class Verdict(TypedDict):
    evidence: NotRequired[list[AssetRef]]
    status: VerdictStatus
    summary: str

VerdictStatus: TypeAlias = Literal['pass', 'fail', 'unknown']

__all__ = ['Verdict']
