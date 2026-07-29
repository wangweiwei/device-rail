import { randomUUID } from "node:crypto";
import { spawn } from "node:child_process";
import type { ChildProcessWithoutNullStreams, SpawnOptionsWithoutStdio } from "node:child_process";
import type { Readable, Writable } from "node:stream";

import type {
  HelloParams,
  HelloResult,
  EventsSubscribeParams,
  PeerInfo,
  RequestCancelResult,
  RpcError,
  RpcId,
  RpcMethod,
  RpcParamsFor,
  RpcResultFor,
  RpcSupportsTimeout,
} from "@devicerail/protocol";

import {
  FeatureNotNegotiatedError,
  EventStreamAbortedError,
  HandshakeStateError,
  PendingRequestLimitError,
  ProtocolViolationError,
  RequestAbortedError,
  RpcRemoteError,
  TransportClosedError,
  WriteFrameTooLargeError,
  WriteQueueOverflowError,
} from "./errors.js";
import { DEFAULT_MAX_FRAME_BYTES, NdjsonDecoder } from "./ndjson.js";
import { BoundedStderrTail, DEFAULT_STDERR_TAIL_BYTES } from "./stderr-tail.js";
import { NdjsonWriteQueue } from "./write-queue.js";
import { assertPureJsonValue, validateRpcResponse } from "./rpc-response-validator.js";
import {
  DeviceRailEventStream,
  connectEventStream,
  type EventStreamOptions,
} from "./event-stream.js";

const REQUEST_CONTROL_FEATURE = "request.control.v1";
const ROUTING_FEATURE = "device.routing.v1";
const EVENTS_FEATURE = "events.snapshot.v1";
const ACTION_PROTECTED_FEATURE = "action.protected.v1";
const EVENTS_STREAM_FEATURE = "events.stream.v1";
const MEDIA_STREAM_FEATURE = "media.stream.v1";
const SESSION_EXPORT_PAGE_FEATURE = "session.export.page.v1";
const UI_SNAPSHOT_FEATURE = "observation.uiSnapshot.v1";
const SEMANTIC_ACTIONS_FEATURE = "device.semanticActions.v1";
const VERDICT_RECORD_FEATURE = "verdict.record.v1";
const SUPPORTED_PROTOCOL_MAJOR = 1;
const SUPPORTED_PROTOCOL_MAX_MINOR = 5;

const semanticActionNames = new Set([
  "findElement",
  "tapElement",
  "clearElement",
  "setElementValue",
  "waitForElement",
]);

export type ClientRpcMethod = Exclude<RpcMethod, "events.subscribe" | "system.hello">;

const timeoutMethods = new Set<ClientRpcMethod>([
  "device.capabilities",
  "device.connect",
  "device.disconnect",
  "device.execute",
  "device.observe",
  "media.stream.capture",
]);

const requiredFeatures: Partial<Record<ClientRpcMethod, string>> = {
  "device.select": ROUTING_FEATURE,
  "devices.list": ROUTING_FEATURE,
  "events.clear": EVENTS_FEATURE,
  "events.list": EVENTS_FEATURE,
  "events.stream.open": EVENTS_STREAM_FEATURE,
  "media.stream.capture": MEDIA_STREAM_FEATURE,
  "media.stream.end": MEDIA_STREAM_FEATURE,
  "media.stream.start": MEDIA_STREAM_FEATURE,
  "request.cancel": REQUEST_CONTROL_FEATURE,
  "session.export": EVENTS_FEATURE,
  "sessions.list": EVENTS_FEATURE,
  "ui.snapshot.get": UI_SNAPSHOT_FEATURE,
  "verdict.record": VERDICT_RECORD_FEATURE,
};

export type ClientState =
  | "awaitingHello"
  | "helloInFlight"
  | "ready"
  | "closing"
  | "closed"
  | "failed";

export interface TransportClosure {
  readonly code: number | null;
  readonly error?: Error;
  readonly signal: NodeJS.Signals | null;
}

export interface ClientTransport {
  readonly closed: Promise<TransportClosure>;
  readonly readable: Readable;
  readonly stderr?: Readable;
  readonly writable: Writable;
  closeInput(): void;
  terminate?(): void;
}

export interface SpawnClientOptions {
  readonly args?: readonly string[];
  readonly closeGraceMs?: number;
  readonly command: string;
  readonly hello: HelloParams;
  readonly maxFrameBytes?: number;
  readonly maxPendingRequests?: number;
  readonly maxQueuedBytes?: number;
  readonly maxQueuedFrames?: number;
  readonly spawn?: Omit<SpawnOptionsWithoutStdio, "shell" | "stdio">;
  readonly stderrTailBytes?: number;
}

export interface AttachedClientOptions {
  readonly closeGraceMs?: number;
  readonly maxFrameBytes?: number;
  readonly maxPendingRequests?: number;
  readonly maxQueuedBytes?: number;
  readonly maxQueuedFrames?: number;
  readonly stderrTailBytes?: number;
}

type TimeoutCallOptions<M extends ClientRpcMethod> = RpcSupportsTimeout<M> extends true
  ? { readonly signal?: AbortSignal; readonly timeoutMs?: number }
  : { readonly signal?: never; readonly timeoutMs?: never };

export type CallOptions<M extends ClientRpcMethod> = TimeoutCallOptions<M>;

export type CallArguments<M extends ClientRpcMethod> = undefined extends RpcParamsFor<M>
  ? [params?: RpcParamsFor<M>, options?: CallOptions<M>]
  : [params: RpcParamsFor<M>, options?: CallOptions<M>];

export interface RequestHandle<Result> {
  readonly id: RpcId;
  readonly result: Promise<Result>;
  cancel(): Promise<RequestCancelResult>;
}

interface PendingRequest {
  readonly application: boolean;
  readonly method: RpcMethod;
  readonly reject: (error: Error) => void;
  readonly resolve: (result: unknown) => void;
}

type ParsedResponse =
  | {
      readonly envelope: Readonly<Record<string, unknown>>;
      readonly error: RpcError;
      readonly id: RpcId;
      readonly kind: "error";
    }
  | {
      readonly envelope: Readonly<Record<string, unknown>>;
      readonly id: RpcId;
      readonly kind: "result";
      readonly result: unknown;
    };

interface HelloOfferSnapshot {
  readonly client: PeerInfo;
  readonly offeredFeatures: ReadonlySet<string>;
  readonly ranges: readonly {
    readonly major: number;
    readonly maxMinor: number;
    readonly minMinor: number;
  }[];
  readonly requiredFeatures: ReadonlySet<string>;
}

function positiveSafeInteger(value: number, name: string): number {
  if (!Number.isSafeInteger(value) || value <= 0) {
    throw new RangeError(`${name} must be a positive safe integer`);
  }
  return value;
}

function rpcIdKey(id: RpcId): string {
  return `${typeof id}:${String(id)}`;
}

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

function parseRpcId(value: unknown, location = "response id"): RpcId {
  if (typeof value === "string") {
    return value;
  }
  if (typeof value === "number" && Number.isSafeInteger(value) && value >= 0) {
    return value;
  }
  throw new ProtocolViolationError(`${location} must be a string or non-negative safe integer`);
}

function parseRpcError(value: unknown): RpcError {
  if (!isObject(value)) {
    throw new ProtocolViolationError("response error must be an object");
  }
  assertExactKeys(value, ["code", "data", "message"], [], "response error");
  const { code, data, message } = value;
  if (
    typeof code !== "number" ||
    !Number.isInteger(code) ||
    code < -2_147_483_648 ||
    code > 2_147_483_647
  ) {
    throw new ProtocolViolationError("response error.code must be a signed 32-bit integer");
  }
  if (typeof message !== "string" || !isObject(data)) {
    throw new ProtocolViolationError("response error message/data are invalid");
  }
  assertExactKeys(data, ["code", "message", "retryable"], ["details"], "response error.data");
  if (
    typeof data.code !== "string" ||
    typeof data.message !== "string" ||
    typeof data.retryable !== "boolean"
  ) {
    throw new ProtocolViolationError("response error.data is invalid");
  }
  return value as unknown as RpcError;
}

function parseResponse(line: string): ParsedResponse {
  let value: unknown;
  try {
    value = JSON.parse(line);
  } catch (cause) {
    throw new ProtocolViolationError("response frame is not valid JSON", { cause });
  }
  if (!isObject(value) || value.jsonrpc !== "2.0") {
    throw new ProtocolViolationError('response must be a JSON-RPC 2.0 object');
  }
  const id = parseRpcId(value.id);
  const hasResult = Object.hasOwn(value, "result");
  const hasError = Object.hasOwn(value, "error");
  if (hasResult === hasError) {
    throw new ProtocolViolationError("response must contain exactly one of result or error");
  }
  if (hasError) {
    assertExactKeys(value, ["error", "id", "jsonrpc"], [], "response");
    return { envelope: value, error: parseRpcError(value.error), id, kind: "error" };
  }
  assertExactKeys(value, ["id", "jsonrpc", "result"], [], "response");
  return { envelope: value, id, kind: "result", result: value.result };
}

function serializeRequest(request: Record<string, unknown>): string {
  try {
    const serialized = JSON.stringify(request, (_key, value: unknown) => {
      if (
        typeof value === "number" &&
        (!Number.isFinite(value) || (Number.isInteger(value) && !Number.isSafeInteger(value)))
      ) {
        throw new ProtocolViolationError("request contains an unsafe JSON number");
      }
      return value;
    });
    if (serialized === undefined) {
      throw new ProtocolViolationError("request is not JSON serializable");
    }
    assertPureJsonValue(JSON.parse(serialized));
    return serialized;
  } catch (cause) {
    if (cause instanceof ProtocolViolationError) {
      throw cause;
    }
    throw new ProtocolViolationError("request is not JSON serializable", { cause });
  }
}

function stringSet(value: unknown, location: string): ReadonlySet<string> {
  if (!Array.isArray(value) || value.some((item) => typeof item !== "string")) {
    throw new ProtocolViolationError(`${location} must be an array of strings`);
  }
  const result = new Set(value as string[]);
  if (result.size !== value.length) {
    throw new ProtocolViolationError(`${location} must contain unique values`);
  }
  return result;
}

function uint16(value: unknown, location: string): number {
  if (typeof value !== "number" || !Number.isInteger(value) || value < 0 || value > 65_535) {
    throw new ProtocolViolationError(`${location} must be an unsigned 16-bit integer`);
  }
  return value;
}

function snapshotHelloOffer(params: HelloParams): HelloOfferSnapshot {
  if (!isObject(params)) {
    throw new ProtocolViolationError("system.hello params must be an object");
  }
  assertExactKeys(params, ["client", "protocol"], ["features"], "system.hello params");
  if (!isObject(params.client)) {
    throw new ProtocolViolationError("system.hello params.client must be an object");
  }
  assertExactKeys(params.client, ["name", "version"], [], "system.hello params.client");
  if (typeof params.client.name !== "string" || typeof params.client.version !== "string") {
    throw new ProtocolViolationError("system.hello client name/version must be strings");
  }
  if (!isObject(params.protocol)) {
    throw new ProtocolViolationError("system.hello params.protocol must be an object");
  }
  assertExactKeys(params.protocol, ["ranges"], [], "system.hello params.protocol");
  if (!Array.isArray(params.protocol.ranges) || params.protocol.ranges.length === 0) {
    throw new ProtocolViolationError("system.hello protocol.ranges must be a non-empty array");
  }
  const ranges = params.protocol.ranges.map((candidate, index) => {
    if (!isObject(candidate)) {
      throw new ProtocolViolationError(`system.hello protocol.ranges[${index}] must be an object`);
    }
    assertExactKeys(
      candidate,
      ["major", "maxMinor", "minMinor"],
      [],
      `system.hello protocol.ranges[${index}]`,
    );
    const major = uint16(candidate.major, `system.hello protocol.ranges[${index}].major`);
    const maxMinor = uint16(
      candidate.maxMinor,
      `system.hello protocol.ranges[${index}].maxMinor`,
    );
    const minMinor = uint16(
      candidate.minMinor,
      `system.hello protocol.ranges[${index}].minMinor`,
    );
    if (minMinor > maxMinor) {
      throw new ProtocolViolationError(
        `system.hello protocol.ranges[${index}] has minMinor greater than maxMinor`,
      );
    }
    if (major !== SUPPORTED_PROTOCOL_MAJOR || maxMinor > SUPPORTED_PROTOCOL_MAX_MINOR) {
      throw new ProtocolViolationError(
        `system.hello protocol.ranges[${index}] exceeds client support for protocol 1.0-1.5`,
      );
    }
    return { major, maxMinor, minMinor };
  });

  let requiredFeatures: ReadonlySet<string> = new Set();
  let optionalFeatures: ReadonlySet<string> = new Set();
  if (params.features !== undefined) {
    if (!isObject(params.features)) {
      throw new ProtocolViolationError("system.hello params.features must be an object");
    }
    assertExactKeys(params.features, [], ["optional", "required"], "system.hello params.features");
    requiredFeatures =
      params.features.required === undefined
        ? new Set()
        : stringSet(params.features.required, "system.hello params.features.required");
    optionalFeatures =
      params.features.optional === undefined
        ? new Set()
        : stringSet(params.features.optional, "system.hello params.features.optional");
  }
  return {
    client: { name: params.client.name, version: params.client.version },
    offeredFeatures: new Set([...requiredFeatures, ...optionalFeatures]),
    ranges,
    requiredFeatures,
  };
}

function validateHelloResult(value: unknown, offer: HelloOfferSnapshot): HelloResult {
  if (!isObject(value)) {
    throw new ProtocolViolationError("system.hello result must be an object");
  }
  assertExactKeys(
    value,
    ["connectionId", "features", "protocol", "server", "transport"],
    [],
    "system.hello result",
  );
  if (
    typeof value.connectionId !== "string" ||
    !/^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/iu.test(
      value.connectionId,
    )
  ) {
    throw new ProtocolViolationError("system.hello result.connectionId must be a UUID");
  }
  if (!isObject(value.features)) {
    throw new ProtocolViolationError("system.hello result.features must be an object");
  }
  assertExactKeys(value.features, ["enabled"], [], "system.hello result.features");
  const enabled = stringSet(value.features.enabled, "system.hello result.features.enabled");
  for (const feature of offer.requiredFeatures) {
    if (!enabled.has(feature)) {
      throw new ProtocolViolationError(`system.hello omitted required feature ${feature}`);
    }
  }
  for (const feature of enabled) {
    if (!offer.offeredFeatures.has(feature)) {
      throw new ProtocolViolationError(`system.hello enabled unoffered feature ${feature}`);
    }
  }
  if (!isObject(value.protocol)) {
    throw new ProtocolViolationError("system.hello result.protocol must be an object");
  }
  assertExactKeys(value.protocol, ["selected"], [], "system.hello result.protocol");
  if (!isObject(value.protocol.selected)) {
    throw new ProtocolViolationError("system.hello result.protocol.selected must be an object");
  }
  assertExactKeys(
    value.protocol.selected,
    ["major", "minor"],
    [],
    "system.hello result.protocol.selected",
  );
  const selectedMajor = uint16(
    value.protocol.selected.major,
    "system.hello result.protocol.selected.major",
  );
  const selectedMinor = uint16(
    value.protocol.selected.minor,
    "system.hello result.protocol.selected.minor",
  );
  if (
    !offer.ranges.some(
      (range) =>
        range.major === selectedMajor &&
        range.minMinor <= selectedMinor &&
        selectedMinor <= range.maxMinor,
    )
  ) {
    throw new ProtocolViolationError("system.hello selected a protocol outside the client offer");
  }
  if (
    selectedMajor !== SUPPORTED_PROTOCOL_MAJOR ||
    selectedMinor > SUPPORTED_PROTOCOL_MAX_MINOR
  ) {
    throw new ProtocolViolationError("system.hello selected a protocol unsupported by this client");
  }
  if (enabled.has(REQUEST_CONTROL_FEATURE) && selectedMinor < 1) {
    throw new ProtocolViolationError(
      `system.hello enabled ${REQUEST_CONTROL_FEATURE} before protocol 1.1`,
    );
  }
  if (enabled.has(ROUTING_FEATURE) && selectedMinor < 2) {
    throw new ProtocolViolationError(`system.hello enabled ${ROUTING_FEATURE} before protocol 1.2`);
  }
  if (enabled.has(ACTION_PROTECTED_FEATURE) && selectedMinor < 2) {
    throw new ProtocolViolationError(
      `system.hello enabled ${ACTION_PROTECTED_FEATURE} before protocol 1.2`,
    );
  }
  if (enabled.has(EVENTS_STREAM_FEATURE) && selectedMinor < 3) {
    throw new ProtocolViolationError(
      `system.hello enabled ${EVENTS_STREAM_FEATURE} before protocol 1.3`,
    );
  }
  if (enabled.has(MEDIA_STREAM_FEATURE) && selectedMinor < 4) {
    throw new ProtocolViolationError(
      `system.hello enabled ${MEDIA_STREAM_FEATURE} before protocol 1.4`,
    );
  }
  if (enabled.has(SESSION_EXPORT_PAGE_FEATURE) && selectedMinor < 4) {
    throw new ProtocolViolationError(
      `system.hello enabled ${SESSION_EXPORT_PAGE_FEATURE} before protocol 1.4`,
    );
  }
  for (const featureName of [
    UI_SNAPSHOT_FEATURE,
    SEMANTIC_ACTIONS_FEATURE,
    VERDICT_RECORD_FEATURE,
  ]) {
    if (enabled.has(featureName) && selectedMinor < 5) {
      throw new ProtocolViolationError(
        `system.hello enabled ${featureName} before protocol 1.5`,
      );
    }
  }
  if (!isObject(value.server)) {
    throw new ProtocolViolationError("system.hello result.server must be an object");
  }
  assertExactKeys(value.server, ["name", "version"], [], "system.hello result.server");
  if (typeof value.server.name !== "string" || typeof value.server.version !== "string") {
    throw new ProtocolViolationError("system.hello server name/version must be strings");
  }
  if (!isObject(value.transport)) {
    throw new ProtocolViolationError("system.hello result.transport must be an object");
  }
  assertExactKeys(value.transport, ["framing", "kind"], [], "system.hello result.transport");
  if (value.transport.kind !== "stdio" || value.transport.framing !== "ndjson") {
    throw new ProtocolViolationError("system.hello negotiated a non-stdio/ndjson transport");
  }
  return value as unknown as HelloResult;
}

function processTransport(child: ChildProcessWithoutNullStreams): ClientTransport {
  let spawnError: Error | undefined;
  const closed = new Promise<TransportClosure>((resolve) => {
    child.once("error", (error) => {
      spawnError = error;
    });
    child.once("close", (code, signal) => {
      resolve(spawnError ? { code, error: spawnError, signal } : { code, signal });
    });
  });
  return {
    closed,
    readable: child.stdout,
    stderr: child.stderr,
    writable: child.stdin,
    closeInput: () => child.stdin.end(),
    terminate: () => {
      child.kill();
    },
  };
}

export class DeviceRailClient {
  readonly #closeGraceMs: number;
  readonly #decoder: NdjsonDecoder;
  readonly #maxApplicationRequests: number;
  readonly #maxAbandonedRequests: number;
  readonly #maxPendingRequests: number;
  readonly #pending = new Map<string, PendingRequest>();
  readonly #requestPrefix = randomUUID();
  readonly #stderr: BoundedStderrTail;
  readonly #transport: ClientTransport;
  readonly #transportClosure: Promise<TransportClosure>;
  readonly #writes: NdjsonWriteQueue;

  #applicationPending = 0;
  #abandonedRequests = new Map<string, RpcMethod>();
  #automaticCancelTargets = new Map<string, RpcId>();
  #cancelBacklog = new Map<string, RpcId>();
  #cancelProgressWaitScheduled = false;
  #closePromise: Promise<void> | undefined;
  #enabledFeatures = new Set<string>();
  #helloClient: PeerInfo | undefined;
  #helloProtocol: HelloResult["protocol"]["selected"] | undefined;
  #nextRequest = 1n;
  #readableEnded = false;
  #state: ClientState = "awaitingHello";
  #terminalError: Error | undefined;
  #transportErrorsGuarded = false;
  readonly #ignoreLateTransportError = (): void => {};

  readonly #onReadableData = (chunk: Buffer | string): void => {
    try {
      const bytes = typeof chunk === "string" ? Buffer.from(chunk) : chunk;
      for (const line of this.#decoder.push(bytes)) {
        if (line.length === 0) {
          throw new ProtocolViolationError("response stream contains an empty NDJSON frame");
        }
        this.#acceptResponse(parseResponse(line));
      }
    } catch (cause) {
      this.#fail(cause instanceof Error ? cause : new ProtocolViolationError("response failed"));
    }
  };
  readonly #onReadableEnd = (): void => {
    try {
      this.#decoder.end();
    } catch (cause) {
      this.#fail(cause instanceof Error ? cause : new ProtocolViolationError("response ended"));
      return;
    }
    this.#readableEnded = true;
    if (
      this.#state !== "failed" &&
      this.#state !== "closed" &&
      !(this.#state === "closing" && this.#pending.size === 0)
    ) {
      const diagnostic = this.#stderr.text.trim();
      this.#fail(
        new TransportClosedError(
          `response stream ended before the client completed${diagnostic ? `; stderr: ${diagnostic}` : ""}`,
        ),
      );
    }
  };
  readonly #onReadableClose = (): void => {
    if (!this.#readableEnded && this.#state !== "closed" && this.#state !== "failed") {
      this.#fail(new TransportClosedError("response stream closed before a clean EOF"));
    }
  };
  readonly #onReadableError = (cause: Error): void => {
    this.#fail(new TransportClosedError("response stream failed", { cause }));
  };

  constructor(transport: ClientTransport, options: AttachedClientOptions = {}) {
    const maxFrameBytes = positiveSafeInteger(
      options.maxFrameBytes ?? DEFAULT_MAX_FRAME_BYTES,
      "maxFrameBytes",
    );
    const maxPendingRequests = positiveSafeInteger(
      options.maxPendingRequests ?? 256,
      "maxPendingRequests",
    );
    if (maxPendingRequests < 2) {
      throw new RangeError("maxPendingRequests must be at least 2 to reserve request.cancel capacity");
    }
    const closeGraceMs = positiveSafeInteger(options.closeGraceMs ?? 7_000, "closeGraceMs");
    const stderrTailBytes = positiveSafeInteger(
      options.stderrTailBytes ?? DEFAULT_STDERR_TAIL_BYTES,
      "stderrTailBytes",
    );
    this.#transport = transport;
    this.#decoder = new NdjsonDecoder({ maxFrameBytes });
    this.#maxPendingRequests = maxPendingRequests;
    this.#maxAbandonedRequests = maxPendingRequests;
    const cancellationReserve = Math.min(16, Math.max(1, Math.floor(maxPendingRequests / 4)));
    this.#maxApplicationRequests = maxPendingRequests - cancellationReserve;
    this.#closeGraceMs = closeGraceMs;
    this.#writes = new NdjsonWriteQueue(transport.writable, {
      maxFrameBytes,
      ...(options.maxQueuedBytes === undefined
        ? {}
        : { maxQueuedBytes: options.maxQueuedBytes }),
      ...(options.maxQueuedFrames === undefined
        ? {}
        : { maxQueuedFrames: options.maxQueuedFrames }),
    });
    void this.#writes.failure.then((error) => this.#fail(error));
    this.#stderr = new BoundedStderrTail(transport.stderr, stderrTailBytes);
    this.#transportClosure = transport.closed.then(
      (closure) => {
        this.#transportClosed(closure);
        return closure;
      },
      (cause: unknown) => {
        const error = new TransportClosedError("transport closure promise rejected", {
          cause,
        });
        this.#fail(error);
        throw error;
      },
    );
    void this.#transportClosure.catch(() => {});

    transport.readable.on("data", this.#onReadableData);
    transport.readable.once("end", this.#onReadableEnd);
    transport.readable.once("close", this.#onReadableClose);
    transport.readable.once("error", this.#onReadableError);
  }

  static async spawn(options: SpawnClientOptions): Promise<DeviceRailClient> {
    let child: ChildProcessWithoutNullStreams | undefined;
    let client: DeviceRailClient | undefined;
    try {
      child = spawn(options.command, [...(options.args ?? [])], {
        ...options.spawn,
        shell: false,
        stdio: ["pipe", "pipe", "pipe"],
      });
      client = new DeviceRailClient(processTransport(child), options);
      await client.hello(options.hello);
      return client;
    } catch (error) {
      if (client) {
        await client.close().catch(() => {});
      } else if (child) {
        try {
          child.stdin.end();
        } catch {
          // Best-effort cleanup while preserving the original construction error.
        }
        try {
          child.kill();
        } catch {
          // Best-effort cleanup while preserving the original construction error.
        }
      }
      throw error;
    }
  }

  get enabledFeatures(): ReadonlySet<string> {
    return new Set(this.#enabledFeatures);
  }

  get pendingRequests(): number {
    return this.#pending.size;
  }

  get state(): ClientState {
    return this.#state;
  }

  get stderrTail(): string {
    return this.#stderr.text;
  }

  async hello(params: HelloParams): Promise<HelloResult> {
    if (this.#state !== "awaitingHello") {
      throw new HandshakeStateError(`system.hello is not allowed while client is ${this.#state}`);
    }
    const offer = snapshotHelloOffer(params);
    this.#state = "helloInFlight";
    try {
      const rawResult = await this.#start<"system.hello">(
        "system.hello",
        params,
        undefined,
        true,
        false,
      ).result;
      let result: HelloResult;
      try {
        result = validateHelloResult(rawResult, offer);
      } catch (cause) {
        const error =
          cause instanceof Error
            ? cause
            : new ProtocolViolationError("system.hello returned an invalid result");
        this.#fail(error);
        throw error;
      }
      this.#enabledFeatures = new Set(result.features.enabled);
      this.#helloClient = offer.client;
      this.#helloProtocol = { ...result.protocol.selected };
      if (this.#state === "helloInFlight") {
        this.#state = "ready";
      } else if (this.#state === "failed" || this.#state === "closed") {
        throw this.#terminalError ?? new TransportClosedError();
      }
      return result;
    } catch (error) {
      if (this.#state === "helloInFlight") {
        this.#state = "awaitingHello";
      }
      throw error;
    }
  }

  async call<M extends ClientRpcMethod>(
    method: M,
    ...args: CallArguments<M>
  ): Promise<RpcResultFor<M>> {
    return await this.beginCall(method, ...args).result;
  }

  async openEventStream(
    params: EventsSubscribeParams,
    options: EventStreamOptions = {},
  ): Promise<DeviceRailEventStream> {
    if (this.#state !== "ready" || !this.#helloClient || !this.#helloProtocol) {
      throw new HandshakeStateError(
        `events.stream.open is not allowed while client is ${this.#state}`,
      );
    }
    return await connectEventStream(
      async (sessionId, originPolicy, signal) => {
        const handle = this.beginCall("events.stream.open", {
          originPolicy,
          sessionId,
        });
        let abortListener: (() => void) | undefined;
        if (signal && this.#enabledFeatures.has(REQUEST_CONTROL_FEATURE)) {
          abortListener = () => {
            void handle.cancel().catch(() => {});
            this.#abandonRequest(handle.id, new EventStreamAbortedError());
          };
          signal.addEventListener("abort", abortListener, { once: true });
        } else if (signal) {
          abortListener = () => {
            this.#abandonRequest(handle.id, new EventStreamAbortedError());
          };
          signal.addEventListener("abort", abortListener, { once: true });
        }
        try {
          return await handle.result;
        } finally {
          if (abortListener) {
            signal?.removeEventListener("abort", abortListener);
          }
        }
      },
      this.#helloClient,
      this.#helloProtocol,
      params,
      options,
    );
  }

  beginCall<M extends ClientRpcMethod>(
    method: M,
    ...args: CallArguments<M>
  ): RequestHandle<RpcResultFor<M>> {
    const [params, options] = args as [RpcParamsFor<M> | undefined, CallOptions<M> | undefined];
    if ((method as RpcMethod) === "system.hello") {
      throw new HandshakeStateError("system.hello must be sent through hello()");
    }
    if ((method as RpcMethod) === "events.subscribe") {
      throw new HandshakeStateError(
        "events.subscribe is available only inside the event WebSocket handshake",
      );
    }
    return this.#start(method, params, options, false, method !== "request.cancel");
  }

  async cancel(requestId: RpcId): Promise<RequestCancelResult> {
    const validatedId = parseRpcId(requestId, "request.cancel requestId");
    return await this.call("request.cancel", { requestId: validatedId });
  }

  close(): Promise<void> {
    this.#closePromise ??= this.#close();
    return this.#closePromise;
  }

  #start<M extends RpcMethod>(
    method: M,
    params: RpcParamsFor<M> | undefined,
    options: { readonly signal?: AbortSignal; readonly timeoutMs?: number } | undefined,
    handshake: boolean,
    application: boolean,
    automaticCancelTarget?: RpcId,
  ): RequestHandle<RpcResultFor<M>> {
    if (!handshake && this.#state !== "ready") {
      throw new HandshakeStateError(`${method} is not allowed while client is ${this.#state}`);
    }
    const signal = (options as { signal?: AbortSignal } | undefined)?.signal;
    if (signal?.aborted) {
      throw new RequestAbortedError();
    }
    this.#checkFeature(method, params, options);
    if (this.#pending.size >= this.#maxPendingRequests) {
      throw new PendingRequestLimitError(this.#maxPendingRequests);
    }
    if (application && this.#applicationPending >= this.#maxApplicationRequests) {
      throw new PendingRequestLimitError(this.#maxApplicationRequests);
    }

    const id: RpcId = `${this.#requestPrefix}:${this.#nextRequest.toString()}`;
    this.#nextRequest += 1n;
    const request: Record<string, unknown> = { id, jsonrpc: "2.0", method };
    if (params !== undefined) {
      if (!isObject(params) && !(Array.isArray(params) && params.length === 0)) {
        throw new ProtocolViolationError(`${method} params must be an object or empty array`);
      }
      request.params = params;
    }
    const timeoutMs = (options as { timeoutMs?: number } | undefined)?.timeoutMs;
    if (timeoutMs !== undefined) {
      request.timeoutMs = positiveSafeInteger(timeoutMs, "timeoutMs");
    }
    const line = serializeRequest(request);

    let pending!: PendingRequest;
    const result = new Promise<RpcResultFor<M>>((resolve, reject) => {
      pending = {
        application,
        method,
        reject,
        resolve: (value) => resolve(value as RpcResultFor<M>),
      };
    });
    const key = rpcIdKey(id);
    this.#pending.set(key, pending);
    if (automaticCancelTarget !== undefined) {
      this.#automaticCancelTargets.set(key, automaticCancelTarget);
    }
    if (application) {
      this.#applicationPending += 1;
    }
    let abortListener: (() => void) | undefined;
    if (signal) {
      abortListener = () => {
        this.#scheduleAutomaticCancel(id);
      };
      signal.addEventListener("abort", abortListener, { once: true });
      void result.finally(() => signal.removeEventListener("abort", abortListener!)).catch(() => {});
    }

    const writeProgress = this.#writes.progressVersion;
    void this.#writes.enqueue(line).catch((error: unknown) => {
      const automaticCancelTarget = this.#automaticCancelTargets.get(key);
      const removed = this.#takePending(key);
      if (!removed) {
        this.#abandonedRequests.delete(key);
      }
      if (removed) {
        removed.reject(error instanceof Error ? error : new TransportClosedError());
      }
      if (error instanceof WriteQueueOverflowError) {
        if (
          automaticCancelTarget !== undefined &&
          this.#state === "ready" &&
          this.#pending.has(rpcIdKey(automaticCancelTarget))
        ) {
          this.#cancelBacklog.set(rpcIdKey(automaticCancelTarget), automaticCancelTarget);
        }
        this.#scheduleCancelPumpAfterWriteProgress(writeProgress);
        return;
      }
      if (!(error instanceof WriteFrameTooLargeError)) {
        this.#fail(error instanceof Error ? error : new TransportClosedError());
      }
      if (automaticCancelTarget !== undefined && error instanceof WriteFrameTooLargeError) {
        this.#fail(error);
      } else {
        this.#pumpCancelBacklog();
      }
    });

    return {
      id,
      result,
      cancel: () => this.cancel(id),
    };
  }

  #checkFeature<M extends RpcMethod>(
    method: M,
    params: RpcParamsFor<M> | undefined,
    options: { readonly signal?: AbortSignal; readonly timeoutMs?: number } | undefined,
  ): void {
    if (method === "system.hello") {
      return;
    }
    const clientMethod = method as ClientRpcMethod;
    const required = requiredFeatures[clientMethod];
    if (required && !this.#enabledFeatures.has(required)) {
      throw new FeatureNotNegotiatedError(method, required);
    }
    const usesPagedSessionExport =
      method === "session.export" &&
      isObject(params) &&
      (params.afterSequence !== undefined || params.limit !== undefined);
    if (
      usesPagedSessionExport &&
      !this.#enabledFeatures.has(SESSION_EXPORT_PAGE_FEATURE)
    ) {
      throw new FeatureNotNegotiatedError(method, SESSION_EXPORT_PAGE_FEATURE);
    }
    const usesSemanticAction =
      method === "device.execute" &&
      isObject(params) &&
      typeof params.name === "string" &&
      semanticActionNames.has(params.name);
    if (usesSemanticAction && !this.#enabledFeatures.has(SEMANTIC_ACTIONS_FEATURE)) {
      throw new FeatureNotNegotiatedError(method, SEMANTIC_ACTIONS_FEATURE);
    }
    const signal = options?.signal;
    const timeoutMs = options?.timeoutMs;
    if ((signal !== undefined || timeoutMs !== undefined) && !timeoutMethods.has(clientMethod)) {
      throw new ProtocolViolationError(`${method} does not support timeout or cancellation options`);
    }
    const actionTimeoutMs =
      method === "device.execute" && isObject(params) ? params.actionTimeoutMs : undefined;
    if (actionTimeoutMs !== undefined) {
      if (typeof actionTimeoutMs !== "number") {
        throw new RangeError("actionTimeoutMs must be a positive safe integer");
      }
      positiveSafeInteger(actionTimeoutMs, "actionTimeoutMs");
    }
    const usesRequestControl =
      signal !== undefined || timeoutMs !== undefined || actionTimeoutMs !== undefined;
    if (usesRequestControl && !this.#enabledFeatures.has(REQUEST_CONTROL_FEATURE)) {
      throw new FeatureNotNegotiatedError(method, REQUEST_CONTROL_FEATURE);
    }
  }

  #acceptResponse(response: ParsedResponse): void {
    const key = rpcIdKey(response.id);
    const pending = this.#pending.get(key);
    const abandonedMethod = this.#abandonedRequests.get(key);
    if (!pending && abandonedMethod === undefined) {
      throw new ProtocolViolationError(`response references an unknown or completed id: ${String(response.id)}`);
    }
    const method = pending?.method ?? abandonedMethod;
    if (method === undefined) {
      throw new ProtocolViolationError(`response has no tracked method for id: ${String(response.id)}`);
    }
    validateRpcResponse(method, response.envelope);
    if (abandonedMethod !== undefined) {
      this.#abandonedRequests.delete(key);
      this.#pumpCancelBacklog();
      return;
    }
    if (!pending) {
      throw new ProtocolViolationError(`response references an unknown or completed id: ${String(response.id)}`);
    }
    this.#takePending(key);
    if (response.kind === "error") {
      pending.reject(new RpcRemoteError(response.id, response.error));
    } else {
      pending.resolve(response.result);
    }
    this.#pumpCancelBacklog();
  }

  #takePending(key: string): PendingRequest | undefined {
    const pending = this.#pending.get(key);
    if (!pending) {
      return undefined;
    }
    this.#pending.delete(key);
    this.#cancelBacklog.delete(key);
    this.#automaticCancelTargets.delete(key);
    if (pending.application) {
      this.#applicationPending -= 1;
    }
    return pending;
  }

  #abandonRequest(requestId: RpcId, error: Error): void {
    const key = rpcIdKey(requestId);
    const pending = this.#takePending(key);
    if (!pending) {
      return;
    }
    if (this.#abandonedRequests.size >= this.#maxAbandonedRequests) {
      pending.reject(error);
      this.#fail(
        new TransportClosedError(
          `the client exceeded its ${this.#maxAbandonedRequests}-request late-response budget`,
        ),
      );
      return;
    }
    this.#abandonedRequests.set(key, pending.method);
    pending.reject(error);
    this.#pumpCancelBacklog();
  }

  #scheduleAutomaticCancel(requestId: RpcId): void {
    if (
      this.#state !== "ready" ||
      !this.#enabledFeatures.has(REQUEST_CONTROL_FEATURE) ||
      !this.#pending.has(rpcIdKey(requestId))
    ) {
      return;
    }
    this.#cancelBacklog.set(rpcIdKey(requestId), requestId);
    this.#pumpCancelBacklog();
  }

  #pumpCancelBacklog(): void {
    if (this.#state !== "ready" || !this.#enabledFeatures.has(REQUEST_CONTROL_FEATURE)) {
      return;
    }
    while (this.#pending.size < this.#maxPendingRequests) {
      const next = this.#cancelBacklog.entries().next().value as
        | readonly [string, RpcId]
        | undefined;
      if (!next) {
        return;
      }
      const [targetKey, requestId] = next;
      this.#cancelBacklog.delete(targetKey);
      if (!this.#pending.has(targetKey)) {
        continue;
      }
      try {
        const handle = this.#start(
          "request.cancel",
          { requestId },
          undefined,
          false,
          false,
          requestId,
        );
        void handle.result.catch(() => {});
      } catch (cause) {
        if (cause instanceof PendingRequestLimitError) {
          this.#cancelBacklog.set(targetKey, requestId);
        }
        return;
      }
    }
  }

  #scheduleCancelPumpAfterWriteProgress(writeProgress: bigint): void {
    if (this.#cancelProgressWaitScheduled || this.#cancelBacklog.size === 0) {
      return;
    }
    this.#cancelProgressWaitScheduled = true;
    void this.#writes.waitForProgress(writeProgress).then(
      () => {
        this.#cancelProgressWaitScheduled = false;
        this.#pumpCancelBacklog();
      },
      () => {
        this.#cancelProgressWaitScheduled = false;
      },
    );
  }

  async #close(): Promise<void> {
    if (this.#state === "closed") {
      return;
    }
    if (this.#state === "failed") {
      throw this.#terminalError ?? new TransportClosedError();
    }
    this.#state = "closing";
    this.#cancelBacklog.clear();
    let timeout: NodeJS.Timeout | undefined;
    try {
      await Promise.race([
        (async () => {
          await this.#writes.close();
          try {
            this.#transport.closeInput();
          } catch (cause) {
            throw new TransportClosedError("failed to close daemon stdin", { cause });
          }
          await this.#transportClosure;
        })(),
        new Promise<never>((_resolve, reject) => {
          timeout = setTimeout(() => {
            const error = new TransportClosedError(
              `daemon did not drain and close within ${this.#closeGraceMs} ms`,
            );
            this.#fail(error);
            reject(error);
          }, this.#closeGraceMs);
        }),
      ]);
    } catch (cause) {
      const error =
        cause instanceof Error
          ? cause
          : new TransportClosedError("daemon close failed", { cause });
      this.#fail(error);
      throw this.#terminalError ?? error;
    } finally {
      if (timeout) {
        clearTimeout(timeout);
      }
    }
    if (this.#terminalError) {
      throw this.#terminalError;
    }
    if (this.state !== "closed") {
      const error = new TransportClosedError("daemon transport closed without completing shutdown");
      this.#fail(error, false);
      throw error;
    }
  }

  #transportClosed(closure: TransportClosure): void {
    if (this.#state === "closed" || this.#state === "failed") {
      return;
    }
    if (!this.#readableEnded) {
      try {
        this.#decoder.end();
        this.#readableEnded = true;
      } catch (cause) {
        this.#fail(
          cause instanceof Error
            ? cause
            : new ProtocolViolationError("response stream ended with an invalid frame"),
          false,
        );
        return;
      }
    }
    const normal = closure.error === undefined && closure.code === 0 && closure.signal === null;
    if (normal && this.#state === "closing" && this.#pending.size === 0) {
      this.#state = "closed";
      this.#cleanup();
      return;
    }
    const diagnostic = this.#stderr.text.trim();
    const error = new TransportClosedError(
      `daemon closed with code ${String(closure.code)} and signal ${String(closure.signal)}${
        diagnostic ? `; stderr: ${diagnostic}` : ""
      }`,
      closure.error ? { cause: closure.error } : undefined,
    );
    this.#fail(error, false);
  }

  #fail(error: Error, terminate = true): void {
    if (this.#state === "failed" || this.#state === "closed") {
      return;
    }
    this.#terminalError = error;
    this.#state = "failed";
    this.#writes.fail(error);
    this.#cancelBacklog.clear();
    this.#abandonedRequests.clear();
    this.#automaticCancelTargets.clear();
    for (const pending of this.#pending.values()) {
      pending.reject(error);
    }
    this.#pending.clear();
    this.#applicationPending = 0;
    if (terminate) {
      try {
        this.#transport.terminate?.();
      } catch {
        // Transport termination is best effort; all requests are already settled.
      }
    }
    this.#cleanup();
  }

  #cleanup(): void {
    this.#abandonedRequests.clear();
    this.#transport.readable.off("data", this.#onReadableData);
    this.#transport.readable.off("end", this.#onReadableEnd);
    this.#transport.readable.off("close", this.#onReadableClose);
    this.#transport.readable.off("error", this.#onReadableError);
    this.#stderr.stop();
    if (!this.#transportErrorsGuarded) {
      this.#transportErrorsGuarded = true;
      this.#transport.readable.on("error", this.#ignoreLateTransportError);
      this.#transport.stderr?.on("error", this.#ignoreLateTransportError);
    }
  }
}
