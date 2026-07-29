# Generated from protocol/schema/v1. DO NOT EDIT.
# Run `python scripts/generate.py` from packages/python-client.
from __future__ import annotations

from typing import Any, Literal, Never, NotRequired, TypeAlias, TypedDict

# Source: protocol/schema/v1/action-outcome.schema.json

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

class ActionOutcomeVariant1(TypedDict):
    outcome: Literal['succeeded']
    result: ActionResult

class ActionOutcomeVariant2(TypedDict):
    error: ErrorInfo
    outcome: Literal['failed']

class ActionOutcomeVariant3(TypedDict):
    error: ErrorInfo
    outcome: Literal['cancelled']

class ActionOutcomeVariant4(TypedDict):
    error: ErrorInfo
    outcome: Literal['timedOut']
    timeoutMs: int

class ActionResult(TypedDict):
    after: NotRequired[Observation | None]
    before: NotRequired[Observation | None]
    callId: str
    evidence: NotRequired[list[AssetRef]]
    execution: NotRequired[ActionExecution | None]
    finishedAtMs: int
    output: Any
    startedAtMs: int

class AssetRef(TypedDict):
    id: str
    mediaType: str
    sha256: NotRequired[str | None]
    uri: str

class ErrorInfo(TypedDict):
    code: str
    details: NotRequired[Any]
    message: str
    retryable: bool

class Observation(TypedDict):
    capturedAtMs: int
    deviceId: str
    id: str
    metadata: NotRequired[dict[str, Any]]
    screenshot: NotRequired[AssetRef | None]
    screenshotOmission: NotRequired[ScreenshotOmissionReason | None]
    uiSnapshot: NotRequired[UiSnapshotRef | None]
    uiSnapshotOmission: NotRequired[UiSnapshotOmissionReason | None]
    viewport: Viewport

class UiContextRef(TypedDict):
    contextId: str
    contextKind: UiContextKind
    documentEpoch: str

class UiSnapshotRef(TypedDict):
    byteLength: int
    context: UiContextRef
    evidence: AssetRef
    formatVersion: int
    nodeCount: int

class Viewport(TypedDict):
    height: int
    scaleFactor: int | float
    width: int

CoordinateFallbackReason: TypeAlias = Literal['semanticInteractionUnavailable', 'platformLimitation']

ScreenshotOmissionReason: TypeAlias = Literal['policy', 'protectedAction']

UiContextKind: TypeAlias = Literal['native', 'web']

UiSnapshotOmissionReason: TypeAlias = Literal['driverUnsupported', 'policy', 'protectedAction']

ActionExecution: TypeAlias = ActionExecutionVariant1 | ActionExecutionVariant2 | ActionExecutionVariant3

ActionOutcome: TypeAlias = ActionOutcomeVariant1 | ActionOutcomeVariant2 | ActionOutcomeVariant3 | ActionOutcomeVariant4

__all__ = ['ActionOutcome']
