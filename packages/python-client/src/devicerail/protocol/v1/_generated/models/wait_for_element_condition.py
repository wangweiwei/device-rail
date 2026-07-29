# Generated from protocol/schema/v1. DO NOT EDIT.
# Run `python scripts/generate.py` from packages/python-client.
from __future__ import annotations

from typing import Any, Literal, Never, NotRequired, TypeAlias, TypedDict

# Source: protocol/schema/v1/wait-for-element-condition.schema.json

WaitForElementCondition: TypeAlias = Literal['present', 'visible', 'enabled', 'absent']

__all__ = ['WaitForElementCondition']
