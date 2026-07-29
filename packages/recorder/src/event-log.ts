import type { ProtocolVersion, TestEvent } from "@devicerail/protocol";

import { RecorderError } from "./errors.js";
import type {
  EventAcceptance,
  EventBatchResult,
  EventLogSnapshot,
} from "./types.js";

const MAX_SAFE_INTEGER = Number.MAX_SAFE_INTEGER;
const MAX_JSON_DEPTH = 128;
const MAX_JSON_NODES = 1_000_000;
const MAX_VERDICT_SUMMARY_LENGTH = 16_384;
const MAX_VERDICT_EVIDENCE_REFERENCES = 64;
const UINT32_MAX = 4_294_967_295;
const UI_SNAPSHOT_MEDIA_TYPE = "application/vnd.devicerail.ui-tree+json;version=1";
const PREPARED_STATE = Symbol("prepared event-log state");
const UUID_PATTERN =
  /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/u;
const ABSENT = Symbol("absent correlation");

type JsonRecord = Record<string, unknown>;
type CorrelationValue = string | number | null | typeof ABSENT;

interface OpenAction {
  readonly deviceId: CorrelationValue;
  readonly requestId: CorrelationValue;
}

interface OpenMediaStream {
  readonly mediaType: string;
  readonly nextFrameIndex: number;
}

interface MutableLogState {
  readonly owner: object;
  eventCount: number;
  readonly appendedEvents: TestEvent[];
  readonly eventIds: OverlaySet;
  openActions: Map<string, OpenAction>;
  openMediaStreams: Map<string, OpenMediaStream>;
  readonly seenCallIds: OverlaySet;
  readonly seenMediaStreamIds: OverlaySet;
  terminal: boolean;
}

class OverlaySet {
  readonly #base: ReadonlySet<string>;
  readonly added = new Set<string>();

  constructor(base: ReadonlySet<string>) {
    this.#base = base;
  }

  has(value: string): boolean {
    return this.added.has(value) || this.#base.has(value);
  }

  add(value: string): void {
    this.added.add(value);
  }
}

/** Opaque, generation-bound result of validating one delivery batch. */
export interface PreparedEventBatch {
  readonly baseGeneration: number;
  readonly acceptedEvents: readonly TestEvent[];
  readonly result: EventBatchResult;
  readonly [PREPARED_STATE]: MutableLogState;
}

interface CloneContext {
  nodes: number;
  readonly seen: WeakSet<object>;
}

function invalidEvent(location: string, message: string): never {
  throw new RecorderError("invalid_event", `${location}: ${message}`, {
    details: { location },
  });
}

function recordAt(value: unknown, location: string): JsonRecord {
  if (value === null || typeof value !== "object" || Array.isArray(value)) {
    return invalidEvent(location, "must be an object");
  }
  const prototype = Object.getPrototypeOf(value) as unknown;
  if (prototype !== Object.prototype && prototype !== null) {
    return invalidEvent(location, "must be a plain JSON object");
  }
  for (const key of Reflect.ownKeys(value)) {
    if (typeof key !== "string") {
      return invalidEvent(location, "must not contain symbol keys");
    }
    const descriptor = Object.getOwnPropertyDescriptor(value, key);
    if (!descriptor?.enumerable || !("value" in descriptor)) {
      return invalidEvent(location, `field ${key} must be an enumerable data property`);
    }
  }
  return value as JsonRecord;
}

function exactKeys(
  value: JsonRecord,
  required: readonly string[],
  optional: readonly string[],
  location: string,
): void {
  const allowed = new Set([...required, ...optional]);
  for (const key of required) {
    if (!Object.hasOwn(value, key)) {
      invalidEvent(location, `is missing required field ${key}`);
    }
  }
  for (const key of Object.keys(value)) {
    if (!allowed.has(key)) {
      invalidEvent(location, `contains unknown field ${key}`);
    }
  }
}

function stringAt(value: unknown, location: string): string {
  if (typeof value !== "string") {
    return invalidEvent(location, "must be a string");
  }
  return value;
}

function boundedStringAt(value: unknown, location: string, maximumLength: number): string {
  const result = stringAt(value, location);
  if (result.length === 0 || [...result].length > maximumLength) {
    return invalidEvent(
      location,
      `must contain 1..${String(maximumLength)} Unicode code points`,
    );
  }
  return result;
}

function booleanAt(value: unknown, location: string): boolean {
  if (typeof value !== "boolean") {
    return invalidEvent(location, "must be a boolean");
  }
  return value;
}

function safeUnsignedIntegerAt(
  value: unknown,
  location: string,
  minimum = 0,
  maximum = MAX_SAFE_INTEGER,
): number {
  if (
    typeof value !== "number" ||
    !Number.isSafeInteger(value) ||
    Object.is(value, -0) ||
    value < minimum ||
    value > maximum
  ) {
    return invalidEvent(
      location,
      `must be an integer between ${String(minimum)} and ${String(maximum)}`,
    );
  }
  return value;
}

function finiteNumberAt(value: unknown, location: string): number {
  if (typeof value !== "number" || !Number.isFinite(value)) {
    return invalidEvent(location, "must be a finite number");
  }
  return value;
}

function uuidAt(value: unknown, location: string): string {
  const uuid = stringAt(value, location);
  if (!UUID_PATTERN.test(uuid)) {
    return invalidEvent(location, "must be a canonical lowercase UUID");
  }
  return uuid;
}

function enumAt<const T extends string>(
  value: unknown,
  allowed: readonly T[],
  location: string,
): T {
  if (typeof value !== "string" || !allowed.includes(value as T)) {
    return invalidEvent(location, `must be one of ${allowed.join(", ")}`);
  }
  return value as T;
}

function arrayAt(value: unknown, location: string): readonly unknown[] {
  if (!Array.isArray(value)) {
    return invalidEvent(location, "must be an array");
  }
  for (let index = 0; index < value.length; index += 1) {
    if (!Object.hasOwn(value, index)) {
      invalidEvent(`${location}[${String(index)}]`, "array entries must not be sparse");
    }
  }
  for (const key of Reflect.ownKeys(value)) {
    if (key === "length") {
      continue;
    }
    if (typeof key !== "string" || !/^(?:0|[1-9][0-9]*)$/u.test(key)) {
      invalidEvent(location, "must not contain non-index properties");
    }
    const descriptor = Object.getOwnPropertyDescriptor(value, key);
    if (!descriptor?.enumerable || !("value" in descriptor)) {
      invalidEvent(location, `entry ${key} must be an enumerable data property`);
    }
  }
  return value;
}

function validateRpcId(value: unknown, location: string): void {
  if (typeof value === "string") {
    return;
  }
  safeUnsignedIntegerAt(value, location);
}

function validateAssetRef(value: unknown, location: string): void {
  const reference = recordAt(value, location);
  exactKeys(reference, ["id", "mediaType", "uri", "sha256"], [], location);
  stringAt(reference.id, `${location}.id`);
  stringAt(reference.mediaType, `${location}.mediaType`);
  stringAt(reference.uri, `${location}.uri`);
  if (reference.sha256 !== null) {
    stringAt(reference.sha256, `${location}.sha256`);
  }
}

function validateViewport(value: unknown, location: string): void {
  const viewport = recordAt(value, location);
  exactKeys(viewport, ["width", "height", "scaleFactor"], [], location);
  safeUnsignedIntegerAt(viewport.width, `${location}.width`, 0, UINT32_MAX);
  safeUnsignedIntegerAt(viewport.height, `${location}.height`, 0, UINT32_MAX);
  finiteNumberAt(viewport.scaleFactor, `${location}.scaleFactor`);
}

function validateUiContext(value: unknown, location: string): "native" | "web" {
  const context = recordAt(value, location);
  exactKeys(context, ["contextKind", "contextId", "documentEpoch"], [], location);
  const contextKind = enumAt(context.contextKind, ["native", "web"], `${location}.contextKind`);
  boundedStringAt(context.contextId, `${location}.contextId`, 4_096);
  boundedStringAt(context.documentEpoch, `${location}.documentEpoch`, 4_096);
  return contextKind;
}

function validateUiSnapshotRef(value: unknown, location: string): void {
  const snapshot = recordAt(value, location);
  exactKeys(
    snapshot,
    ["formatVersion", "context", "nodeCount", "byteLength", "evidence"],
    [],
    location,
  );
  safeUnsignedIntegerAt(snapshot.formatVersion, `${location}.formatVersion`, 1, 1);
  validateUiContext(snapshot.context, `${location}.context`);
  safeUnsignedIntegerAt(snapshot.nodeCount, `${location}.nodeCount`, 1, 10_000);
  safeUnsignedIntegerAt(snapshot.byteLength, `${location}.byteLength`, 1, 786_432);
  validateAssetRef(snapshot.evidence, `${location}.evidence`);
  const evidence = recordAt(snapshot.evidence, `${location}.evidence`);
  if (evidence.mediaType !== UI_SNAPSHOT_MEDIA_TYPE) {
    invalidEvent(`${location}.evidence.mediaType`, "must identify a v1 DeviceRail UI Tree");
  }
}

function validateActionExecution(value: unknown, location: string): void {
  const execution = recordAt(value, location);
  const mode = stringAt(execution.mode, `${location}.mode`);
  switch (mode) {
    case "nativeSemantic":
    case "webSemantic": {
      exactKeys(execution, ["mode", "context"], [], location);
      const contextKind = validateUiContext(execution.context, `${location}.context`);
      const expected = mode === "nativeSemantic" ? "native" : "web";
      if (contextKind !== expected) {
        invalidEvent(`${location}.context.contextKind`, "must match the execution mode");
      }
      return;
    }
    case "coordinateFallback":
      exactKeys(execution, ["mode", "context", "fallbackReason"], [], location);
      validateUiContext(execution.context, `${location}.context`);
      enumAt(
        execution.fallbackReason,
        ["semanticInteractionUnavailable", "platformLimitation"],
        `${location}.fallbackReason`,
      );
      return;
    default:
      invalidEvent(`${location}.mode`, "has an unknown execution mode");
  }
}

function validateObservation(value: unknown, location: string): void {
  const observation = recordAt(value, location);
  exactKeys(
    observation,
    ["id", "deviceId", "capturedAtMs", "viewport", "screenshot", "metadata"],
    ["screenshotOmission", "uiSnapshot", "uiSnapshotOmission"],
    location,
  );
  uuidAt(observation.id, `${location}.id`);
  stringAt(observation.deviceId, `${location}.deviceId`);
  safeUnsignedIntegerAt(observation.capturedAtMs, `${location}.capturedAtMs`);
  validateViewport(observation.viewport, `${location}.viewport`);

  const hasScreenshot = observation.screenshot !== null;
  if (hasScreenshot) {
    validateAssetRef(observation.screenshot, `${location}.screenshot`);
  }
  const hasOmission = Object.hasOwn(observation, "screenshotOmission");
  if (hasOmission) {
    enumAt(
      observation.screenshotOmission,
      ["policy", "protectedAction"],
      `${location}.screenshotOmission`,
    );
  }
  if (hasScreenshot && hasOmission) {
    invalidEvent(location, "screenshot and screenshotOmission are mutually exclusive");
  }
  const hasUiSnapshot = Object.hasOwn(observation, "uiSnapshot");
  if (hasUiSnapshot) {
    validateUiSnapshotRef(observation.uiSnapshot, `${location}.uiSnapshot`);
  }
  const hasUiOmission = Object.hasOwn(observation, "uiSnapshotOmission");
  if (hasUiOmission) {
    enumAt(
      observation.uiSnapshotOmission,
      ["driverUnsupported", "policy", "protectedAction"],
      `${location}.uiSnapshotOmission`,
    );
  }
  if (hasUiSnapshot && hasUiOmission) {
    invalidEvent(location, "uiSnapshot and uiSnapshotOmission are mutually exclusive");
  }
  recordAt(observation.metadata, `${location}.metadata`);
}

function validateErrorInfo(value: unknown, location: string): void {
  const error = recordAt(value, location);
  exactKeys(error, ["code", "message", "retryable", "details"], [], location);
  stringAt(error.code, `${location}.code`);
  stringAt(error.message, `${location}.message`);
  booleanAt(error.retryable, `${location}.retryable`);
}

function validateVerdict(value: unknown, location: string): void {
  const verdict = recordAt(value, location);
  exactKeys(verdict, ["status", "summary", "evidence"], [], location);
  enumAt(verdict.status, ["pass", "fail", "unknown"], `${location}.status`);
  const summary = boundedStringAt(
    verdict.summary,
    `${location}.summary`,
    MAX_VERDICT_SUMMARY_LENGTH,
  );
  if (summary.trim().length === 0) {
    invalidEvent(`${location}.summary`, "must not be blank");
  }
  const evidence = arrayAt(verdict.evidence, `${location}.evidence`);
  if (evidence.length > MAX_VERDICT_EVIDENCE_REFERENCES) {
    invalidEvent(
      `${location}.evidence`,
      `must contain at most ${String(MAX_VERDICT_EVIDENCE_REFERENCES)} references`,
    );
  }
  for (const [index, reference] of evidence.entries()) {
    validateAssetRef(reference, `${location}.evidence[${String(index)}]`);
  }
}

function validateActionResult(value: unknown, location: string): void {
  const result = recordAt(value, location);
  exactKeys(
    result,
    ["callId", "startedAtMs", "finishedAtMs", "output", "before", "after", "evidence"],
    ["execution"],
    location,
  );
  uuidAt(result.callId, `${location}.callId`);
  const startedAtMs = safeUnsignedIntegerAt(result.startedAtMs, `${location}.startedAtMs`);
  const finishedAtMs = safeUnsignedIntegerAt(result.finishedAtMs, `${location}.finishedAtMs`);
  if (finishedAtMs < startedAtMs) {
    invalidEvent(location, "finishedAtMs must not precede startedAtMs");
  }
  for (const key of ["before", "after"] as const) {
    if (result[key] !== null) {
      validateObservation(result[key], `${location}.${key}`);
    }
  }
  for (const [index, reference] of arrayAt(result.evidence, `${location}.evidence`).entries()) {
    validateAssetRef(reference, `${location}.evidence[${String(index)}]`);
  }
  if (Object.hasOwn(result, "execution")) {
    validateActionExecution(result.execution, `${location}.execution`);
  }
}

function validateActionOutcome(value: unknown, location: string): void {
  const outcome = recordAt(value, location);
  const kind = stringAt(outcome.outcome, `${location}.outcome`);
  switch (kind) {
    case "succeeded":
      exactKeys(outcome, ["outcome", "result"], [], location);
      validateActionResult(outcome.result, `${location}.result`);
      return;
    case "failed":
    case "cancelled":
      exactKeys(outcome, ["outcome", "error"], [], location);
      validateErrorInfo(outcome.error, `${location}.error`);
      return;
    case "timedOut":
      exactKeys(outcome, ["outcome", "error", "timeoutMs"], [], location);
      validateErrorInfo(outcome.error, `${location}.error`);
      safeUnsignedIntegerAt(outcome.timeoutMs, `${location}.timeoutMs`);
      return;
    default:
      invalidEvent(`${location}.outcome`, "has an unknown Action outcome");
  }
}

function validateRecordedActionCall(value: unknown, location: string): void {
  const call = recordAt(value, location);
  // `arguments` is intentionally required here even though the historical
  // generated type marks its serde default as optional. Without an own value,
  // redaction cannot be distinguished from an invalid or lossy delivery.
  exactKeys(call, ["id", "name", "arguments"], ["argumentsRedacted"], location);
  uuidAt(call.id, `${location}.id`);
  stringAt(call.name, `${location}.name`);
  if (Object.hasOwn(call, "argumentsRedacted")) {
    const redacted = booleanAt(call.argumentsRedacted, `${location}.argumentsRedacted`);
    if (!redacted) {
      invalidEvent(location, "argumentsRedacted must be omitted when false");
    }
    if (redacted && call.arguments !== null) {
      invalidEvent(location, "redacted Action arguments must be null");
    }
  }
}

function validatePayload(value: unknown, location: string): void {
  const payload = recordAt(value, location);
  const type = stringAt(payload.type, `${location}.type`);
  switch (type) {
    case "sessionStarted":
      exactKeys(payload, ["type"], [], location);
      return;
    case "sessionEnded":
      exactKeys(payload, ["type", "outcome", "reason"], [], location);
      enumAt(
        payload.outcome,
        ["completed", "failed", "cancelled", "shutdown"],
        `${location}.outcome`,
      );
      if (payload.reason !== null) {
        stringAt(payload.reason, `${location}.reason`);
      }
      return;
    case "observationCaptured":
      exactKeys(payload, ["type", "observation"], [], location);
      validateObservation(payload.observation, `${location}.observation`);
      return;
    case "actionStarted":
      exactKeys(payload, ["type", "call"], [], location);
      validateRecordedActionCall(payload.call, `${location}.call`);
      return;
    case "actionCompleted":
      exactKeys(payload, ["type", "callId", "outcome"], [], location);
      uuidAt(payload.callId, `${location}.callId`);
      validateActionOutcome(payload.outcome, `${location}.outcome`);
      return;
    case "mediaStreamStarted": {
      exactKeys(payload, ["type", "stream"], [], location);
      const stream = recordAt(payload.stream, `${location}.stream`);
      exactKeys(stream, ["id", "kind", "mediaType"], ["viewport"], `${location}.stream`);
      uuidAt(stream.id, `${location}.stream.id`);
      enumAt(stream.kind, ["screenshot", "video"], `${location}.stream.kind`);
      stringAt(stream.mediaType, `${location}.stream.mediaType`);
      if (stream.viewport !== undefined) {
        validateViewport(stream.viewport, `${location}.stream.viewport`);
      }
      return;
    }
    case "mediaFrameCaptured": {
      exactKeys(payload, ["type", "frame"], [], location);
      const frame = recordAt(payload.frame, `${location}.frame`);
      exactKeys(
        frame,
        ["streamId", "frameIndex", "evidence"],
        ["keyFrame", "durationMs"],
        `${location}.frame`,
      );
      uuidAt(frame.streamId, `${location}.frame.streamId`);
      safeUnsignedIntegerAt(frame.frameIndex, `${location}.frame.frameIndex`, 1);
      if (frame.keyFrame !== undefined) {
        booleanAt(frame.keyFrame, `${location}.frame.keyFrame`);
      }
      if (frame.durationMs !== undefined) {
        safeUnsignedIntegerAt(frame.durationMs, `${location}.frame.durationMs`);
      }
      validateAssetRef(frame.evidence, `${location}.frame.evidence`);
      return;
    }
    case "mediaStreamEnded":
      exactKeys(payload, ["type", "streamId", "frameCount"], [], location);
      uuidAt(payload.streamId, `${location}.streamId`);
      safeUnsignedIntegerAt(payload.frameCount, `${location}.frameCount`);
      return;
    case "verdictRecorded":
      exactKeys(payload, ["type", "verdict"], [], location);
      validateVerdict(payload.verdict, `${location}.verdict`);
      return;
    case "error":
      exactKeys(payload, ["type", "error"], [], location);
      validateErrorInfo(payload.error, `${location}.error`);
      return;
    default:
      invalidEvent(`${location}.type`, "has an unknown TestEvent payload type");
  }
}

function observationUsesProtocol15(value: unknown): boolean {
  if (value === null || typeof value !== "object" || Array.isArray(value)) {
    return false;
  }
  return Object.hasOwn(value, "uiSnapshot") || Object.hasOwn(value, "uiSnapshotOmission");
}

function payloadUsesProtocol15(value: unknown): boolean {
  if (value === null || typeof value !== "object" || Array.isArray(value)) {
    return false;
  }
  const payload = value as JsonRecord;
  if (payload.type === "observationCaptured") {
    return observationUsesProtocol15(payload.observation);
  }
  if (payload.type !== "actionCompleted") {
    return false;
  }
  const outcome = payload.outcome;
  if (outcome === null || typeof outcome !== "object" || Array.isArray(outcome)) {
    return false;
  }
  const outcomeRecord = outcome as JsonRecord;
  const result = outcomeRecord.result;
  if (
    outcomeRecord.outcome !== "succeeded" ||
    result === null ||
    typeof result !== "object" ||
    Array.isArray(result)
  ) {
    return false;
  }
  const resultRecord = result as JsonRecord;
  return Object.hasOwn(resultRecord, "execution") ||
    observationUsesProtocol15(resultRecord.before) ||
    observationUsesProtocol15(resultRecord.after);
}

function validatePayloadProtocol(
  payload: unknown,
  protocol: ProtocolVersion,
  location: string,
): void {
  if (protocol.major !== 1 || protocol.minor < 0 || protocol.minor > 5) {
    invalidEvent(location, "uses an unsupported event protocol version");
  }
  const payloadType = (payload as JsonRecord).type;
  if (
    protocol.minor < 4 &&
    (payloadType === "mediaStreamStarted" ||
      payloadType === "mediaFrameCaptured" ||
      payloadType === "mediaStreamEnded")
  ) {
    invalidEvent(location, "requires event protocol 1.4 or newer");
  }
  if (protocol.minor < 5 && payloadUsesProtocol15(payload)) {
    invalidEvent(location, "requires event protocol 1.5 or newer");
  }
}

function cloneJson(value: unknown, location: string, context: CloneContext, depth: number): unknown {
  context.nodes += 1;
  if (context.nodes > MAX_JSON_NODES) {
    return invalidEvent(location, `exceeds the ${String(MAX_JSON_NODES)} JSON node limit`);
  }
  if (depth > MAX_JSON_DEPTH) {
    return invalidEvent(location, `exceeds the ${String(MAX_JSON_DEPTH)} JSON depth limit`);
  }
  if (value === null || typeof value === "string" || typeof value === "boolean") {
    return value;
  }
  if (typeof value === "number") {
    if (!Number.isFinite(value) || (Number.isInteger(value) && !Number.isSafeInteger(value))) {
      return invalidEvent(location, "contains an unsafe JSON number");
    }
    return value;
  }
  if (typeof value !== "object") {
    return invalidEvent(location, "contains a non-JSON value");
  }
  if (context.seen.has(value)) {
    return invalidEvent(location, "contains a repeated or cyclic object");
  }
  context.seen.add(value);

  if (Array.isArray(value)) {
    const array = arrayAt(value, location);
    return array.map((entry, index) =>
      cloneJson(entry, `${location}[${String(index)}]`, context, depth + 1),
    );
  }
  const record = recordAt(value, location);
  const clone: Record<string, unknown> = {};
  for (const [key, entry] of Object.entries(record)) {
    Object.defineProperty(clone, key, {
      configurable: true,
      enumerable: true,
      value: cloneJson(entry, `${location}.${key}`, context, depth + 1),
      writable: true,
    });
  }
  return clone;
}

function deepFreeze(value: unknown): void {
  if (value === null || typeof value !== "object" || Object.isFrozen(value)) {
    return;
  }
  for (const child of Array.isArray(value) ? value : Object.values(value as JsonRecord)) {
    deepFreeze(child);
  }
  Object.freeze(value);
}

/** Runtime-validates and takes an immutable snapshot of one untrusted event. */
export function validateTestEvent(
  value: unknown,
  location = "event",
  protocol: ProtocolVersion = { major: 1, minor: 5 },
): TestEvent {
  const event = recordAt(value, location);
  exactKeys(
    event,
    ["eventId", "sessionId", "sequence", "atMs", "payload"],
    ["requestId", "deviceId"],
    location,
  );
  uuidAt(event.eventId, `${location}.eventId`);
  uuidAt(event.sessionId, `${location}.sessionId`);
  safeUnsignedIntegerAt(event.sequence, `${location}.sequence`, 1);
  safeUnsignedIntegerAt(event.atMs, `${location}.atMs`);
  if (Object.hasOwn(event, "requestId")) {
    validateRpcId(event.requestId, `${location}.requestId`);
  }
  if (Object.hasOwn(event, "deviceId")) {
    stringAt(event.deviceId, `${location}.deviceId`);
  }
  validatePayload(event.payload, `${location}.payload`);
  validatePayloadProtocol(event.payload, protocol, `${location}.payload`);

  const snapshot = cloneJson(
    event,
    location,
    { nodes: 0, seen: new WeakSet<object>() },
    1,
  );
  deepFreeze(snapshot);
  return snapshot as TestEvent;
}

function jsonEqual(left: unknown, right: unknown): boolean {
  if (Object.is(left, right)) {
    return true;
  }
  if (typeof left !== typeof right || left === null || right === null) {
    return false;
  }
  if (Array.isArray(left) || Array.isArray(right)) {
    return (
      Array.isArray(left) &&
      Array.isArray(right) &&
      left.length === right.length &&
      left.every((entry, index) => jsonEqual(entry, right[index]))
    );
  }
  if (typeof left !== "object" || typeof right !== "object") {
    return false;
  }
  const leftRecord = left as JsonRecord;
  const rightRecord = right as JsonRecord;
  const leftKeys = Object.keys(leftRecord);
  const rightKeys = Object.keys(rightRecord);
  return (
    leftKeys.length === rightKeys.length &&
    leftKeys.every(
      (key) => Object.hasOwn(rightRecord, key) && jsonEqual(leftRecord[key], rightRecord[key]),
    )
  );
}

function correlation(event: TestEvent, field: "requestId" | "deviceId"): CorrelationValue {
  if (!Object.hasOwn(event, field)) {
    return ABSENT;
  }
  const value = event[field];
  if (value === null || typeof value === "string" || typeof value === "number") {
    return value;
  }
  // Runtime validation above makes this unreachable.
  return ABSENT;
}

function stateError(
  code: ConstructorParameters<typeof RecorderError>[0],
  message: string,
  details: Readonly<Record<string, unknown>>,
): never {
  throw new RecorderError(code, message, { details });
}

function applyNewEvent(state: MutableLogState, event: TestEvent): void {
  const payload = event.payload;
  const first = state.eventCount === 0;
  if (first && payload.type !== "sessionStarted") {
    stateError("invalid_lifecycle", "sequence 1 must be sessionStarted", {
      sequence: event.sequence,
    });
  }
  if (!first && payload.type === "sessionStarted") {
    stateError("invalid_lifecycle", "sessionStarted may only appear at sequence 1", {
      sequence: event.sequence,
    });
  }
  if (state.eventIds.has(event.eventId)) {
    stateError("duplicate_event_id", "eventId was reused at a different sequence", {
      eventId: event.eventId,
      sequence: event.sequence,
    });
  }

  switch (payload.type) {
    case "actionStarted": {
      if (state.seenCallIds.has(payload.call.id)) {
        stateError("action_call_reused", "Action call id was reused", {
          callId: payload.call.id,
          sequence: event.sequence,
        });
      }
      state.seenCallIds.add(payload.call.id);
      state.openActions.set(payload.call.id, {
        deviceId: correlation(event, "deviceId"),
        requestId: correlation(event, "requestId"),
      });
      break;
    }
    case "actionCompleted": {
      const started = state.openActions.get(payload.callId);
      if (!started) {
        stateError("action_not_started", "Action completion has no open ActionStarted", {
          callId: payload.callId,
          sequence: event.sequence,
        });
      }
      if (
        !Object.is(started.requestId, correlation(event, "requestId")) ||
        !Object.is(started.deviceId, correlation(event, "deviceId"))
      ) {
        stateError("action_correlation_mismatch", "Action event correlation changed", {
          callId: payload.callId,
          sequence: event.sequence,
        });
      }
      if (
        payload.outcome.outcome === "succeeded" &&
        payload.outcome.result.callId !== payload.callId
      ) {
        stateError("action_result_mismatch", "successful ActionResult has a different callId", {
          callId: payload.callId,
          resultCallId: payload.outcome.result.callId,
          sequence: event.sequence,
        });
      }
      state.openActions.delete(payload.callId);
      break;
    }
    case "mediaStreamStarted": {
      if (state.seenMediaStreamIds.has(payload.stream.id)) {
        stateError("invalid_lifecycle", "media stream id was reused", {
          sequence: event.sequence,
          streamId: payload.stream.id,
        });
      }
      state.seenMediaStreamIds.add(payload.stream.id);
      state.openMediaStreams.set(payload.stream.id, {
        mediaType: payload.stream.mediaType,
        nextFrameIndex: 1,
      });
      break;
    }
    case "mediaFrameCaptured": {
      const stream = state.openMediaStreams.get(payload.frame.streamId);
      if (
        !stream
        || payload.frame.frameIndex !== stream.nextFrameIndex
        || payload.frame.evidence.mediaType !== stream.mediaType
      ) {
        stateError("invalid_lifecycle", "media frame does not match an active stream", {
          sequence: event.sequence,
          streamId: payload.frame.streamId,
        });
      }
      state.openMediaStreams.set(payload.frame.streamId, {
        mediaType: stream.mediaType,
        nextFrameIndex: stream.nextFrameIndex + 1,
      });
      break;
    }
    case "mediaStreamEnded": {
      const stream = state.openMediaStreams.get(payload.streamId);
      if (!stream || payload.frameCount !== stream.nextFrameIndex - 1) {
        stateError("invalid_lifecycle", "media stream frame count is inconsistent", {
          sequence: event.sequence,
          streamId: payload.streamId,
        });
      }
      state.openMediaStreams.delete(payload.streamId);
      break;
    }
    case "sessionEnded":
      if (state.openActions.size !== 0) {
        stateError("action_in_flight", "Session ended with Action calls still open", {
          openActionCount: state.openActions.size,
          sequence: event.sequence,
        });
      }
      if (state.openMediaStreams.size !== 0) {
        stateError("invalid_lifecycle", "Session ended with media streams still open", {
          openMediaStreamCount: state.openMediaStreams.size,
          sequence: event.sequence,
        });
      }
      state.terminal = true;
      break;
    case "sessionStarted":
    case "observationCaptured":
    case "verdictRecorded":
    case "error":
      break;
  }

  state.appendedEvents.push(event);
  state.eventCount += 1;
  state.eventIds.add(event.eventId);
}

/** Sequence-authoritative, transport-neutral Session event accumulator. */
export class EventLog {
  readonly #sessionId: string;
  readonly #eventProtocolVersion: ProtocolVersion;
  readonly #identity = Object.freeze({});
  #eventIds = new Set<string>();
  #eventChunks: (readonly TestEvent[])[] = [];
  #eventsSnapshot: readonly TestEvent[] | undefined = Object.freeze([]);
  #openActions = new Map<string, OpenAction>();
  #openMediaStreams = new Map<string, OpenMediaStream>();
  #seenCallIds = new Set<string>();
  #seenMediaStreamIds = new Set<string>();
  #terminal = false;
  #generation = 0;

  constructor(sessionId: string, eventProtocolVersion: ProtocolVersion = { major: 1, minor: 5 }) {
    this.#sessionId = uuidAt(sessionId, "sessionId");
    this.#eventProtocolVersion = { ...eventProtocolVersion };
  }

  /** Strictly reconstructs a canonical checkpoint log. */
  static replay(
    sessionId: string,
    events: readonly unknown[],
    eventProtocolVersion: ProtocolVersion = { major: 1, minor: 5 },
  ): EventLog {
    const log = new EventLog(sessionId, eventProtocolVersion);
    const result = log.acceptBatch(events);
    if (result.duplicates !== 0) {
      stateError("sequence_conflict", "checkpoint replay contains duplicate sequences", {
        duplicates: result.duplicates,
      });
    }
    return log;
  }

  get sessionId(): string {
    return this.#sessionId;
  }

  get events(): readonly TestEvent[] {
    this.#eventsSnapshot ??= Object.freeze(this.#eventChunks.flat());
    return this.#eventsSnapshot;
  }

  get lastSequence(): number | null {
    return this.#eventChunks.at(-1)?.at(-1)?.sequence ?? null;
  }

  get nextSequence(): number {
    return (this.lastSequence ?? 0) + 1;
  }

  get terminal(): boolean {
    return this.#terminal;
  }

  get openActionCount(): number {
    return this.#openActions.size;
  }

  /**
   * Creates an isolated speculative branch. Immutable event chunks are shared;
   * identity indexes are copied because subsequent writes to either branch must
   * remain independent. The Recorder hot path uses `prepareBatch` instead.
   */
  fork(): EventLog {
    const candidate = new EventLog(this.#sessionId, this.#eventProtocolVersion);
    candidate.#eventIds = new Set(this.#eventIds);
    candidate.#eventChunks = [...this.#eventChunks];
    candidate.#eventsSnapshot = this.#eventsSnapshot;
    candidate.#openActions = new Map(this.#openActions);
    candidate.#openMediaStreams = new Map(this.#openMediaStreams);
    candidate.#seenCallIds = new Set(this.#seenCallIds);
    candidate.#seenMediaStreamIds = new Set(this.#seenMediaStreamIds);
    candidate.#terminal = this.#terminal;
    candidate.#generation = this.#generation;
    return candidate;
  }

  accept(value: unknown): EventAcceptance {
    const result = this.acceptBatch([value]);
    return result.accepted === 1 ? "accepted" : "duplicate";
  }

  /** Validate a batch without copying or mutating the confirmed event prefix. */
  prepareBatch(values: readonly unknown[]): PreparedEventBatch {
    const incoming = values.map((value, index) =>
      validateTestEvent(value, `events[${String(index)}]`, this.#eventProtocolVersion),
    );
    for (const event of incoming) {
      if (event.sessionId !== this.#sessionId) {
        stateError("session_mismatch", "event belongs to a different Session", {
          actualSessionId: event.sessionId,
          expectedSessionId: this.#sessionId,
          sequence: event.sequence,
        });
      }
    }

    const baseEventCount = this.lastSequence ?? 0;
    const state: MutableLogState = {
      owner: this.#identity,
      eventCount: baseEventCount,
      appendedEvents: [],
      eventIds: new OverlaySet(this.#eventIds),
      openActions: new Map(this.#openActions),
      openMediaStreams: new Map(this.#openMediaStreams),
      seenCallIds: new OverlaySet(this.#seenCallIds),
      seenMediaStreamIds: new OverlaySet(this.#seenMediaStreamIds),
      terminal: this.#terminal,
    };
    let duplicates = 0;

    for (const [index, event] of incoming.entries()) {
      const expected = state.eventCount + 1;
      if (event.sequence < expected) {
        const appendedIndex = event.sequence - baseEventCount - 1;
        const existing = appendedIndex >= 0
          ? state.appendedEvents[appendedIndex]
          : this.#eventAt(event.sequence);
        if (!existing || !jsonEqual(existing, event)) {
          stateError("sequence_conflict", "a delivered sequence differs from its recorded event", {
            sequence: event.sequence,
          });
        }
        duplicates += 1;
        continue;
      }
      if (state.terminal) {
        stateError("terminal_append", "a new event cannot follow sessionEnded", {
          sequence: event.sequence,
        });
      }
      if (event.sequence > expected) {
        const expectedAppearsLater = incoming
          .slice(index + 1)
          .some((candidate) => candidate.sequence === expected);
        stateError(
          expectedAppearsLater ? "out_of_order" : "sequence_gap",
          expectedAppearsLater
            ? "event delivery is out of sequence"
            : "event delivery contains a sequence gap",
          { actualSequence: event.sequence, expectedSequence: expected },
        );
      }
      applyNewEvent(state, event);
    }

    const result = Object.freeze({
      accepted: state.appendedEvents.length,
      duplicates,
      lastSequence: state.eventCount === 0 ? null : state.eventCount,
      terminal: state.terminal,
    });
    return Object.freeze({
      baseGeneration: this.#generation,
      acceptedEvents: Object.freeze([...state.appendedEvents]),
      result,
      [PREPARED_STATE]: state,
    });
  }

  /** Commit a previously validated batch without replaying its durable prefix. */
  commitPreparedBatch(prepared: PreparedEventBatch): EventBatchResult {
    const state = prepared[PREPARED_STATE];
    if (state.owner !== this.#identity || prepared.baseGeneration !== this.#generation) {
      stateError("sequence_conflict", "event log changed after the batch was prepared", {
        actualGeneration: this.#generation,
        expectedGeneration: prepared.baseGeneration,
      });
    }
    if (prepared.acceptedEvents.length === 0) {
      return prepared.result;
    }
    this.#eventChunks.push(Object.freeze([...prepared.acceptedEvents]));
    this.#eventsSnapshot = undefined;
    for (const eventId of state.eventIds.added) {
      this.#eventIds.add(eventId);
    }
    for (const callId of state.seenCallIds.added) {
      this.#seenCallIds.add(callId);
    }
    for (const streamId of state.seenMediaStreamIds.added) {
      this.#seenMediaStreamIds.add(streamId);
    }
    this.#openActions = state.openActions;
    this.#openMediaStreams = state.openMediaStreams;
    this.#terminal = state.terminal;
    this.#generation += 1;
    return prepared.result;
  }

  #eventAt(sequence: number): TestEvent | undefined {
    let offset = sequence - 1;
    for (const chunk of this.#eventChunks) {
      if (offset < chunk.length) {
        return chunk[offset];
      }
      offset -= chunk.length;
    }
    return undefined;
  }

  /**
   * Accepts one delivery batch atomically. Any malformed event or state-machine
   * failure leaves the prior log unchanged.
   */
  acceptBatch(values: readonly unknown[]): EventBatchResult {
    return this.commitPreparedBatch(this.prepareBatch(values));
  }

  snapshot(): EventLogSnapshot {
    return Object.freeze({
      sessionId: this.sessionId,
      events: this.events,
      lastSequence: this.lastSequence,
      terminal: this.terminal,
      openActionCount: this.openActionCount,
    });
  }
}
