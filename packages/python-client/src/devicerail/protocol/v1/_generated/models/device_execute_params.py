# Generated from protocol/schema/v1. DO NOT EDIT.
# Run `python scripts/generate.py` from packages/python-client.
from __future__ import annotations

from typing import Any, Literal, Never, NotRequired, TypeAlias, TypedDict

# Source: protocol/schema/v1/device-execute-params.schema.json

class DeviceExecuteParams(TypedDict):
    actionTimeoutMs: NotRequired[RequestTimeoutMs]
    arguments: NotRequired[Any]
    id: str
    name: str

RequestTimeoutMs: TypeAlias = int

__all__ = ['DeviceExecuteParams']
