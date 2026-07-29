# Generated from protocol/schema/v1. DO NOT EDIT.
# Run `python scripts/generate.py` from packages/python-client.
from __future__ import annotations

from typing import Any, Literal, Never, NotRequired, TypeAlias, TypedDict

# Source: protocol/schema/v1/action-execution.schema.json

class ActionExecutionVariant1(TypedDict):
    context: UiContextRef
    mode: Literal['nativeSemantic']

class ActionExecutionVariant2(TypedDict):
    context: UiContextRef
    mode: Literal['webSemantic']

class ActionExecutionVariant3(TypedDict):
    context: UiContextRef
    fallbackReason: CoordinateFallbackReason
    mode: Literal['coordinateFallback']

class UiContextRef(TypedDict):
    contextId: str
    contextKind: UiContextKind
    documentEpoch: str

CoordinateFallbackReason: TypeAlias = Literal['semanticInteractionUnavailable', 'platformLimitation']

UiContextKind: TypeAlias = Literal['native', 'web']

ActionExecution: TypeAlias = ActionExecutionVariant1 | ActionExecutionVariant2 | ActionExecutionVariant3

__all__ = ['ActionExecution']
