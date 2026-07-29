# Generated from protocol/schema/v1. DO NOT EDIT.
# Run `python scripts/generate.py` from packages/python-client.
from __future__ import annotations

from typing import Any, Literal, Never, NotRequired, TypeAlias, TypedDict

# Source: protocol/schema/v1/session-export-request.schema.json

class SessionExportParams(TypedDict):
    afterSequence: NotRequired[EventSequence | None]
    limit: NotRequired[int | None]
    sessionId: NotRequired[str | None]

class SessionExportRequest(TypedDict):
    id: RpcIdSchema
    jsonrpc: JsonRpcVersion
    method: SessionExportMethodSchema
    params: NotRequired[SessionExportParams]

EventSequence: TypeAlias = int

JsonRpcVersion: TypeAlias = Literal['2.0']

RpcIdSchema: TypeAlias = str | int

SessionExportMethodSchema: TypeAlias = Literal['session.export']

__all__ = ['SessionExportRequest']
