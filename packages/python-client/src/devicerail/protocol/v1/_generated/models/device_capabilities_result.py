# Generated from protocol/schema/v1. DO NOT EDIT.
# Run `python scripts/generate.py` from packages/python-client.
from __future__ import annotations

from typing import Any, Literal, Never, NotRequired, TypeAlias, TypedDict

# Source: protocol/schema/v1/device-capabilities-result.schema.json

class ActionDefinition(TypedDict):
    description: str
    inputSchema: Any
    name: str
    protection: NotRequired[ActionProtection]

ActionProtection: TypeAlias = Literal['standard', 'protected']

DeviceCapabilitiesResult: TypeAlias = list[ActionDefinition]

__all__ = ['DeviceCapabilitiesResult']
