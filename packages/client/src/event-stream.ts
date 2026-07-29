import type {
  ErrorInfo,
  EventStreamCursor,
  EventStreamOriginPolicy,
  EventsStreamOpenResult,
  EventsStreamTerminalParams,
  EventsStreamTermination,
  EventsSubscribeParams,
  EventsSubscribeResult,
  HelloResult,
  PeerInfo,
  ProtocolVersion,
  RpcError,
  RpcId,
  TestEvent,
} from "@devicerail/protocol";

import {
  EventStreamAbortedError,
  EventStreamClosedError,
  EventStreamCursorError,
  EventStreamQueueOverflowError,
  EventStreamRemoteTerminationError,
  ProtocolViolationError,
  RequestAbortedError,
  RpcRemoteError,
} from "./errors.js";
import { assertPureJsonValue, validateRpcResponse } from "./rpc-response-validator.js";

const EVENTS_STREAM_FEATURE = "events.stream.v1";
const MEDIA_PAYLOAD_TYPES = new Set([
  "mediaStreamStarted",
  "mediaFrameCaptured",
  "mediaStreamEnded",
]);
const UI_SNAPSHOT_MEDIA_TYPE = "application/vnd.devicerail.ui-tree+json;version=1";
const HELLO_REQUEST_ID = "devicerail:event-stream:hello";
const SUBSCRIBE_REQUEST_ID = "devicerail:event-stream:subscribe";
const UUID_PATTERN = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/iu;

export const DEFAULT_EVENT_STREAM_MAX_MESSAGE_BYTES = 1024 * 1024;
export const DEFAULT_EVENT_STREAM_MAX_QUEUED_BYTES = 4 * 1024 * 1024;
export const DEFAULT_EVENT_STREAM_MAX_QUEUED_EVENTS = 64;

export interface EventStreamWebSocketEvent {
  readonly code?: number;
  readonly data?: unknown;
  readonly reason?: string;
}

export type EventStreamWebSocketEventType = "close" | "error" | "message" | "open";

/** The minimal WebSocket surface used by the Node client and its deterministic tests. */
export interface EventStreamWebSocket {
  readonly protocol: string;
  addEventListener(
    type: EventStreamWebSocketEventType,
    listener: (event: EventStreamWebSocketEvent) => void,
  ): void;
  close(code?: number, reason?: string): void;
  removeEventListener(
    type: EventStreamWebSocketEventType,
    listener: (event: EventStreamWebSocketEvent) => void,
  ): void;
  send(data: string): void;
}

export type EventStreamWebSocketFactory = (
  endpoint: string,
  subprotocol: "devicerail.events.v1",
) => EventStreamWebSocket;

export interface EventStreamOptions {
  readonly maxMessageBytes?: number;
  readonly maxQueuedBytes?: number;
  readonly maxQueuedEvents?: number;
  /** Defaults to `absent` for Node; browser callers bind the capability to one exact Origin. */
  readonly originPolicy?: EventStreamOriginPolicy;
  readonly signal?: AbortSignal;
  readonly webSocketFactory?: EventStreamWebSocketFactory;
}

export interface EventStreamItem {
  readonly cursor: EventStreamCursor;
  readonly event: TestEvent;
  /** Advances the resumable cursor only when this is the next delivered event. */
  confirm(): EventStreamCursor;
}

type EventStreamCapabilityOpener = (
  sessionId: string,
  originPolicy: EventStreamOriginPolicy,
  signal?: AbortSignal,
) => Promise<EventsStreamOpenResult>;

interface ValidatedEvent {
  readonly bytes: number;
  readonly cursor: EventStreamCursor;
  readonly event: TestEvent;
}

interface QueuedEvent {
  readonly bytes: number;
  readonly item: EventStreamItem;
}

interface PendingNext {
  readonly reject: (error: Error) => void;
  readonly resolve: (result: IteratorResult<EventStreamItem>) => void;
}

interface StreamSettings {
  readonly maxMessageBytes: number;
  readonly maxQueuedBytes: number;
  readonly maxQueuedEvents: number;
  readonly originPolicy: EventStreamOriginPolicy;
  readonly webSocketFactory: EventStreamWebSocketFactory;
}

type StreamPhase =
  | "connecting"
  | "awaitingHello"
  | "awaitingSubscribe"
  | "streaming"
  | "finished";

function isObject(value: unknown): value is Record<string, unknown> {
  return value !== null && typeof value === "object" && !Array.isArray(value);
}

function assertExactKeys(
  value: Record<string, unknown>,
  required: readonly string[],
  optional: readonly string[],
  location: string,
): void {
  const allowed = new Set([...required, ...optional]);
  for (const key of required) {
    if (!Object.hasOwn(value, key)) {
      throw new ProtocolViolationError(`${location} is missing ${key}`);
    }
  }
  for (const key of Object.keys(value)) {
    if (!allowed.has(key)) {
      throw new ProtocolViolationError(`${location} contains unknown field ${key}`);
    }
  }
}

function assertRequiredKeys(
  value: Record<string, unknown>,
  required: readonly string[],
  location: string,
): void {
  for (const key of required) {
    if (!Object.hasOwn(value, key)) {
      throw new ProtocolViolationError(`${location} is missing ${key}`);
    }
  }
}

function positiveSafeInteger(value: number, name: string): number {
  if (!Number.isSafeInteger(value) || value <= 0) {
    throw new RangeError(`${name} must be a positive safe integer`);
  }
  return value;
}

function safeUnsignedInteger(value: unknown, location: string, oneBased = false): number {
  if (
    typeof value !== "number" ||
    !Number.isSafeInteger(value) ||
    value < (oneBased ? 1 : 0)
  ) {
    throw new ProtocolViolationError(
      `${location} must be a ${oneBased ? "positive" : "non-negative"} safe integer`,
    );
  }
  return value;
}

function uuid(value: unknown, location: string): string {
  if (typeof value !== "string" || !UUID_PATTERN.test(value)) {
    throw new ProtocolViolationError(`${location} must be a UUID`);
  }
  return value;
}

function stringValue(value: unknown, location: string): string {
  if (typeof value !== "string") {
    throw new ProtocolViolationError(`${location} must be a string`);
  }
  return value;
}

function boundedString(value: unknown, location: string, maxLength: number): string {
  const result = stringValue(value, location);
  if (result.length === 0 || [...result].length > maxLength) {
    throw new ProtocolViolationError(
      `${location} must contain 1..${String(maxLength)} Unicode code points`,
    );
  }
  return result;
}

function validateCursor(value: unknown, location: string): EventStreamCursor {
  if (!isObject(value)) {
    throw new ProtocolViolationError(`${location} must be an object`);
  }
  assertExactKeys(value, ["sequence", "sessionId", "streamEpoch"], [], location);
  return {
    sequence: safeUnsignedInteger(value.sequence, `${location}.sequence`, true),
    sessionId: uuid(value.sessionId, `${location}.sessionId`),
    streamEpoch: uuid(value.streamEpoch, `${location}.streamEpoch`),
  };
}

function sameCursor(left: EventStreamCursor, right: EventStreamCursor): boolean {
  return (
    left.sequence === right.sequence &&
    left.sessionId === right.sessionId &&
    left.streamEpoch === right.streamEpoch
  );
}

function cloneCursor(cursor: EventStreamCursor): EventStreamCursor {
  return { ...cursor };
}

function validateErrorInfo(value: unknown, location: string): ErrorInfo {
  if (!isObject(value)) {
    throw new ProtocolViolationError(`${location} must be an object`);
  }
  assertExactKeys(value, ["code", "message", "retryable"], ["details"], location);
  if (
    typeof value.code !== "string" ||
    typeof value.message !== "string" ||
    typeof value.retryable !== "boolean"
  ) {
    throw new ProtocolViolationError(`${location} has invalid code/message/retryable fields`);
  }
  return value as unknown as ErrorInfo;
}

function validateRpcError(value: unknown, location: string): RpcError {
  if (!isObject(value)) {
    throw new ProtocolViolationError(`${location} must be an object`);
  }
  assertExactKeys(value, ["code", "data", "message"], [], location);
  if (
    typeof value.code !== "number" ||
    !Number.isInteger(value.code) ||
    value.code < -2_147_483_648 ||
    value.code > 2_147_483_647 ||
    typeof value.message !== "string"
  ) {
    throw new ProtocolViolationError(`${location} has an invalid JSON-RPC code or message`);
  }
  validateErrorInfo(value.data, `${location}.data`);
  return value as unknown as RpcError;
}

function parseResponseResult(value: unknown, expectedId: RpcId, location: string): unknown {
  if (!isObject(value) || value.jsonrpc !== "2.0") {
    throw new ProtocolViolationError(`${location} must be a JSON-RPC 2.0 response`);
  }
  if (value.id !== expectedId) {
    throw new ProtocolViolationError(`${location} has an unexpected response id`);
  }
  const hasResult = Object.hasOwn(value, "result");
  const hasError = Object.hasOwn(value, "error");
  if (hasResult === hasError) {
    throw new ProtocolViolationError(`${location} must contain exactly one of result or error`);
  }
  if (hasError) {
    assertExactKeys(value, ["error", "id", "jsonrpc"], [], location);
    throw new RpcRemoteError(expectedId, validateRpcError(value.error, `${location}.error`));
  }
  assertExactKeys(value, ["id", "jsonrpc", "result"], [], location);
  return value.result;
}

function validateOpenResult(value: unknown): EventsStreamOpenResult {
  if (!isObject(value)) {
    throw new ProtocolViolationError("events.stream.open result must be an object");
  }
  assertExactKeys(
    value,
    ["endpoint", "expiresAtMs", "streamEpoch"],
    [],
    "events.stream.open result",
  );
  if (typeof value.endpoint !== "string" || value.endpoint.length === 0 || value.endpoint.length > 2_048) {
    throw new ProtocolViolationError("events.stream.open result.endpoint must contain 1..2048 characters");
  }
  let endpoint: URL;
  try {
    endpoint = new URL(value.endpoint);
  } catch (cause) {
    throw new ProtocolViolationError("events.stream.open result.endpoint must be an absolute URL", {
      cause,
    });
  }
  if (
    endpoint.protocol !== "ws:" ||
    endpoint.hostname !== "127.0.0.1" ||
    endpoint.port === "" ||
    endpoint.username !== "" ||
    endpoint.password !== "" ||
    endpoint.hash !== "" ||
    endpoint.search !== "" ||
    endpoint.pathname === "/"
  ) {
    throw new ProtocolViolationError(
      "events.stream.open result.endpoint must be an uncredentialed loopback ws:// URL with a capability path",
    );
  }
  const port = Number(endpoint.port);
  if (!Number.isInteger(port) || port < 1 || port > 65_535) {
    throw new ProtocolViolationError("events.stream.open result.endpoint has an invalid port");
  }
  return {
    endpoint: value.endpoint,
    expiresAtMs: safeUnsignedInteger(
      value.expiresAtMs,
      "events.stream.open result.expiresAtMs",
    ),
    streamEpoch: uuid(value.streamEpoch, "events.stream.open result.streamEpoch"),
  };
}

function validateHelloResult(value: unknown, expectedProtocol: ProtocolVersion): HelloResult {
  if (!isObject(value)) {
    throw new ProtocolViolationError("WebSocket system.hello result must be an object");
  }
  assertExactKeys(
    value,
    ["connectionId", "features", "protocol", "server", "transport"],
    [],
    "WebSocket system.hello result",
  );
  uuid(value.connectionId, "WebSocket system.hello result.connectionId");
  if (!isObject(value.features)) {
    throw new ProtocolViolationError("WebSocket system.hello result.features must be an object");
  }
  assertExactKeys(value.features, ["enabled"], [], "WebSocket system.hello result.features");
  if (
    !Array.isArray(value.features.enabled) ||
    value.features.enabled.length !== 1 ||
    value.features.enabled[0] !== EVENTS_STREAM_FEATURE
  ) {
    throw new ProtocolViolationError(
      `WebSocket system.hello must enable only ${EVENTS_STREAM_FEATURE}`,
    );
  }
  if (!isObject(value.protocol) || !isObject(value.protocol.selected)) {
    throw new ProtocolViolationError("WebSocket system.hello result.protocol is invalid");
  }
  assertExactKeys(value.protocol, ["selected"], [], "WebSocket system.hello result.protocol");
  assertExactKeys(
    value.protocol.selected,
    ["major", "minor"],
    [],
    "WebSocket system.hello result.protocol.selected",
  );
  if (
    value.protocol.selected.major !== expectedProtocol.major
    || value.protocol.selected.minor !== expectedProtocol.minor
  ) {
    throw new ProtocolViolationError(
      "WebSocket system.hello selected a protocol different from the control connection",
    );
  }
  if (!isObject(value.server)) {
    throw new ProtocolViolationError("WebSocket system.hello result.server must be an object");
  }
  assertExactKeys(value.server, ["name", "version"], [], "WebSocket system.hello result.server");
  if (typeof value.server.name !== "string" || typeof value.server.version !== "string") {
    throw new ProtocolViolationError("WebSocket system.hello server name/version must be strings");
  }
  if (!isObject(value.transport)) {
    throw new ProtocolViolationError("WebSocket system.hello result.transport must be an object");
  }
  assertExactKeys(
    value.transport,
    ["framing", "kind"],
    [],
    "WebSocket system.hello result.transport",
  );
  if (value.transport.kind !== "webSocket" || value.transport.framing !== "jsonMessage") {
    throw new ProtocolViolationError(
      "WebSocket system.hello must negotiate webSocket/jsonMessage transport",
    );
  }
  return value as unknown as HelloResult;
}

function validateSubscribeResult(
  value: unknown,
  sessionId: string,
  streamEpoch: string,
  afterCursor: EventStreamCursor | undefined,
): EventsSubscribeResult {
  if (!isObject(value)) {
    throw new ProtocolViolationError("events.subscribe result must be an object");
  }
  assertExactKeys(
    value,
    ["replayThrough", "sessionId", "sessionState", "subscriptionId"],
    [],
    "events.subscribe result",
  );
  const resultSessionId = uuid(value.sessionId, "events.subscribe result.sessionId");
  const subscriptionId = uuid(
    value.subscriptionId,
    "events.subscribe result.subscriptionId",
  );
  const replayThrough = validateCursor(
    value.replayThrough,
    "events.subscribe result.replayThrough",
  );
  if (value.sessionState !== "active" && value.sessionState !== "ended") {
    throw new ProtocolViolationError("events.subscribe result.sessionState is invalid");
  }
  if (
    resultSessionId !== sessionId ||
    replayThrough.sessionId !== sessionId ||
    replayThrough.streamEpoch !== streamEpoch
  ) {
    throw new EventStreamCursorError("events.subscribe returned a cursor for another stream");
  }
  if (afterCursor && replayThrough.sequence < afterCursor.sequence) {
    throw new EventStreamCursorError("events.subscribe replay boundary precedes afterCursor");
  }
  return {
    replayThrough,
    sessionId: resultSessionId,
    sessionState: value.sessionState,
    subscriptionId,
  };
}

function validateAsset(value: unknown, location: string): void {
  if (!isObject(value)) {
    throw new ProtocolViolationError(`${location} must be an object`);
  }
  assertRequiredKeys(value, ["id", "mediaType", "uri"], location);
  for (const key of ["id", "mediaType", "uri"] as const) {
    stringValue(value[key], `${location}.${key}`);
  }
  if (value.sha256 !== undefined && value.sha256 !== null && typeof value.sha256 !== "string") {
    throw new ProtocolViolationError(`${location}.sha256 must be a string or null`);
  }
}

function validateMediaViewport(value: unknown, location: string): void {
  if (!isObject(value)) {
    throw new ProtocolViolationError(`${location} must be an object`);
  }
  assertExactKeys(value, ["height", "scaleFactor", "width"], [], location);
  for (const key of ["width", "height"] as const) {
    const dimension = safeUnsignedInteger(value[key], `${location}.${key}`, true);
    if (dimension > 4_294_967_295) {
      throw new ProtocolViolationError(`${location}.${key} exceeds u32`);
    }
  }
  if (typeof value.scaleFactor !== "number" || !Number.isFinite(value.scaleFactor) || value.scaleFactor <= 0) {
    throw new ProtocolViolationError(`${location}.scaleFactor must be positive and finite`);
  }
}

function validateUiContext(value: unknown, location: string): "native" | "web" {
  if (!isObject(value)) {
    throw new ProtocolViolationError(`${location} must be an object`);
  }
  assertExactKeys(value, ["contextKind", "contextId", "documentEpoch"], [], location);
  if (value.contextKind !== "native" && value.contextKind !== "web") {
    throw new ProtocolViolationError(`${location}.contextKind is invalid`);
  }
  boundedString(value.contextId, `${location}.contextId`, 4_096);
  boundedString(value.documentEpoch, `${location}.documentEpoch`, 4_096);
  return value.contextKind;
}

function validateUiSnapshotRef(value: unknown, location: string): void {
  if (!isObject(value)) {
    throw new ProtocolViolationError(`${location} must be an object`);
  }
  assertExactKeys(
    value,
    ["formatVersion", "context", "nodeCount", "byteLength", "evidence"],
    [],
    location,
  );
  if (value.formatVersion !== 1) {
    throw new ProtocolViolationError(`${location}.formatVersion must be 1`);
  }
  validateUiContext(value.context, `${location}.context`);
  const nodeCount = safeUnsignedInteger(value.nodeCount, `${location}.nodeCount`, true);
  if (nodeCount > 10_000) {
    throw new ProtocolViolationError(`${location}.nodeCount exceeds 10000`);
  }
  const byteLength = safeUnsignedInteger(value.byteLength, `${location}.byteLength`, true);
  if (byteLength > 786_432) {
    throw new ProtocolViolationError(`${location}.byteLength exceeds 786432`);
  }
  validateAsset(value.evidence, `${location}.evidence`);
  if ((value.evidence as Record<string, unknown>).mediaType !== UI_SNAPSHOT_MEDIA_TYPE) {
    throw new ProtocolViolationError(`${location}.evidence.mediaType is not a v1 UI Tree`);
  }
}

function validateActionExecution(value: unknown, location: string): void {
  if (!isObject(value) || typeof value.mode !== "string") {
    throw new ProtocolViolationError(`${location} must be a tagged object`);
  }
  switch (value.mode) {
    case "nativeSemantic":
    case "webSemantic": {
      assertExactKeys(value, ["mode", "context"], [], location);
      const kind = validateUiContext(value.context, `${location}.context`);
      const expected = value.mode === "nativeSemantic" ? "native" : "web";
      if (kind !== expected) {
        throw new ProtocolViolationError(`${location}.context.contextKind does not match mode`);
      }
      break;
    }
    case "coordinateFallback":
      assertExactKeys(value, ["mode", "context", "fallbackReason"], [], location);
      validateUiContext(value.context, `${location}.context`);
      if (
        value.fallbackReason !== "semanticInteractionUnavailable" &&
        value.fallbackReason !== "platformLimitation"
      ) {
        throw new ProtocolViolationError(`${location}.fallbackReason is invalid`);
      }
      break;
    default:
      throw new ProtocolViolationError(`${location}.mode is invalid`);
  }
}

function validateObservation(value: unknown, location: string): void {
  if (!isObject(value) || !isObject(value.viewport)) {
    throw new ProtocolViolationError(`${location} and its viewport must be objects`);
  }
  assertRequiredKeys(value, ["capturedAtMs", "deviceId", "id", "viewport"], location);
  assertRequiredKeys(value.viewport, ["height", "scaleFactor", "width"], `${location}.viewport`);
  uuid(value.id, `${location}.id`);
  stringValue(value.deviceId, `${location}.deviceId`);
  safeUnsignedInteger(value.capturedAtMs, `${location}.capturedAtMs`);
  for (const key of ["width", "height"] as const) {
    const dimension = safeUnsignedInteger(value.viewport[key], `${location}.viewport.${key}`);
    if (dimension > 4_294_967_295) {
      throw new ProtocolViolationError(`${location}.viewport.${key} exceeds u32`);
    }
  }
  if (typeof value.viewport.scaleFactor !== "number" || !Number.isFinite(value.viewport.scaleFactor)) {
    throw new ProtocolViolationError(`${location}.viewport.scaleFactor must be finite`);
  }
  if (value.screenshot !== undefined && value.screenshot !== null) {
    validateAsset(value.screenshot, `${location}.screenshot`);
  }
  if (
    value.screenshotOmission !== undefined &&
    value.screenshotOmission !== null &&
    value.screenshotOmission !== "policy" &&
    value.screenshotOmission !== "protectedAction"
  ) {
    throw new ProtocolViolationError(`${location}.screenshotOmission is invalid`);
  }
  if (
    value.screenshot !== undefined &&
    value.screenshot !== null &&
    value.screenshotOmission !== undefined
  ) {
    throw new ProtocolViolationError(`${location} has both screenshot and screenshotOmission`);
  }
  if (value.uiSnapshot !== undefined) {
    validateUiSnapshotRef(value.uiSnapshot, `${location}.uiSnapshot`);
  }
  if (
    value.uiSnapshotOmission !== undefined &&
    value.uiSnapshotOmission !== "driverUnsupported" &&
    value.uiSnapshotOmission !== "policy" &&
    value.uiSnapshotOmission !== "protectedAction"
  ) {
    throw new ProtocolViolationError(`${location}.uiSnapshotOmission is invalid`);
  }
  if (value.uiSnapshot !== undefined && value.uiSnapshotOmission !== undefined) {
    throw new ProtocolViolationError(`${location} has both uiSnapshot and uiSnapshotOmission`);
  }
  if (value.metadata !== undefined && !isObject(value.metadata)) {
    throw new ProtocolViolationError(`${location}.metadata must be an object`);
  }
}

function validateActionResult(value: unknown, location: string): void {
  if (!isObject(value)) {
    throw new ProtocolViolationError(`${location} must be an object`);
  }
  assertRequiredKeys(value, ["callId", "finishedAtMs", "output", "startedAtMs"], location);
  uuid(value.callId, `${location}.callId`);
  safeUnsignedInteger(value.startedAtMs, `${location}.startedAtMs`);
  safeUnsignedInteger(value.finishedAtMs, `${location}.finishedAtMs`);
  if (!Object.hasOwn(value, "output")) {
    throw new ProtocolViolationError(`${location} is missing output`);
  }
  for (const key of ["before", "after"] as const) {
    if (value[key] !== undefined && value[key] !== null) {
      validateObservation(value[key], `${location}.${key}`);
    }
  }
  if (value.evidence !== undefined) {
    if (!Array.isArray(value.evidence)) {
      throw new ProtocolViolationError(`${location}.evidence must be an array`);
    }
    value.evidence.forEach((asset, index) => validateAsset(asset, `${location}.evidence[${index}]`));
  }
  if (value.execution !== undefined) {
    validateActionExecution(value.execution, `${location}.execution`);
  }
}

function validateActionOutcome(value: unknown, location: string): void {
  if (!isObject(value) || typeof value.outcome !== "string") {
    throw new ProtocolViolationError(`${location} must be a tagged object`);
  }
  switch (value.outcome) {
    case "succeeded":
      assertExactKeys(value, ["outcome", "result"], [], location);
      validateActionResult(value.result, `${location}.result`);
      break;
    case "failed":
    case "cancelled":
      assertExactKeys(value, ["error", "outcome"], [], location);
      validateErrorInfo(value.error, `${location}.error`);
      break;
    case "timedOut":
      assertExactKeys(value, ["error", "outcome", "timeoutMs"], [], location);
      validateErrorInfo(value.error, `${location}.error`);
      safeUnsignedInteger(value.timeoutMs, `${location}.timeoutMs`);
      break;
    default:
      throw new ProtocolViolationError(`${location}.outcome is invalid`);
  }
}

function validatePayload(value: unknown, location: string): void {
  if (!isObject(value) || typeof value.type !== "string") {
    throw new ProtocolViolationError(`${location} must be a tagged object`);
  }
  switch (value.type) {
    case "sessionStarted":
      assertExactKeys(value, ["type"], [], location);
      break;
    case "sessionEnded":
      assertExactKeys(value, ["outcome", "type"], ["reason"], location);
      if (!new Set(["completed", "failed", "cancelled", "shutdown"]).has(String(value.outcome))) {
        throw new ProtocolViolationError(`${location}.outcome is invalid`);
      }
      if (value.reason !== undefined && value.reason !== null && typeof value.reason !== "string") {
        throw new ProtocolViolationError(`${location}.reason must be a string or null`);
      }
      break;
    case "observationCaptured":
      assertExactKeys(value, ["observation", "type"], [], location);
      validateObservation(value.observation, `${location}.observation`);
      break;
    case "actionStarted":
      assertExactKeys(value, ["call", "type"], [], location);
      if (!isObject(value.call)) {
        throw new ProtocolViolationError(`${location}.call must be an object`);
      }
      assertRequiredKeys(value.call, ["id", "name"], `${location}.call`);
      uuid(value.call.id, `${location}.call.id`);
      stringValue(value.call.name, `${location}.call.name`);
      if (
        value.call.argumentsRedacted !== undefined &&
        typeof value.call.argumentsRedacted !== "boolean"
      ) {
        throw new ProtocolViolationError(`${location}.call.argumentsRedacted must be boolean`);
      }
      break;
    case "actionCompleted":
      assertExactKeys(value, ["callId", "outcome", "type"], [], location);
      uuid(value.callId, `${location}.callId`);
      validateActionOutcome(value.outcome, `${location}.outcome`);
      break;
    case "mediaStreamStarted": {
      assertExactKeys(value, ["stream", "type"], [], location);
      if (!isObject(value.stream)) {
        throw new ProtocolViolationError(`${location}.stream must be an object`);
      }
      assertExactKeys(
        value.stream,
        ["id", "kind", "mediaType"],
        ["viewport"],
        `${location}.stream`,
      );
      uuid(value.stream.id, `${location}.stream.id`);
      if (value.stream.kind !== "screenshot" && value.stream.kind !== "video") {
        throw new ProtocolViolationError(`${location}.stream.kind is invalid`);
      }
      stringValue(value.stream.mediaType, `${location}.stream.mediaType`);
      if (value.stream.viewport !== undefined) {
        validateMediaViewport(value.stream.viewport, `${location}.stream.viewport`);
      }
      break;
    }
    case "mediaFrameCaptured": {
      assertExactKeys(value, ["frame", "type"], [], location);
      if (!isObject(value.frame)) {
        throw new ProtocolViolationError(`${location}.frame must be an object`);
      }
      assertExactKeys(
        value.frame,
        ["streamId", "frameIndex", "evidence"],
        ["keyFrame", "durationMs"],
        `${location}.frame`,
      );
      uuid(value.frame.streamId, `${location}.frame.streamId`);
      safeUnsignedInteger(value.frame.frameIndex, `${location}.frame.frameIndex`, true);
      if (value.frame.keyFrame !== undefined && typeof value.frame.keyFrame !== "boolean") {
        throw new ProtocolViolationError(`${location}.frame.keyFrame must be boolean`);
      }
      if (value.frame.durationMs !== undefined) {
        safeUnsignedInteger(value.frame.durationMs, `${location}.frame.durationMs`);
      }
      validateAsset(value.frame.evidence, `${location}.frame.evidence`);
      break;
    }
    case "mediaStreamEnded":
      assertExactKeys(value, ["frameCount", "streamId", "type"], [], location);
      uuid(value.streamId, `${location}.streamId`);
      safeUnsignedInteger(value.frameCount, `${location}.frameCount`);
      break;
    case "verdictRecorded":
      assertExactKeys(value, ["type", "verdict"], [], location);
      if (!isObject(value.verdict)) {
        throw new ProtocolViolationError(`${location}.verdict must be an object`);
      }
      assertExactKeys(value.verdict, ["status", "summary"], ["evidence"], `${location}.verdict`);
      if (!new Set(["pass", "fail", "unknown"]).has(String(value.verdict.status))) {
        throw new ProtocolViolationError(`${location}.verdict.status is invalid`);
      }
      if (typeof value.verdict.summary !== "string") {
        throw new ProtocolViolationError(`${location}.verdict.summary must be a string`);
      }
      if (value.verdict.evidence !== undefined) {
        if (!Array.isArray(value.verdict.evidence)) {
          throw new ProtocolViolationError(`${location}.verdict.evidence must be an array`);
        }
        value.verdict.evidence.forEach((asset, index) =>
          validateAsset(asset, `${location}.verdict.evidence[${index}]`),
        );
      }
      break;
    case "error":
      assertExactKeys(value, ["error", "type"], [], location);
      validateErrorInfo(value.error, `${location}.error`);
      break;
    default:
      throw new ProtocolViolationError(`${location}.type is unknown`);
  }
}

function observationUsesProtocol15(value: unknown): boolean {
  return isObject(value) &&
    (Object.hasOwn(value, "uiSnapshot") || Object.hasOwn(value, "uiSnapshotOmission"));
}

function payloadUsesProtocol15(value: unknown): boolean {
  if (!isObject(value)) {
    return false;
  }
  if (value.type === "observationCaptured") {
    return observationUsesProtocol15(value.observation);
  }
  if (value.type !== "actionCompleted" || !isObject(value.outcome)) {
    return false;
  }
  if (value.outcome.outcome !== "succeeded" || !isObject(value.outcome.result)) {
    return false;
  }
  const result = value.outcome.result;
  return Object.hasOwn(result, "execution") ||
    observationUsesProtocol15(result.before) ||
    observationUsesProtocol15(result.after);
}

function validateTestEvent(
  value: unknown,
  location: string,
  selectedProtocol: ProtocolVersion,
): TestEvent {
  if (!isObject(value)) {
    throw new ProtocolViolationError(`${location} must be an object`);
  }
  assertExactKeys(
    value,
    ["atMs", "eventId", "payload", "sequence", "sessionId"],
    ["deviceId", "requestId"],
    location,
  );
  uuid(value.eventId, `${location}.eventId`);
  uuid(value.sessionId, `${location}.sessionId`);
  safeUnsignedInteger(value.atMs, `${location}.atMs`);
  safeUnsignedInteger(value.sequence, `${location}.sequence`, true);
  if (value.deviceId !== undefined && value.deviceId !== null && typeof value.deviceId !== "string") {
    throw new ProtocolViolationError(`${location}.deviceId must be a string or null`);
  }
  if (
    value.requestId !== undefined &&
    value.requestId !== null &&
    typeof value.requestId !== "string" &&
    !(
      typeof value.requestId === "number" &&
      Number.isSafeInteger(value.requestId) &&
      value.requestId >= 0
    )
  ) {
    throw new ProtocolViolationError(`${location}.requestId is invalid`);
  }
  validatePayload(value.payload, `${location}.payload`);
  if (
    selectedProtocol.minor < 4
    && isObject(value.payload)
    && MEDIA_PAYLOAD_TYPES.has(String(value.payload.type))
  ) {
    throw new ProtocolViolationError(
      `${location}.payload requires protocol 1.4 but the stream selected ${selectedProtocol.major}.${selectedProtocol.minor}`,
    );
  }
  if (selectedProtocol.minor < 5 && payloadUsesProtocol15(value.payload)) {
    throw new ProtocolViolationError(
      `${location}.payload requires protocol 1.5 but the stream selected ${selectedProtocol.major}.${selectedProtocol.minor}`,
    );
  }
  return value as unknown as TestEvent;
}

function validateEventNotification(
  value: unknown,
  bytes: number,
  sessionId: string,
  streamEpoch: string,
  subscriptionId: string,
  selectedProtocol: ProtocolVersion,
): ValidatedEvent {
  if (!isObject(value)) {
    throw new ProtocolViolationError("events.stream.event notification must be an object");
  }
  assertExactKeys(value, ["jsonrpc", "method", "params"], [], "events.stream.event notification");
  if (value.jsonrpc !== "2.0" || value.method !== "events.stream.event" || !isObject(value.params)) {
    throw new ProtocolViolationError("events.stream.event notification envelope is invalid");
  }
  assertExactKeys(
    value.params,
    ["cursor", "event", "subscriptionId"],
    [],
    "events.stream.event params",
  );
  if (value.params.subscriptionId !== subscriptionId) {
    throw new ProtocolViolationError("events.stream.event has an unexpected subscriptionId");
  }
  const cursor = validateCursor(value.params.cursor, "events.stream.event params.cursor");
  const event = validateTestEvent(
    value.params.event,
    "events.stream.event params.event",
    selectedProtocol,
  );
  if (
    cursor.sessionId !== sessionId ||
    cursor.streamEpoch !== streamEpoch ||
    event.sessionId !== sessionId ||
    event.sequence !== cursor.sequence
  ) {
    throw new EventStreamCursorError("events.stream.event cursor and event identity do not match");
  }
  return { bytes, cursor, event };
}

function validateTermination(value: unknown, location: string): EventsStreamTermination {
  if (!isObject(value) || typeof value.reason !== "string") {
    throw new ProtocolViolationError(`${location} must be a tagged object`);
  }
  if (value.reason === "sessionEnded" || value.reason === "cancelled") {
    assertExactKeys(value, ["reason"], [], location);
  } else if (
    new Set([
      "slowConsumer",
      "sessionDeleted",
      "serverShutdown",
      "sequenceGap",
      "eventTooLarge",
      "internalError",
    ]).has(value.reason)
  ) {
    assertExactKeys(value, ["error", "reason"], [], location);
    validateErrorInfo(value.error, `${location}.error`);
  } else {
    throw new ProtocolViolationError(`${location}.reason is unknown`);
  }
  return value as unknown as EventsStreamTermination;
}

function validateTerminalNotification(
  value: unknown,
  sessionId: string,
  streamEpoch: string,
  subscriptionId: string,
): EventsStreamTerminalParams {
  if (!isObject(value)) {
    throw new ProtocolViolationError("events.stream.terminal notification must be an object");
  }
  assertExactKeys(
    value,
    ["jsonrpc", "method", "params"],
    [],
    "events.stream.terminal notification",
  );
  if (
    value.jsonrpc !== "2.0" ||
    value.method !== "events.stream.terminal" ||
    !isObject(value.params)
  ) {
    throw new ProtocolViolationError("events.stream.terminal notification envelope is invalid");
  }
  assertExactKeys(
    value.params,
    ["sessionId", "subscriptionId", "termination"],
    ["lastEmittedCursor"],
    "events.stream.terminal params",
  );
  if (value.params.sessionId !== sessionId || value.params.subscriptionId !== subscriptionId) {
    throw new ProtocolViolationError("events.stream.terminal identifies another subscription");
  }
  const lastEmittedCursor =
    value.params.lastEmittedCursor === undefined || value.params.lastEmittedCursor === null
      ? undefined
      : validateCursor(
          value.params.lastEmittedCursor,
          "events.stream.terminal params.lastEmittedCursor",
        );
  if (
    lastEmittedCursor &&
    (lastEmittedCursor.sessionId !== sessionId || lastEmittedCursor.streamEpoch !== streamEpoch)
  ) {
    throw new EventStreamCursorError("terminal lastEmittedCursor belongs to another stream");
  }
  const result: EventsStreamTerminalParams = {
    sessionId,
    subscriptionId,
    termination: validateTermination(
      value.params.termination,
      "events.stream.terminal params.termination",
    ),
    ...(lastEmittedCursor ? { lastEmittedCursor } : {}),
  };
  return result;
}

function parseJsonMessage(data: unknown, maxMessageBytes: number): { bytes: number; value: unknown } {
  if (typeof data !== "string") {
    throw new ProtocolViolationError("event stream WebSocket messages must be UTF-8 text");
  }
  const bytes = Buffer.byteLength(data, "utf8");
  if (bytes > maxMessageBytes) {
    throw new ProtocolViolationError(
      `event stream WebSocket message is ${bytes} bytes; the limit is ${maxMessageBytes}`,
    );
  }
  let value: unknown;
  try {
    value = JSON.parse(data);
  } catch (cause) {
    throw new ProtocolViolationError("event stream WebSocket message is not valid JSON", { cause });
  }
  assertPureJsonValue(value);
  return { bytes, value };
}

function nativeWebSocketFactory(
  endpoint: string,
  subprotocol: "devicerail.events.v1",
): EventStreamWebSocket {
  return new WebSocket(endpoint, subprotocol) as unknown as EventStreamWebSocket;
}

function validateOriginPolicy(value: EventStreamOriginPolicy | undefined): EventStreamOriginPolicy {
  if (value === undefined) {
    return { kind: "absent" };
  }
  if (!isObject(value) || (value.kind !== "absent" && value.kind !== "exact")) {
    throw new ProtocolViolationError("event stream originPolicy is invalid");
  }
  if (value.kind === "absent") {
    assertExactKeys(value, ["kind"], [], "event stream originPolicy");
    return { kind: "absent" };
  }
  assertExactKeys(value, ["kind", "origin"], [], "event stream originPolicy");
  if (typeof value.origin !== "string" || value.origin.length === 0) {
    throw new ProtocolViolationError("event stream exact originPolicy requires an origin");
  }
  const prefix = value.origin.startsWith("http://127.0.0.1:")
    ? "http://127.0.0.1:"
    : value.origin.startsWith("https://127.0.0.1:")
      ? "https://127.0.0.1:"
      : undefined;
  const port = prefix ? value.origin.slice(prefix.length) : "";
  if (
    value.origin.length > 256 ||
    !/^[\u0000-\u007f]*$/u.test(value.origin) ||
    port.length === 0 ||
    (port.length > 1 && port.startsWith("0")) ||
    !/^[0-9]+$/u.test(port) ||
    Number(port) < 1 ||
    Number(port) > 65_535 ||
    (prefix === "http://127.0.0.1:" && port === "80") ||
    (prefix === "https://127.0.0.1:" && port === "443")
  ) {
    throw new ProtocolViolationError(
      "event stream exact originPolicy must contain only an http(s) origin",
    );
  }
  return { kind: "exact", origin: value.origin };
}

function normalizeSettings(options: EventStreamOptions): StreamSettings {
  return {
    maxMessageBytes: positiveSafeInteger(
      options.maxMessageBytes ?? DEFAULT_EVENT_STREAM_MAX_MESSAGE_BYTES,
      "maxMessageBytes",
    ),
    maxQueuedBytes: positiveSafeInteger(
      options.maxQueuedBytes ?? DEFAULT_EVENT_STREAM_MAX_QUEUED_BYTES,
      "maxQueuedBytes",
    ),
    maxQueuedEvents: positiveSafeInteger(
      options.maxQueuedEvents ?? DEFAULT_EVENT_STREAM_MAX_QUEUED_EVENTS,
      "maxQueuedEvents",
    ),
    originPolicy: validateOriginPolicy(options.originPolicy),
    webSocketFactory: options.webSocketFactory ?? nativeWebSocketFactory,
  };
}

function validateEventProtocolVersion(value: ProtocolVersion): ProtocolVersion {
  if (
    !isObject(value)
    || value.major !== 1
    || (value.minor !== 3 && value.minor !== 4 && value.minor !== 5)
    || !Object.hasOwn(value, "major")
    || !Object.hasOwn(value, "minor")
    || Object.keys(value).length !== 2
  ) {
    throw new ProtocolViolationError("event stream requires selected protocol 1.3, 1.4, or 1.5");
  }
  return { major: value.major, minor: value.minor };
}

async function openCapabilityWithAbort(
  capabilityOpener: EventStreamCapabilityOpener,
  sessionId: string,
  originPolicy: EventStreamOriginPolicy,
  signal: AbortSignal | undefined,
): Promise<EventsStreamOpenResult> {
  const pending = capabilityOpener(sessionId, originPolicy, signal);
  if (!signal) {
    return await pending;
  }
  return await new Promise<EventsStreamOpenResult>((resolve, reject) => {
    let settled = false;
    const finish = (operation: () => void): void => {
      if (settled) {
        return;
      }
      settled = true;
      signal.removeEventListener("abort", abort);
      operation();
    };
    const abort = (): void => finish(() => reject(new EventStreamAbortedError()));
    signal.addEventListener("abort", abort, { once: true });
    if (signal.aborted) {
      abort();
    }
    void pending.then(
      (result) => finish(() => resolve(result)),
      (error: unknown) =>
        finish(() =>
          reject(
            error instanceof Error
              ? error
              : new EventStreamClosedError(1006, "capability request failed"),
          ),
        ),
    );
  });
}

/**
 * A single resumable Session stream. Socket receipt, iterator delivery, and
 * application confirmation are deliberately separate cursor boundaries.
 */
export class DeviceRailEventStream implements AsyncIterable<EventStreamItem> {
  readonly #afterCursor: EventStreamCursor | undefined;
  readonly #capabilityOpener: EventStreamCapabilityOpener;
  readonly #clientPeer: PeerInfo;
  readonly #eventProtocol: ProtocolVersion;
  readonly #ready: Promise<void>;
  readonly #sessionId: string;
  readonly #signal: AbortSignal | undefined;
  readonly #settings: StreamSettings;
  readonly #socket: EventStreamWebSocket;
  readonly #streamEpoch: string;

  #abortListener: (() => void) | undefined;
  #confirmedCursor: EventStreamCursor | undefined;
  #deliveredSequence: number;
  #failure: Error | undefined;
  #iteratorClosed = false;
  #pendingNext: PendingNext | undefined;
  #phase: StreamPhase = "connecting";
  #queue: QueuedEvent[] = [];
  #queuedBytes = 0;
  #receivedCursor: EventStreamCursor | undefined;
  #receivedPayloadType: TestEvent["payload"]["type"] | undefined;
  #readyReject!: (error: Error) => void;
  #readyResolve!: () => void;
  #subscription: EventsSubscribeResult | undefined;
  #terminal: EventsStreamTerminalParams | undefined;

  readonly #onOpen = (): void => {
    if (this.#phase !== "connecting") {
      this.#fail(new ProtocolViolationError("event stream WebSocket opened more than once"));
      return;
    }
    if (this.#socket.protocol !== "devicerail.events.v1") {
      this.#fail(
        new ProtocolViolationError(
          "event stream WebSocket did not negotiate devicerail.events.v1",
        ),
      );
      return;
    }
    this.#phase = "awaitingHello";
    this.#send({
      id: HELLO_REQUEST_ID,
      jsonrpc: "2.0",
      method: "system.hello",
      params: {
        client: this.#clientPeer,
        features: { required: [EVENTS_STREAM_FEATURE] },
        protocol: {
          ranges: [{
            major: this.#eventProtocol.major,
            maxMinor: this.#eventProtocol.minor,
            minMinor: this.#eventProtocol.minor,
          }],
        },
      },
    });
  };

  readonly #onMessage = (event: EventStreamWebSocketEvent): void => {
    try {
      const message = parseJsonMessage(event.data, this.#settings.maxMessageBytes);
      if (this.#phase === "awaitingHello") {
        validateRpcResponse("system.hello", message.value);
        const result = parseResponseResult(
          message.value,
          HELLO_REQUEST_ID,
          "WebSocket system.hello response",
        );
        validateHelloResult(result, this.#eventProtocol);
        this.#phase = "awaitingSubscribe";
        this.#send({
          id: SUBSCRIBE_REQUEST_ID,
          jsonrpc: "2.0",
          method: "events.subscribe",
          params: {
            sessionId: this.#sessionId,
            ...(this.#afterCursor ? { afterCursor: this.#afterCursor } : {}),
          },
        });
        return;
      }
      if (this.#phase === "awaitingSubscribe") {
        validateRpcResponse("events.subscribe", message.value);
        const result = parseResponseResult(
          message.value,
          SUBSCRIBE_REQUEST_ID,
          "events.subscribe response",
        );
        this.#subscription = validateSubscribeResult(
          result,
          this.#sessionId,
          this.#streamEpoch,
          this.#afterCursor,
        );
        this.#phase = "streaming";
        this.#readyResolve();
        return;
      }
      if (this.#phase !== "streaming" || !this.#subscription) {
        throw new ProtocolViolationError("event stream received a message outside its active phase");
      }
      if (isObject(message.value) && message.value.method === "events.stream.event") {
        this.#acceptEvent(
          validateEventNotification(
            message.value,
            message.bytes,
            this.#sessionId,
            this.#streamEpoch,
            this.#subscription.subscriptionId,
            this.#eventProtocol,
          ),
        );
        return;
      }
      if (isObject(message.value) && message.value.method === "events.stream.terminal") {
        this.#acceptTerminal(
          validateTerminalNotification(
            message.value,
            this.#sessionId,
            this.#streamEpoch,
            this.#subscription.subscriptionId,
          ),
        );
        return;
      }
      throw new ProtocolViolationError("event stream received an unknown server message");
    } catch (cause) {
      this.#fail(
        cause instanceof Error
          ? cause
          : new ProtocolViolationError("event stream message processing failed"),
      );
    }
  };

  readonly #onClose = (event: EventStreamWebSocketEvent): void => {
    if (this.#phase === "finished") {
      return;
    }
    this.#fail(new EventStreamClosedError(event.code ?? 1006, event.reason ?? ""), false);
  };

  readonly #onError = (): void => {
    if (this.#phase !== "finished") {
      this.#fail(new EventStreamClosedError(1006, "WebSocket transport error"));
    }
  };

  constructor(
    capabilityOpener: EventStreamCapabilityOpener,
    clientPeer: PeerInfo,
    eventProtocol: ProtocolVersion,
    sessionId: string,
    afterCursor: EventStreamCursor | undefined,
    streamEpoch: string,
    socket: EventStreamWebSocket,
    settings: StreamSettings,
    signal?: AbortSignal,
  ) {
    this.#capabilityOpener = capabilityOpener;
    this.#clientPeer = { ...clientPeer };
    this.#eventProtocol = validateEventProtocolVersion(eventProtocol);
    this.#sessionId = sessionId;
    this.#signal = signal;
    this.#afterCursor = afterCursor ? cloneCursor(afterCursor) : undefined;
    this.#confirmedCursor = afterCursor ? cloneCursor(afterCursor) : undefined;
    this.#deliveredSequence = afterCursor?.sequence ?? 0;
    this.#streamEpoch = streamEpoch;
    this.#socket = socket;
    this.#settings = settings;
    this.#ready = new Promise<void>((resolve, reject) => {
      this.#readyResolve = resolve;
      this.#readyReject = reject;
    });
    socket.addEventListener("open", this.#onOpen);
    socket.addEventListener("message", this.#onMessage);
    socket.addEventListener("close", this.#onClose);
    socket.addEventListener("error", this.#onError);
    if (signal) {
      this.#abortListener = () => this.#abort();
      signal.addEventListener("abort", this.#abortListener, { once: true });
      if (signal.aborted) {
        this.#abort();
      }
    }
  }

  get confirmedCursor(): EventStreamCursor | undefined {
    return this.#confirmedCursor ? cloneCursor(this.#confirmedCursor) : undefined;
  }

  get receivedCursor(): EventStreamCursor | undefined {
    return this.#receivedCursor ? cloneCursor(this.#receivedCursor) : undefined;
  }

  get sessionId(): string {
    return this.#sessionId;
  }

  get terminal(): EventsStreamTerminalParams | undefined {
    return this.#terminal ? structuredClone(this.#terminal) : undefined;
  }

  [Symbol.asyncIterator](): AsyncIterator<EventStreamItem> {
    return this;
  }

  async next(): Promise<IteratorResult<EventStreamItem>> {
    if (this.#iteratorClosed) {
      return { done: true, value: undefined };
    }
    const queued = this.#queue.shift();
    if (queued) {
      this.#queuedBytes -= queued.bytes;
      this.#deliveredSequence = queued.item.cursor.sequence;
      return { done: false, value: queued.item };
    }
    if (this.#failure) {
      throw this.#failure;
    }
    if (this.#terminal) {
      if (this.#terminal.termination.reason === "sessionEnded") {
        return { done: true, value: undefined };
      }
      throw new EventStreamRemoteTerminationError(this.#terminal.termination);
    }
    if (this.#pendingNext) {
      throw new ProtocolViolationError("only one event stream next() call may be pending");
    }
    return await new Promise<IteratorResult<EventStreamItem>>((resolve, reject) => {
      this.#pendingNext = { reject, resolve };
    });
  }

  async return(): Promise<IteratorResult<EventStreamItem>> {
    this.#iteratorClosed = true;
    this.#queue = [];
    this.#queuedBytes = 0;
    const pending = this.#pendingNext;
    this.#pendingNext = undefined;
    pending?.resolve({ done: true, value: undefined });
    this.#abort();
    return { done: true, value: undefined };
  }

  /** Marks exactly one delivered event as durably accepted by the application. */
  confirm(cursor: EventStreamCursor): EventStreamCursor {
    const validated = validateCursor(cursor, "confirmed cursor");
    if (
      validated.sessionId !== this.#sessionId ||
      validated.streamEpoch !== this.#streamEpoch
    ) {
      throw new EventStreamCursorError("confirmed cursor belongs to another stream");
    }
    const expected = (this.#confirmedCursor?.sequence ?? 0) + 1;
    if (validated.sequence !== expected) {
      throw new EventStreamCursorError(
        `confirmed cursor sequence ${validated.sequence} is not the contiguous next sequence ${expected}`,
      );
    }
    if (validated.sequence > this.#deliveredSequence) {
      throw new EventStreamCursorError("an event must be delivered before it can be confirmed");
    }
    this.#confirmedCursor = cloneCursor(validated);
    return cloneCursor(validated);
  }

  /** Aborts this socket without advancing the confirmed application cursor. */
  cancel(): void {
    this.#abort();
  }

  /** Opens a fresh single-use capability from the last explicitly confirmed cursor. */
  async resume(options: EventStreamOptions = {}): Promise<DeviceRailEventStream> {
    if (this.#phase !== "finished") {
      throw new EventStreamCursorError("an active event stream cannot be resumed");
    }
    if (this.#queue.length > 0 || this.#pendingNext) {
      throw new EventStreamCursorError(
        "the previous event stream prefix must be drained before it can be resumed",
      );
    }
    return await connectEventStream(
      this.#capabilityOpener,
      this.#clientPeer,
      this.#eventProtocol,
      {
        sessionId: this.#sessionId,
        ...(this.#confirmedCursor ? { afterCursor: cloneCursor(this.#confirmedCursor) } : {}),
      },
      {
        maxMessageBytes: options.maxMessageBytes ?? this.#settings.maxMessageBytes,
        maxQueuedBytes: options.maxQueuedBytes ?? this.#settings.maxQueuedBytes,
        maxQueuedEvents: options.maxQueuedEvents ?? this.#settings.maxQueuedEvents,
        originPolicy: options.originPolicy ?? this.#settings.originPolicy,
        webSocketFactory: options.webSocketFactory ?? this.#settings.webSocketFactory,
        ...(options.signal ? { signal: options.signal } : {}),
      },
    );
  }

  async waitUntilReady(): Promise<void> {
    await this.#ready;
  }

  #acceptEvent(validated: ValidatedEvent): void {
    const previousSequence = this.#receivedCursor?.sequence ?? this.#afterCursor?.sequence ?? 0;
    if (previousSequence >= Number.MAX_SAFE_INTEGER || validated.cursor.sequence !== previousSequence + 1) {
      throw new EventStreamCursorError(
        `event stream sequence ${validated.cursor.sequence} does not follow ${previousSequence}`,
      );
    }
    this.#receivedCursor = cloneCursor(validated.cursor);
    this.#receivedPayloadType = validated.event.payload.type;
    const item: EventStreamItem = Object.freeze({
      cursor: cloneCursor(validated.cursor),
      event: validated.event,
      confirm: () => this.confirm(validated.cursor),
    });
    if (this.#pendingNext) {
      const pending = this.#pendingNext;
      this.#pendingNext = undefined;
      this.#deliveredSequence = validated.cursor.sequence;
      pending.resolve({ done: false, value: item });
      return;
    }
    if (
      this.#queue.length >= this.#settings.maxQueuedEvents ||
      this.#queuedBytes + validated.bytes > this.#settings.maxQueuedBytes
    ) {
      throw new EventStreamQueueOverflowError(
        this.#settings.maxQueuedEvents,
        this.#settings.maxQueuedBytes,
      );
    }
    this.#queue.push({ bytes: validated.bytes, item });
    this.#queuedBytes += validated.bytes;
  }

  #acceptTerminal(terminal: EventsStreamTerminalParams): void {
    const emitted = terminal.lastEmittedCursor;
    if (
      (emitted === undefined) !== (this.#receivedCursor === undefined) ||
      (emitted && this.#receivedCursor && !sameCursor(emitted, this.#receivedCursor))
    ) {
      throw new EventStreamCursorError(
        "terminal lastEmittedCursor does not equal the continuous received prefix",
      );
    }
    if (terminal.termination.reason === "sessionEnded") {
      const subscription = this.#subscription;
      if (!subscription) {
        throw new ProtocolViolationError("normal event stream termination preceded subscription");
      }
      const continuousSequence =
        this.#receivedCursor?.sequence ?? this.#afterCursor?.sequence ?? 0;
      if (continuousSequence < subscription.replayThrough.sequence) {
        throw new EventStreamCursorError(
          `normal event stream termination stopped at sequence ${continuousSequence} before replay boundary ${subscription.replayThrough.sequence}`,
        );
      }
      const replayWasAlreadyConfirmed =
        subscription.sessionState === "ended" &&
        this.#receivedCursor === undefined &&
        this.#afterCursor !== undefined &&
        sameCursor(this.#afterCursor, subscription.replayThrough);
      if (
        !replayWasAlreadyConfirmed &&
        this.#receivedPayloadType !== "sessionEnded"
      ) {
        throw new ProtocolViolationError(
          "a subscription that receives events through Session end must receive sessionEnded as its final event before normal termination",
        );
      }
    }
    this.#terminal = terminal;
    this.#phase = "finished";
    this.#detachAbort();
    this.#detachSocket();
    this.#settlePendingNext();
    try {
      this.#socket.close(1000, "terminal received");
    } catch {
      // The typed terminal remains authoritative if the close handshake fails.
    }
  }

  #abort(): void {
    if (this.#phase === "finished") {
      return;
    }
    this.#queue = [];
    this.#queuedBytes = 0;
    this.#fail(new EventStreamAbortedError(), true, 1000, "client aborted");
  }

  #detachAbort(): void {
    if (this.#abortListener) {
      this.#signal?.removeEventListener("abort", this.#abortListener);
    }
    this.#abortListener = undefined;
  }

  #detachSocket(): void {
    this.#socket.removeEventListener("open", this.#onOpen);
    this.#socket.removeEventListener("message", this.#onMessage);
    this.#socket.removeEventListener("close", this.#onClose);
    this.#socket.removeEventListener("error", this.#onError);
  }

  #fail(
    error: Error,
    closeSocket = true,
    closeCode = 1002,
    closeReason = "protocol violation",
  ): void {
    if (this.#phase === "finished") {
      return;
    }
    this.#failure = error;
    this.#phase = "finished";
    this.#readyReject(error);
    this.#detachAbort();
    this.#detachSocket();
    this.#settlePendingNext();
    if (closeSocket) {
      try {
        this.#socket.close(closeCode, closeReason);
      } catch {
        // Preserve the first explicit stream failure.
      }
    }
  }

  #send(value: Record<string, unknown>): void {
    try {
      this.#socket.send(JSON.stringify(value));
    } catch (cause) {
      this.#fail(
        new EventStreamClosedError(
          1006,
          cause instanceof Error ? cause.message : "WebSocket send failed",
        ),
      );
    }
  }

  #settlePendingNext(): void {
    if (!this.#pendingNext || this.#queue.length > 0) {
      return;
    }
    const pending = this.#pendingNext;
    this.#pendingNext = undefined;
    if (this.#failure) {
      pending.reject(this.#failure);
    } else if (this.#terminal?.termination.reason === "sessionEnded") {
      pending.resolve({ done: true, value: undefined });
    } else if (this.#terminal) {
      pending.reject(new EventStreamRemoteTerminationError(this.#terminal.termination));
    }
  }
}

export async function connectEventStream(
  capabilityOpener: EventStreamCapabilityOpener,
  clientPeer: PeerInfo,
  eventProtocol: ProtocolVersion,
  params: EventsSubscribeParams,
  options: EventStreamOptions,
): Promise<DeviceRailEventStream> {
  if (options.signal?.aborted) {
    throw new RequestAbortedError();
  }
  const selectedProtocol = validateEventProtocolVersion(eventProtocol);
  if (!isObject(params)) {
    throw new ProtocolViolationError("event stream params must be an object");
  }
  assertExactKeys(params, ["sessionId"], ["afterCursor"], "event stream params");
  const sessionId = uuid(params.sessionId, "event stream params.sessionId");
  const afterCursor =
    params.afterCursor === undefined || params.afterCursor === null
      ? undefined
      : validateCursor(params.afterCursor, "event stream params.afterCursor");
  if (afterCursor && afterCursor.sessionId !== sessionId) {
    throw new EventStreamCursorError("afterCursor belongs to another session");
  }
  const settings = normalizeSettings(options);
  const capability = validateOpenResult(
    await openCapabilityWithAbort(
      capabilityOpener,
      sessionId,
      settings.originPolicy,
      options.signal,
    ),
  );
  if (afterCursor && afterCursor.streamEpoch !== capability.streamEpoch) {
    throw new EventStreamCursorError("afterCursor belongs to another daemon stream epoch");
  }
  if (options.signal?.aborted) {
    throw new EventStreamAbortedError();
  }
  let socket: EventStreamWebSocket;
  try {
    socket = settings.webSocketFactory(capability.endpoint, "devicerail.events.v1");
  } catch (cause) {
    throw new EventStreamClosedError(
      1006,
      cause instanceof Error ? cause.message : "WebSocket construction failed",
    );
  }
  const stream = new DeviceRailEventStream(
    capabilityOpener,
    clientPeer,
    selectedProtocol,
    sessionId,
    afterCursor,
    capability.streamEpoch,
    socket,
    settings,
    options.signal,
  );
  await stream.waitUntilReady();
  return stream;
}
