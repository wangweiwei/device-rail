# Generated from protocol/schema/v1. DO NOT EDIT.
# Run `python scripts/generate.py` from packages/python-client.
from __future__ import annotations

from typing import Any, Literal, Never, NotRequired, TypeAlias, TypedDict

# Source: protocol/schema/v1/verdict-record-request.schema.json

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

class VerdictRecordRequest(TypedDict):
    id: RpcIdSchema
    jsonrpc: JsonRpcVersion
    method: VerdictRecordMethodSchema
    params: VerdictRecordParams

JsonRpcVersion: TypeAlias = Literal['2.0']

RpcIdSchema: TypeAlias = str | int

VerdictRecordMethodSchema: TypeAlias = Literal['verdict.record']

VerdictStatus: TypeAlias = Literal['pass', 'fail', 'unknown']

__all__ = ['VerdictRecordRequest']
