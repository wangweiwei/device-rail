# Generated from protocol/schema/v1. DO NOT EDIT.
# Run `python scripts/generate.py` from packages/python-client.
from __future__ import annotations

from typing import Any, Literal, Never, NotRequired, TypeAlias, TypedDict

# Source: protocol/schema/v1/error-info.schema.json

class ErrorInfo(TypedDict):
    code: str
    details: NotRequired[Any]
    message: str
    retryable: bool

__all__ = ['ErrorInfo']
