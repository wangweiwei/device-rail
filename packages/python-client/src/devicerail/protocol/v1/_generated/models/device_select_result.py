# Generated from protocol/schema/v1. DO NOT EDIT.
# Run `python scripts/generate.py` from packages/python-client.
from __future__ import annotations

from typing import Any, Literal, Never, NotRequired, TypeAlias, TypedDict

# Source: protocol/schema/v1/device-select-result.schema.json

class DeviceInfo(TypedDict):
    connected: bool
    id: str
    name: str
    osVersion: NotRequired[str | None]
    platform: Platform

class DeviceSelectResult(TypedDict):
    device: DeviceInfo

class PlatformVariant1(TypedDict):
    kind: Literal['web']

class PlatformVariant10(TypedDict):
    kind: Literal['other']
    value: str

class PlatformVariant2(TypedDict):
    kind: Literal['android']

class PlatformVariant3(TypedDict):
    kind: Literal['ios']

class PlatformVariant4(TypedDict):
    kind: Literal['harmonyOs']

class PlatformVariant5(TypedDict):
    kind: Literal['macOs']

class PlatformVariant6(TypedDict):
    kind: Literal['windows']

class PlatformVariant7(TypedDict):
    kind: Literal['linux']

class PlatformVariant8(TypedDict):
    kind: Literal['rdp']

class PlatformVariant9(TypedDict):
    kind: Literal['mock']

Platform: TypeAlias = PlatformVariant1 | PlatformVariant2 | PlatformVariant3 | PlatformVariant4 | PlatformVariant5 | PlatformVariant6 | PlatformVariant7 | PlatformVariant8 | PlatformVariant9 | PlatformVariant10

__all__ = ['DeviceSelectResult']
