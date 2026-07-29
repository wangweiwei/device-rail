# Generated from protocol/schema/v1. DO NOT EDIT.
# Run `python scripts/generate.py` from packages/python-client.
from __future__ import annotations

from typing import Any, Literal, Never, NotRequired, TypeAlias, TypedDict

# Source: protocol/schema/v1/events-stream-event-params.schema.json

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

class EventStreamCursor(TypedDict):
    sequence: EventSequence
    sessionId: str
    streamEpoch: EventStreamEpoch

class EventsStreamEventParams(TypedDict):
    cursor: EventStreamCursor
    event: TestEvent
    subscriptionId: str

class MediaFrame(TypedDict):
    durationMs: NotRequired[int | None]
    evidence: AssetRef
    frameIndex: EventSequence
    keyFrame: NotRequired[bool]
    streamId: str

class MediaStreamInfo(TypedDict):
    id: str
    kind: MediaStreamKind
    mediaType: str
    viewport: NotRequired[Viewport | None]

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

class RecordedActionCall(TypedDict):
    arguments: NotRequired[Any]
    argumentsRedacted: NotRequired[bool]
    id: str
    name: str

class TestEvent(TypedDict):
    atMs: int
    deviceId: NotRequired[str | None]
    eventId: str
    payload: TestEventPayload
    requestId: NotRequired[RpcIdSchema | None]
    sequence: EventSequence
    sessionId: str

class TestEventPayloadVariant1(TypedDict):
    type: Literal['sessionStarted']

class TestEventPayloadVariant10(TypedDict):
    error: ErrorInfo
    type: Literal['error']

class TestEventPayloadVariant2(TypedDict):
    outcome: SessionOutcome
    reason: NotRequired[str | None]
    type: Literal['sessionEnded']

class TestEventPayloadVariant3(TypedDict):
    observation: Observation
    type: Literal['observationCaptured']

class TestEventPayloadVariant4(TypedDict):
    call: RecordedActionCall
    type: Literal['actionStarted']

class TestEventPayloadVariant5(TypedDict):
    callId: str
    outcome: ActionOutcome
    type: Literal['actionCompleted']

class TestEventPayloadVariant6(TypedDict):
    stream: MediaStreamInfo
    type: Literal['mediaStreamStarted']

class TestEventPayloadVariant7(TypedDict):
    frame: MediaFrame
    type: Literal['mediaFrameCaptured']

class TestEventPayloadVariant8(TypedDict):
    frameCount: int
    streamId: str
    type: Literal['mediaStreamEnded']

class TestEventPayloadVariant9(TypedDict):
    type: Literal['verdictRecorded']
    verdict: Verdict

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

class Verdict(TypedDict):
    evidence: NotRequired[list[AssetRef]]
    status: VerdictStatus
    summary: str

class Viewport(TypedDict):
    height: int
    scaleFactor: int | float
    width: int

ActionOutcome: TypeAlias = ActionOutcomeVariant1 | ActionOutcomeVariant2 | ActionOutcomeVariant3 | ActionOutcomeVariant4

CoordinateFallbackReason: TypeAlias = Literal['semanticInteractionUnavailable', 'platformLimitation']

EventSequence: TypeAlias = int

EventStreamEpoch: TypeAlias = str

MediaStreamKind: TypeAlias = Literal['screenshot', 'video']

RpcIdSchema: TypeAlias = str | int

ScreenshotOmissionReason: TypeAlias = Literal['policy', 'protectedAction']

SessionOutcome: TypeAlias = Literal['completed', 'failed', 'cancelled', 'shutdown']

UiContextKind: TypeAlias = Literal['native', 'web']

UiSnapshotOmissionReason: TypeAlias = Literal['driverUnsupported', 'policy', 'protectedAction']

VerdictStatus: TypeAlias = Literal['pass', 'fail', 'unknown']

ActionExecution: TypeAlias = ActionExecutionVariant1 | ActionExecutionVariant2 | ActionExecutionVariant3

TestEventPayload: TypeAlias = TestEventPayloadVariant1 | TestEventPayloadVariant2 | TestEventPayloadVariant3 | TestEventPayloadVariant4 | TestEventPayloadVariant5 | TestEventPayloadVariant6 | TestEventPayloadVariant7 | TestEventPayloadVariant8 | TestEventPayloadVariant9 | TestEventPayloadVariant10

__all__ = ['EventsStreamEventParams']
