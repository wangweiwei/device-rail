# Generated from protocol/schema/v1. DO NOT EDIT.
# Run `python scripts/generate.py` from packages/python-client.
from __future__ import annotations

from typing import Any, Literal, Never, NotRequired, TypeAlias, TypedDict

# Source: protocol/schema/v1/session-end-request.schema.json

class SessionEndParams(TypedDict):
    outcome: NotRequired[SessionOutcome | None]
    reason: NotRequired[str | None]

class SessionEndRequest(TypedDict):
    id: RpcIdSchema
    jsonrpc: JsonRpcVersion
    method: SessionEndMethodSchema
    params: NotRequired[SessionEndParams]

JsonRpcVersion: TypeAlias = Literal['2.0']

RpcIdSchema: TypeAlias = str | int

SessionEndMethodSchema: TypeAlias = Literal['session.end']

SessionOutcome: TypeAlias = Literal['completed', 'failed', 'cancelled', 'shutdown']

__all__ = ['SessionEndRequest']
