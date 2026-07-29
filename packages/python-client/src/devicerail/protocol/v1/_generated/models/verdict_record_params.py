# Generated from protocol/schema/v1. DO NOT EDIT.
# Run `python scripts/generate.py` from packages/python-client.
from __future__ import annotations

from typing import Any, Literal, Never, NotRequired, TypeAlias, TypedDict

# Source: protocol/schema/v1/verdict-record-params.schema.json

class AssetRef(TypedDict):
    id: str
    mediaType: str
    sha256: NotRequired[str | None]
    uri: str

class Verdict(TypedDict):
    evidence: NotRequired[list[AssetRef]]
    status: VerdictStatus
    summary: str

class VerdictRecordParams(TypedDict):
    verdict: Verdict

VerdictStatus: TypeAlias = Literal['pass', 'fail', 'unknown']

__all__ = ['VerdictRecordParams']
