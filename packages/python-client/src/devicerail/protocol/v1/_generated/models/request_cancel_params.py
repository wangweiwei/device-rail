# Generated from protocol/schema/v1. DO NOT EDIT.
# Run `python scripts/generate.py` from packages/python-client.
from __future__ import annotations

from typing import Any, Literal, Never, NotRequired, TypeAlias, TypedDict

# Source: protocol/schema/v1/request-cancel-params.schema.json

class RequestCancelParams(TypedDict):
    requestId: RpcIdSchema

RpcIdSchema: TypeAlias = str | int

__all__ = ['RequestCancelParams']
