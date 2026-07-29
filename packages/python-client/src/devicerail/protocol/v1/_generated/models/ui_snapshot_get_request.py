# Generated from protocol/schema/v1. DO NOT EDIT.
# Run `python scripts/generate.py` from packages/python-client.
from __future__ import annotations

from typing import Any, Literal, Never, NotRequired, TypeAlias, TypedDict

# Source: protocol/schema/v1/ui-snapshot-get-request.schema.json

class UiSnapshotGetParams(TypedDict):
    observationId: str

class UiSnapshotGetRequest(TypedDict):
    id: RpcIdSchema
    jsonrpc: JsonRpcVersion
    method: UiSnapshotGetMethodSchema
    params: UiSnapshotGetParams

JsonRpcVersion: TypeAlias = Literal['2.0']

RpcIdSchema: TypeAlias = str | int

UiSnapshotGetMethodSchema: TypeAlias = Literal['ui.snapshot.get']

__all__ = ['UiSnapshotGetRequest']
