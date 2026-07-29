import assert from "node:assert/strict";
import { PassThrough, Writable } from "node:stream";
import test from "node:test";

import type { HelloParams, HelloResult, RpcId, RpcMethod } from "@devicerail/protocol";

import {
  DeviceRailClient,
  EventStreamAbortedError,
  FeatureNotNegotiatedError,
  HandshakeStateError,
  ProtocolViolationError,
  RequestAbortedError,
  RpcRemoteError,
  TransportClosedError,
  validateRpcResult,
  type ClientTransport,
  type EventStreamWebSocket,
  type EventStreamWebSocketEvent,
  type EventStreamWebSocketEventType,
  type TransportClosure,
} from "../src/index.js";

interface RequestFrame {
  readonly id: RpcId;
  readonly jsonrpc: "2.0";
  readonly method: string;
  readonly params?: unknown;
  readonly timeoutMs?: number;
}

class CapturingWritable extends Writable {
  readonly frames: string[] = [];
  readonly #callbacks: Array<(error?: Error | null) => void> = [];
  #stall: boolean;

  constructor(stall = false) {
    super();
    this.#stall = stall;
  }

  set stalled(value: boolean) {
    this.#stall = value;
  }

  releaseOne(error?: Error): void {
    const callback = this.#callbacks.shift();
    assert.ok(callback, "a stalled write callback must exist");
    callback(error);
  }

  async waitForFrames(count: number): Promise<void> {
    while (this.frames.length < count) {
      await new Promise<void>((resolve) => this.once("frame", resolve));
    }
  }

  override _write(
    chunk: Buffer,
    _encoding: BufferEncoding,
    callback: (error?: Error | null) => void,
  ): void {
    this.frames.push(Buffer.from(chunk).toString("utf8"));
    this.emit("frame");
    if (this.#stall) {
      this.#callbacks.push(callback);
    } else {
      callback();
    }
  }
}

class FakeTransport implements ClientTransport {
  readonly readable = new PassThrough();
  readonly stderr = new PassThrough();
  readonly writable: CapturingWritable;
  readonly closed: Promise<TransportClosure>;

  inputClosed = false;
  terminated = false;

  #closed = false;
  readonly #finishOnCloseInput: boolean;
  #resolveClosed!: (closure: TransportClosure) => void;

  constructor(
    options: { readonly finishOnCloseInput?: boolean; readonly stallWrites?: boolean } = {},
  ) {
    this.writable = new CapturingWritable(options.stallWrites ?? false);
    this.#finishOnCloseInput = options.finishOnCloseInput ?? true;
    this.closed = new Promise<TransportClosure>((resolve) => {
      this.#resolveClosed = resolve;
    });
  }

  closeInput(): void {
    this.inputClosed = true;
    if (this.#finishOnCloseInput) {
      this.finish({ code: 0, signal: null });
    }
  }

  terminate(): void {
    this.terminated = true;
    this.finish({ code: null, signal: "SIGTERM" });
  }

  finish(closure: TransportClosure): void {
    if (this.#closed) {
      return;
    }
    this.#closed = true;
    this.#resolveClosed(closure);
  }

  send(value: unknown): void {
    this.sendRaw(JSON.stringify(value));
  }

  sendRaw(line: string): void {
    this.readable.write(`${line}\n`);
  }

  async requestAt(index: number): Promise<RequestFrame> {
    await this.writable.waitForFrames(index + 1);
    const frame = this.writable.frames[index];
    assert.ok(frame, `request frame ${index} must exist`);
    assert.equal(frame.endsWith("\n"), true, "requests must use NDJSON framing");
    const value = JSON.parse(frame.slice(0, -1)) as unknown;
    assert.equal(typeof value, "object");
    assert.notEqual(value, null);
    return value as RequestFrame;
  }
}

class FakeEventStreamSocket implements EventStreamWebSocket {
  readonly protocol = "devicerail.events.v1";
  readonly sent: string[] = [];
  readonly #listeners = new Map<
    EventStreamWebSocketEventType,
    Set<(event: EventStreamWebSocketEvent) => void>
  >();

  addEventListener(
    type: EventStreamWebSocketEventType,
    listener: (event: EventStreamWebSocketEvent) => void,
  ): void {
    const listeners = this.#listeners.get(type) ?? new Set();
    listeners.add(listener);
    this.#listeners.set(type, listeners);
  }

  close(): void {}

  removeEventListener(
    type: EventStreamWebSocketEventType,
    listener: (event: EventStreamWebSocketEvent) => void,
  ): void {
    this.#listeners.get(type)?.delete(listener);
  }

  send(data: string): void {
    this.sent.push(data);
  }

  open(): void {
    this.#emit("open", {});
  }

  message(value: unknown): void {
    this.#emit("message", { data: JSON.stringify(value) });
  }

  #emit(type: EventStreamWebSocketEventType, event: EventStreamWebSocketEvent): void {
    for (const listener of this.#listeners.get(type) ?? []) {
      listener(event);
    }
  }
}

const ALL_FEATURES = ["request.control.v1", "device.routing.v1", "events.snapshot.v1"];
const ALL_RPC_METHODS = [
  "device.capabilities",
  "device.connect",
  "device.disconnect",
  "device.execute",
  "device.observe",
  "device.select",
  "devices.list",
  "events.clear",
  "events.list",
  "events.stream.open",
  "events.subscribe",
  "media.stream.capture",
  "media.stream.end",
  "media.stream.start",
  "request.cancel",
  "session.current",
  "session.end",
  "session.export",
  "session.start",
  "sessions.list",
  "system.describe",
  "system.hello",
  "ui.snapshot.get",
  "verdict.record",
] as const satisfies readonly RpcMethod[];
type Assert<T extends true> = T;
type _AllRpcMethodsCovered = Assert<
  Exclude<RpcMethod, (typeof ALL_RPC_METHODS)[number]> extends never ? true : false
>;
type _NoUnknownRpcMethodsListed = Assert<
  Exclude<(typeof ALL_RPC_METHODS)[number], RpcMethod> extends never ? true : false
>;

function helloParams(features: readonly string[] = ALL_FEATURES): HelloParams {
  return {
    client: { name: "rpc-client-test", version: "1.0.0" },
    features: { optional: [...features] },
    protocol: { ranges: [{ major: 1, maxMinor: 2, minMinor: 2 }] },
  };
}

function helloResult(enabled: readonly string[] = ALL_FEATURES): HelloResult {
  return {
    connectionId: "01234567-89ab-cdef-8123-456789abcdef",
    features: { enabled: [...enabled] },
    protocol: { selected: { major: 1, minor: 2 } },
    server: { name: "devicerail-daemon", version: "0.1.0" },
    transport: { framing: "ndjson", kind: "stdio" },
  };
}

function describeResult(
  sequence: number,
  connection = helloResult(),
): Record<string, unknown> {
  return {
    activeSessionId: null,
    client: { name: `client-${sequence}`, version: "1.0.0" },
    connection,
    deviceId: null,
  };
}

function observation(sequence: number): unknown {
  return {
    capturedAtMs: sequence,
    deviceId: "mock-1",
    id: `00000000-0000-4000-8000-${sequence.toString().padStart(12, "0")}`,
    viewport: { height: 800, scaleFactor: 1, width: 600 },
  };
}

function emptySessionExport(sessionId: string): unknown {
  return {
    events: [],
    session: {
      eventCount: 1,
      id: sessionId,
      lastSequence: 1,
      startedAtMs: 1,
      state: "active",
    },
  };
}

function success(id: RpcId, result: unknown): unknown {
  return { id, jsonrpc: "2.0", result };
}

function remoteFailure(id: RpcId): Record<string, unknown> {
  return {
    error: {
      code: -32_000,
      data: {
        code: "temporary_failure",
        message: "try again",
        retryable: true,
      },
      message: "temporary failure",
    },
    id,
    jsonrpc: "2.0",
  };
}

async function performHello(
  client: DeviceRailClient,
  transport: FakeTransport,
  offered: readonly string[] = ALL_FEATURES,
  enabled: readonly string[] = offered,
): Promise<HelloResult> {
  const requestIndex = transport.writable.frames.length;
  const result = client.hello(helloParams(offered));
  const request = await transport.requestAt(requestIndex);
  assert.equal(request.method, "system.hello");
  transport.send(success(request.id, helloResult(enabled)));
  return await result;
}

async function nextTurn(): Promise<void> {
  await new Promise<void>((resolve) => setImmediate(resolve));
}

async function within<T>(operation: Promise<T>, label: string, timeoutMs = 500): Promise<T> {
  let timeout: NodeJS.Timeout | undefined;
  try {
    return await Promise.race([
      operation,
      new Promise<never>((_resolve, reject) => {
        timeout = setTimeout(() => reject(new Error(`timed out waiting for ${label}`)), timeoutMs);
      }),
    ]);
  } finally {
    if (timeout) {
      clearTimeout(timeout);
    }
  }
}

test("hello negotiates once, feature snapshots cannot be mutated, and feature gates are local", async () => {
  const transport = new FakeTransport();
  const client = new DeviceRailClient(transport);

  const negotiated = await performHello(
    client,
    transport,
    ALL_FEATURES,
    ["request.control.v1", "device.routing.v1"],
  );
  assert.equal(client.state, "ready");
  assert.deepEqual(negotiated.features.enabled, ["request.control.v1", "device.routing.v1"]);

  const exposed = client.enabledFeatures as Set<string>;
  exposed.clear();
  exposed.add("events.snapshot.v1");
  assert.deepEqual(
    [...client.enabledFeatures].sort(),
    ["device.routing.v1", "request.control.v1"],
  );

  await assert.rejects(client.hello(helloParams()), HandshakeStateError);
  assert.throws(() => client.beginCall("events.clear"), FeatureNotNegotiatedError);
  const streamId = "00000000-0000-4000-8000-000000000105";
  const frameCount = transport.writable.frames.length;
  const requiresMediaFeature = (error: unknown): boolean =>
    error instanceof FeatureNotNegotiatedError && error.feature === "media.stream.v1";
  assert.throws(
    () => client.beginCall("media.stream.start", { streamId, kind: "screenshot" }),
    requiresMediaFeature,
  );
  assert.throws(
    () => client.beginCall("media.stream.capture", { streamId, frameIndex: 1 }),
    requiresMediaFeature,
  );
  assert.throws(
    () => client.beginCall("media.stream.end", { streamId }),
    requiresMediaFeature,
  );
  assert.equal(transport.writable.frames.length, frameCount);

  const requestIndex = transport.writable.frames.length;
  const routed = client.beginCall("devices.list");
  const request = await transport.requestAt(requestIndex);
  transport.send(success(request.id, { devices: [], selectedDeviceId: null }));
  assert.deepEqual(await routed.result, { devices: [], selectedDeviceId: null });

  await client.close();
  assert.equal(client.state, "closed");
});

test("hello accepts protocol 1.5 within the client support window", async () => {
  const transport = new FakeTransport();
  const client = new DeviceRailClient(transport);
  const pending = client.hello({
    client: { name: "protocol-15", version: "1.0.0" },
    protocol: { ranges: [{ major: 1, minMinor: 5, maxMinor: 5 }] },
  });
  const request = await transport.requestAt(0);
  transport.send(
    success(request.id, {
      ...helloResult([]),
      protocol: { selected: { major: 1, minor: 5 } },
    }),
  );

  const negotiated = await pending;
  assert.deepEqual(negotiated.protocol.selected, { major: 1, minor: 5 });
  await client.close();
});

test("protocol 1.5 UI, semantic Action, and Verdict methods are feature-gated locally", async () => {
  const observationId = "00000000-0000-4000-8000-000000000151";
  const callId = "00000000-0000-4000-8000-000000000152";
  const gatedTransport = new FakeTransport();
  const gatedClient = new DeviceRailClient(gatedTransport);
  const gatedHello = gatedClient.hello({
    client: { name: "gated-15", version: "1.0.0" },
    protocol: { ranges: [{ major: 1, minMinor: 5, maxMinor: 5 }] },
  });
  const gatedRequest = await gatedTransport.requestAt(0);
  gatedTransport.send(success(gatedRequest.id, {
    ...helloResult([]),
    protocol: { selected: { major: 1, minor: 5 } },
  }));
  await gatedHello;

  const hasFeature = (feature: string) => (error: unknown): boolean =>
    error instanceof FeatureNotNegotiatedError && error.feature === feature;
  assert.throws(
    () => gatedClient.beginCall("ui.snapshot.get", { observationId }),
    hasFeature("observation.uiSnapshot.v1"),
  );
  assert.throws(
    () => gatedClient.beginCall("verdict.record", {
      verdict: { status: "unknown", summary: "not evaluated", evidence: [] },
    }),
    hasFeature("verdict.record.v1"),
  );
  assert.throws(
    () => gatedClient.beginCall("device.execute", {
      id: callId,
      name: "findElement",
      arguments: { selector: { role: "button" } },
    }),
    hasFeature("device.semanticActions.v1"),
  );
  await gatedClient.close();

  const enabled = [
    "observation.uiSnapshot.v1",
    "device.semanticActions.v1",
    "verdict.record.v1",
  ];
  const transport = new FakeTransport();
  const client = new DeviceRailClient(transport);
  const hello = client.hello({
    client: { name: "enabled-15", version: "1.0.0" },
    features: { required: enabled },
    protocol: { ranges: [{ major: 1, minMinor: 5, maxMinor: 5 }] },
  });
  const request = await transport.requestAt(0);
  transport.send(success(request.id, {
    ...helloResult(enabled),
    protocol: { selected: { major: 1, minor: 5 } },
  }));
  await hello;

  const ui = client.beginCall("ui.snapshot.get", { observationId });
  const uiRequest = await transport.requestAt(1);
  assert.equal(uiRequest.method, "ui.snapshot.get");
  transport.send(remoteFailure(uiRequest.id));
  await assert.rejects(ui.result, RpcRemoteError);

  const semantic = client.beginCall("device.execute", {
    id: callId,
    name: "findElement",
    arguments: { selector: { role: "button" } },
  });
  const semanticRequest = await transport.requestAt(2);
  assert.equal(semanticRequest.method, "device.execute");
  transport.send(remoteFailure(semanticRequest.id));
  await assert.rejects(semantic.result, RpcRemoteError);

  const verdict = client.beginCall("verdict.record", {
    verdict: { status: "unknown", summary: "not evaluated", evidence: [] },
  });
  const verdictRequest = await transport.requestAt(3);
  assert.equal(verdictRequest.method, "verdict.record");
  transport.send(remoteFailure(verdictRequest.id));
  await assert.rejects(verdict.result, RpcRemoteError);
  await client.close();
});

test("media capture admits request timeout only after both features are negotiated", async () => {
  const transport = new FakeTransport();
  const client = new DeviceRailClient(transport);
  const streamId = "00000000-0000-4000-8000-000000000106";
  const hello = client.hello({
    client: { name: "media-client", version: "1.0.0" },
    features: { optional: ["media.stream.v1", "request.control.v1"] },
    protocol: { ranges: [{ major: 1, minMinor: 4, maxMinor: 4 }] },
  });
  const helloRequest = await transport.requestAt(0);
  transport.send(
    success(helloRequest.id, {
      ...helloResult(["media.stream.v1", "request.control.v1"]),
      protocol: { selected: { major: 1, minor: 4 } },
    }),
  );
  await hello;

  const capture = client.beginCall(
    "media.stream.capture",
    { streamId, frameIndex: 1, durationMs: 100 },
    { timeoutMs: 1_234 },
  );
  const request = await transport.requestAt(1);
  assert.equal(request.method, "media.stream.capture");
  assert.equal(request.timeoutMs, 1_234);
  assert.deepEqual(request.params, { streamId, frameIndex: 1, durationMs: 100 });
  const digest = "a".repeat(64);
  transport.send(
    success(request.id, {
      frame: {
        streamId,
        frameIndex: 1,
        keyFrame: true,
        durationMs: 100,
        evidence: {
          id: `sha256:${digest}`,
          mediaType: "image/png",
          sha256: digest,
          uri: `devicerail://assets/sha256/${digest}`,
        },
      },
    }),
  );
  assert.equal((await capture.result).frame.frameIndex, 1);
  await client.close();
});

test("openEventStream issues only the capability over stdio and performs subscribe on WebSocket", async () => {
  const transport = new FakeTransport({ finishOnCloseInput: false });
  const client = new DeviceRailClient(transport);
  const sessionId = "00000000-0000-4000-8000-000000000101";
  const streamEpoch = "00000000-0000-4000-8000-000000000102";
  const subscriptionId = "00000000-0000-4000-8000-000000000103";

  const helloPromise = client.hello({
    client: { name: "stream-client", version: "1.0.0" },
    features: { required: ["events.stream.v1"] },
    protocol: { ranges: [{ major: 1, maxMinor: 3, minMinor: 3 }] },
  });
  const helloRequest = await transport.requestAt(0);
  transport.send(
    success(helloRequest.id, {
      connectionId: "00000000-0000-4000-8000-000000000104",
      features: { enabled: ["events.stream.v1"] },
      protocol: { selected: { major: 1, minor: 3 } },
      server: { name: "devicerail-daemon", version: "0.1.0" },
      transport: { framing: "ndjson", kind: "stdio" },
    }),
  );
  await helloPromise;

  assert.throws(
    () =>
      (
        client.beginCall as unknown as (
          method: string,
          params: unknown,
        ) => unknown
      )("events.subscribe", { sessionId }),
    HandshakeStateError,
  );

  const aborted = new AbortController();
  let abortedFactoryCalls = 0;
  const abortedOpen = client.openEventStream(
    { sessionId },
    {
      signal: aborted.signal,
      webSocketFactory: () => {
        abortedFactoryCalls += 1;
        return new FakeEventStreamSocket();
      },
    },
  );
  const abandonedRequest = await transport.requestAt(1);
  assert.equal(abandonedRequest.method, "events.stream.open");
  aborted.abort();
  await assert.rejects(abortedOpen, EventStreamAbortedError);
  assert.equal(client.pendingRequests, 0);
  transport.send(
    success(abandonedRequest.id, {
      endpoint: `ws://127.0.0.1:45123/v/${"c".repeat(64)}`,
      expiresAtMs: Date.now() + 30_000,
      streamEpoch,
    }),
  );
  await nextTurn();
  assert.equal(client.state, "ready");
  assert.equal(abortedFactoryCalls, 0);

  const socket = new FakeEventStreamSocket();
  const streamPromise = client.openEventStream(
    { sessionId },
    {
      webSocketFactory: (endpoint, subprotocol) => {
        assert.match(endpoint, /^ws:\/\/127\.0\.0\.1:/u);
        assert.equal(subprotocol, "devicerail.events.v1");
        return socket;
      },
    },
  );
  const openRequest = await transport.requestAt(2);
  assert.equal(openRequest.method, "events.stream.open");
  assert.deepEqual(openRequest.params, {
    originPolicy: { kind: "absent" },
    sessionId,
  });
  transport.send(
    success(openRequest.id, {
      endpoint: `ws://127.0.0.1:45123/events/${"b".repeat(64)}`,
      expiresAtMs: Date.now() + 30_000,
      streamEpoch,
    }),
  );
  await nextTurn();
  socket.open();
  const webSocketHello = JSON.parse(socket.sent[0] ?? "null") as RequestFrame;
  assert.equal(webSocketHello.method, "system.hello");
  socket.message({
    id: webSocketHello.id,
    jsonrpc: "2.0",
    result: {
      connectionId: "00000000-0000-4000-8000-000000000105",
      features: { enabled: ["events.stream.v1"] },
      protocol: { selected: { major: 1, minor: 3 } },
      server: { name: "devicerail-daemon", version: "0.1.0" },
      transport: { framing: "jsonMessage", kind: "webSocket" },
    },
  });
  const subscribe = JSON.parse(socket.sent[1] ?? "null") as RequestFrame;
  assert.equal(subscribe.method, "events.subscribe");
  socket.message({
    id: subscribe.id,
    jsonrpc: "2.0",
    result: {
      replayThrough: { sequence: 1, sessionId, streamEpoch },
      sessionId,
      sessionState: "active",
      subscriptionId,
    },
  });
  const stream = await streamPromise;
  assert.equal(transport.writable.frames.length, 3);

  const finalCursor = { sequence: 1, sessionId, streamEpoch };
  socket.message({
    jsonrpc: "2.0",
    method: "events.stream.event",
    params: {
      cursor: finalCursor,
      event: {
        atMs: 1,
        eventId: "00000000-0000-4000-8000-000000000106",
        payload: { outcome: "completed", type: "sessionEnded" },
        sequence: 1,
        sessionId,
      },
      subscriptionId,
    },
  });
  socket.message({
    jsonrpc: "2.0",
    method: "events.stream.terminal",
    params: {
      lastEmittedCursor: finalCursor,
      sessionId,
      subscriptionId,
      termination: { reason: "sessionEnded" },
    },
  });
  assert.equal((await stream.next()).value?.event.payload.type, "sessionEnded");
  assert.deepEqual(await stream.next(), { done: true, value: undefined });

  const closeAbort = new AbortController();
  const abandonedDuringClose = client.openEventStream(
    { sessionId },
    { signal: closeAbort.signal, webSocketFactory: () => new FakeEventStreamSocket() },
  );
  const lateRequest = await transport.requestAt(3);
  closeAbort.abort();
  await assert.rejects(abandonedDuringClose, EventStreamAbortedError);
  const closing = client.close();
  while (!transport.inputClosed) {
    await nextTurn();
  }
  transport.send(
    success(lateRequest.id, {
      endpoint: `ws://127.0.0.1:45123/v/${"d".repeat(64)}`,
      expiresAtMs: Date.now() + 30_000,
      streamEpoch,
    }),
  );
  await nextTurn();
  assert.equal(client.state, "closing");
  transport.finish({ code: 0, signal: null });
  await closing;
  assert.equal(client.state, "closed");
});

test("fifty concurrent requests correlate correctly when responses arrive in reverse order", async () => {
  const transport = new FakeTransport();
  const client = new DeviceRailClient(transport);
  await performHello(client, transport);

  const firstIndex = transport.writable.frames.length;
  const handles = Array.from({ length: 50 }, () => client.beginCall("system.describe"));
  await transport.writable.waitForFrames(firstIndex + handles.length);
  const requests = await Promise.all(
    handles.map((_handle, index) => transport.requestAt(firstIndex + index)),
  );

  for (let index = requests.length - 1; index >= 0; index -= 1) {
    const request = requests[index];
    assert.ok(request);
    transport.send(success(request.id, describeResult(index)));
  }

  const results = await Promise.all(handles.map((handle) => handle.result));
  assert.deepEqual(
    results.map((result) => result.client.name),
    Array.from({ length: 50 }, (_unused, index) => `client-${index}`),
  );
  assert.equal(client.pendingRequests, 0);
  await client.close();
});

test("strict response envelopes fail the connection and settle the pending hello", async (context) => {
  const cases: ReadonlyArray<{
    readonly frame: (id: RpcId) => unknown;
    readonly name: string;
  }> = [
    {
      name: "wrong JSON-RPC version",
      frame: (id) => ({ id, jsonrpc: "1.0", result: {} }),
    },
    {
      name: "both result and error",
      frame: (id) => ({ ...remoteFailure(id), result: {} }),
    },
    {
      name: "neither result nor error",
      frame: (id) => ({ id, jsonrpc: "2.0" }),
    },
    {
      name: "unknown envelope field",
      frame: (id) => ({ id, jsonrpc: "2.0", result: {}, surprise: true }),
    },
    {
      name: "unknown response id",
      frame: (id) => ({ id: `${String(id)}-unknown`, jsonrpc: "2.0", result: {} }),
    },
    {
      name: "unsafe integer",
      frame: (id) => ({ id, jsonrpc: "2.0", result: { value: 9_007_199_254_740_992 } }),
    },
    {
      name: "out-of-range RPC error code",
      frame: (id) => ({
        ...remoteFailure(id),
        error: {
          code: 2_147_483_648,
          data: { code: "bad", message: "bad", retryable: false },
          message: "bad",
        },
      }),
    },
  ];

  for (const candidate of cases) {
    await context.test(candidate.name, async () => {
      const transport = new FakeTransport();
      const client = new DeviceRailClient(transport);
      const pending = client.hello(helloParams());
      const request = await transport.requestAt(0);
      transport.send(candidate.frame(request.id));
      await assert.rejects(pending, ProtocolViolationError);
      assert.equal(client.state, "failed");
      assert.equal(client.pendingRequests, 0);
      assert.equal(transport.terminated, true);
    });
  }
});

test("method response Schemas reject invalid result types and unknown result fields", async (context) => {
  const cases: ReadonlyArray<{
    readonly name: string;
    readonly result: unknown;
  }> = [
    {
      name: "wrong nested type",
      result: { ...describeResult(1), client: 7 },
    },
    {
      name: "unknown result field",
      result: { ...describeResult(1), surprise: true },
    },
    {
      name: "result from another method",
      result: { devices: [], selectedDeviceId: null },
    },
  ];

  for (const candidate of cases) {
    await context.test(candidate.name, async () => {
      const transport = new FakeTransport();
      const client = new DeviceRailClient(transport);
      await performHello(client, transport);
      const requestIndex = transport.writable.frames.length;
      const pending = client.beginCall("system.describe").result;
      const request = await transport.requestAt(requestIndex);
      transport.send(success(request.id, candidate.result));
      await assert.rejects(pending, ProtocolViolationError);
      assert.equal(client.state, "failed");
      assert.equal(client.pendingRequests, 0);
      assert.equal(transport.terminated, true);
    });
  }
});

test("all method response Schemas compile and the public result boundary accepts only pure JSON", () => {
  for (const method of ALL_RPC_METHODS) {
    assert.throws(() => validateRpcResult(method, undefined), ProtocolViolationError);
  }
  assert.doesNotThrow(() => validateRpcResult("device.capabilities", []));

  const baseActionResult = {
    callId: "00000000-0000-4000-8000-000000000301",
    finishedAtMs: 2,
    output: null as unknown,
    startedAtMs: 1,
  };
  let getterCalled = false;
  const accessor: Record<string, unknown> = {};
  Object.defineProperty(accessor, "protectedSentinel", {
    enumerable: true,
    get() {
      getterCalled = true;
      return "must-not-be-read";
    },
  });
  const proxy = new Proxy({}, {
    ownKeys() {
      getterCalled = true;
      return [];
    },
  });
  const arrayWithNonIndexProperty: unknown[] = [];
  Object.defineProperty(arrayWithNonIndexProperty, "4294967295", {
    enumerable: true,
    value: "not-an-array-element",
  });
  let tooDeep: unknown = null;
  for (let depth = 0; depth <= 256; depth += 1) {
    tooDeep = { child: tooDeep };
  }
  for (const output of [
    undefined,
    () => {},
    Symbol("secret"),
    1n,
    accessor,
    proxy,
    arrayWithNonIndexProperty,
    tooDeep,
  ]) {
    assert.throws(
      () => validateRpcResult("device.execute", { ...baseActionResult, output }),
      ProtocolViolationError,
    );
  }
  assert.throws(
    () =>
      validateRpcResult("device.execute", {
        ...baseActionResult,
        output: { value: 9_007_199_254_740_992 },
      }),
    ProtocolViolationError,
  );
  assert.equal(getterCalled, false, "runtime validation must not inspect accessors or proxies");
  const uncheckedValidator = validateRpcResult as unknown as (
    method: unknown,
    result: unknown,
  ) => void;
  assert.throws(
    () => uncheckedValidator("toString", {}),
    ProtocolViolationError,
  );
  let coercionCalled = false;
  const coercingMethod = {
    [Symbol.toPrimitive]() {
      coercionCalled = true;
      return "system.describe";
    },
  };
  assert.throws(() => uncheckedValidator(Symbol("method"), {}), ProtocolViolationError);
  assert.throws(() => uncheckedValidator(coercingMethod, {}), ProtocolViolationError);
  assert.equal(coercionCalled, false);
});

test("Schema diagnostics redact remote property names and values", () => {
  const protectedKey = "protected-sentinel\n\u001b[31m";
  let error: unknown;
  try {
    validateRpcResult("system.describe", {
      ...describeResult(1),
      [protectedKey]: "protected-value-sentinel",
    });
  } catch (cause) {
    error = cause;
  }
  assert.ok(error instanceof ProtocolViolationError);
  assert.equal(error.message.includes("protected-sentinel"), false);
  assert.equal(error.message.includes("protected-value-sentinel"), false);
  assert.equal(/[\n\r\u001b]/u.test(error.message), false);
  assert.ok(error.message.length <= 512);
  assert.equal(error.cause, undefined);
});

test("one invalid method result terminally rejects every concurrent pending request", async () => {
  const transport = new FakeTransport();
  const client = new DeviceRailClient(transport);
  await performHello(client, transport);

  const requestIndex = transport.writable.frames.length;
  const malformed = client.beginCall("system.describe").result;
  const collateral = client.beginCall("devices.list").result;
  const malformedRequest = await transport.requestAt(requestIndex);
  await transport.requestAt(requestIndex + 1);
  transport.send(success(malformedRequest.id, { ...describeResult(1), unknown: true }));

  await Promise.all([
    assert.rejects(malformed, ProtocolViolationError),
    assert.rejects(collateral, ProtocolViolationError),
  ]);
  assert.equal(client.state, "failed");
  assert.equal(client.pendingRequests, 0);
  assert.equal(transport.terminated, true);
});

test("an invalid late response for an abandoned method remains a terminal protocol violation", async () => {
  const transport = new FakeTransport({ finishOnCloseInput: false });
  const client = new DeviceRailClient(transport);
  const sessionId = "00000000-0000-4000-8000-000000000201";
  const hello = client.hello({
    client: { name: "late-response-client", version: "1.0.0" },
    features: { required: ["events.stream.v1"] },
    protocol: { ranges: [{ major: 1, maxMinor: 3, minMinor: 3 }] },
  });
  const helloRequest = await transport.requestAt(0);
  transport.send(
    success(helloRequest.id, {
      ...helloResult(["events.stream.v1"]),
      protocol: { selected: { major: 1, minor: 3 } },
    }),
  );
  await hello;

  const controller = new AbortController();
  const opening = client.openEventStream(
    { sessionId },
    {
      signal: controller.signal,
      webSocketFactory: () => new FakeEventStreamSocket(),
    },
  );
  const request = await transport.requestAt(1);
  controller.abort();
  await assert.rejects(opening, EventStreamAbortedError);
  transport.send(success(request.id, { endpoint: 7 }));
  await nextTurn();

  assert.equal(client.state, "failed");
  assert.equal(transport.terminated, true);
});

test("a response for a completed id is a terminal protocol violation", async () => {
  const transport = new FakeTransport();
  const client = new DeviceRailClient(transport);
  await performHello(client, transport);

  const requestIndex = transport.writable.frames.length;
  const handle = client.beginCall("system.describe");
  const request = await transport.requestAt(requestIndex);
  const response = success(request.id, describeResult(1));
  transport.send(response);
  await handle.result;

  transport.send(response);
  await nextTurn();
  assert.equal(client.state, "failed");
  assert.equal(transport.terminated, true);
});

test("an invalid hello result is terminal, while a remote hello error can be retried", async () => {
  const malformedTransport = new FakeTransport();
  const malformedClient = new DeviceRailClient(malformedTransport);
  const malformed = malformedClient.hello(helloParams());
  const malformedRequest = await malformedTransport.requestAt(0);
  malformedTransport.send(
    success(malformedRequest.id, {
      ...helloResult(),
      connectionId: "not-a-uuid",
    }),
  );
  await assert.rejects(malformed, ProtocolViolationError);
  assert.equal(malformedClient.state, "failed");
  assert.equal(malformedTransport.terminated, true);

  const retryTransport = new FakeTransport();
  const retryClient = new DeviceRailClient(retryTransport);
  const first = retryClient.hello(helloParams());
  const firstRequest = await retryTransport.requestAt(0);
  retryTransport.send(remoteFailure(firstRequest.id));
  await assert.rejects(first, RpcRemoteError);
  assert.equal(retryClient.state, "awaitingHello");
  assert.equal(retryTransport.terminated, false);

  const retried = retryClient.hello(helloParams());
  const secondRequest = await retryTransport.requestAt(1);
  retryTransport.send(success(secondRequest.id, helloResult()));
  await retried;
  assert.equal(retryClient.state, "ready");
  await retryClient.close();
});

test("hello rejects unsupported offers and known features below their protocol version", async () => {
  const unsupportedTransport = new FakeTransport();
  const unsupportedClient = new DeviceRailClient(unsupportedTransport);
  await assert.rejects(
    unsupportedClient.hello({
      client: { name: "future", version: "1.0.0" },
      protocol: { ranges: [{ major: 2, minMinor: 0, maxMinor: 0 }] },
    }),
    ProtocolViolationError,
  );
  assert.equal(unsupportedClient.state, "awaitingHello");
  assert.equal(unsupportedTransport.writable.frames.length, 0);

  const inconsistentTransport = new FakeTransport();
  const inconsistentClient = new DeviceRailClient(inconsistentTransport);
  const pending = inconsistentClient.hello({
    client: { name: "inconsistent", version: "1.0.0" },
    features: { optional: ["request.control.v1"] },
    protocol: { ranges: [{ major: 1, minMinor: 0, maxMinor: 0 }] },
  });
  const request = await inconsistentTransport.requestAt(0);
  inconsistentTransport.send(
    success(request.id, {
      ...helloResult(["request.control.v1"]),
      protocol: { selected: { major: 1, minor: 0 } },
    }),
  );
  await assert.rejects(pending, ProtocolViolationError);
  assert.equal(inconsistentClient.state, "failed");

  const protectedTransport = new FakeTransport();
  const protectedClient = new DeviceRailClient(protectedTransport);
  const protectedPending = protectedClient.hello({
    client: { name: "protected-inconsistent", version: "1.0.0" },
    features: { optional: ["action.protected.v1"] },
    protocol: { ranges: [{ major: 1, minMinor: 1, maxMinor: 1 }] },
  });
  const protectedRequest = await protectedTransport.requestAt(0);
  protectedTransport.send(
    success(protectedRequest.id, {
      ...helloResult(["action.protected.v1"]),
      protocol: { selected: { major: 1, minor: 1 } },
    }),
  );
  await assert.rejects(protectedPending, ProtocolViolationError);
  assert.equal(protectedClient.state, "failed");

  const streamTransport = new FakeTransport();
  const streamClient = new DeviceRailClient(streamTransport);
  const streamPending = streamClient.hello({
    client: { name: "stream-inconsistent", version: "1.0.0" },
    features: { optional: ["events.stream.v1"] },
    protocol: { ranges: [{ major: 1, minMinor: 2, maxMinor: 2 }] },
  });
  const streamRequest = await streamTransport.requestAt(0);
  streamTransport.send(
    success(streamRequest.id, {
      ...helloResult(["events.stream.v1"]),
      protocol: { selected: { major: 1, minor: 2 } },
    }),
  );
  await assert.rejects(streamPending, ProtocolViolationError);
  assert.equal(streamClient.state, "failed");

  const mediaTransport = new FakeTransport();
  const mediaClient = new DeviceRailClient(mediaTransport);
  const mediaPending = mediaClient.hello({
    client: { name: "media-inconsistent", version: "1.0.0" },
    features: { optional: ["media.stream.v1"] },
    protocol: { ranges: [{ major: 1, minMinor: 3, maxMinor: 3 }] },
  });
  const mediaRequest = await mediaTransport.requestAt(0);
  mediaTransport.send(
    success(mediaRequest.id, {
      ...helloResult(["media.stream.v1"]),
      protocol: { selected: { major: 1, minor: 3 } },
    }),
  );
  await assert.rejects(mediaPending, ProtocolViolationError);
  assert.equal(mediaClient.state, "failed");

  const exportPageTransport = new FakeTransport();
  const exportPageClient = new DeviceRailClient(exportPageTransport);
  const exportPagePending = exportPageClient.hello({
    client: { name: "export-page-inconsistent", version: "1.0.0" },
    features: { optional: ["session.export.page.v1"] },
    protocol: { ranges: [{ major: 1, minMinor: 3, maxMinor: 3 }] },
  });
  const exportPageRequest = await exportPageTransport.requestAt(0);
  exportPageTransport.send(
    success(exportPageRequest.id, {
      ...helloResult(["session.export.page.v1"]),
      protocol: { selected: { major: 1, minor: 3 } },
    }),
  );
  await assert.rejects(exportPagePending, ProtocolViolationError);
  assert.equal(exportPageClient.state, "failed");

  for (const feature of [
    "observation.uiSnapshot.v1",
    "device.semanticActions.v1",
    "verdict.record.v1",
  ]) {
    const transport = new FakeTransport();
    const client = new DeviceRailClient(transport);
    const featurePending = client.hello({
      client: { name: `inconsistent-${feature}`, version: "1.0.0" },
      features: { optional: [feature] },
      protocol: { ranges: [{ major: 1, minMinor: 4, maxMinor: 4 }] },
    });
    const featureRequest = await transport.requestAt(0);
    transport.send(success(featureRequest.id, {
      ...helloResult([feature]),
      protocol: { selected: { major: 1, minor: 4 } },
    }));
    await assert.rejects(featurePending, ProtocolViolationError);
    assert.equal(client.state, "failed");
  }

  const overlapTransport = new FakeTransport();
  const overlapClient = new DeviceRailClient(overlapTransport);
  const overlap = overlapClient.hello({
    client: { name: "overlap", version: "1.0.0" },
    features: {
      optional: ["events.snapshot.v1"],
      required: ["events.snapshot.v1"],
    },
    protocol: { ranges: [{ major: 1, minMinor: 0, maxMinor: 2 }] },
  });
  const overlapRequest = await overlapTransport.requestAt(0);
  overlapTransport.send(success(overlapRequest.id, helloResult(["events.snapshot.v1"])));
  await overlap;
  await overlapClient.close();
});

test("stdout EOF and abnormal transport closure reject every pending request with stderr context", async (context) => {
  await context.test("stdout EOF", async () => {
    const transport = new FakeTransport();
    const client = new DeviceRailClient(transport);
    await performHello(client, transport);
    const pending = client.beginCall("system.describe").result;
    void pending.catch(() => {});
    transport.stderr.write("fatal stdout diagnostic");
    transport.readable.end();

    await assert.rejects(
      pending,
      (error: unknown) =>
        error instanceof TransportClosedError && error.message.includes("fatal stdout diagnostic"),
    );
    assert.equal(client.state, "failed");
    assert.equal(client.pendingRequests, 0);
  });

  await context.test("abnormal process closure", async () => {
    const transport = new FakeTransport();
    const client = new DeviceRailClient(transport);
    await performHello(client, transport);
    const pending = client.beginCall("system.describe").result;
    void pending.catch(() => {});
    transport.stderr.write("fatal process diagnostic");
    transport.finish({ code: 17, error: new Error("process failed"), signal: null });

    await assert.rejects(
      pending,
      (error: unknown) =>
        error instanceof TransportClosedError &&
        error.message.includes("code 17") &&
        error.message.includes("fatal process diagnostic") &&
        error.cause instanceof Error &&
        error.cause.message === "process failed",
    );
    assert.equal(client.state, "failed");
    assert.equal(client.pendingRequests, 0);
  });
});

test("close times out a permanently stalled write and settles pending work", async () => {
  const transport = new FakeTransport({ stallWrites: true });
  const client = new DeviceRailClient(transport, { closeGraceMs: 25 });
  const hello = client.hello(helloParams());
  void hello.catch(() => {});
  await transport.writable.waitForFrames(1);

  await assert.rejects(client.close(), TransportClosedError);
  await assert.rejects(hello, TransportClosedError);
  assert.equal(client.state, "failed");
  assert.equal(client.pendingRequests, 0);
  assert.equal(transport.inputClosed, false);
  assert.equal(transport.terminated, true);
});

test("AbortSignal cancellation uses the reserved capacity and does not drop a burst", async () => {
  const transport = new FakeTransport();
  const client = new DeviceRailClient(transport, { maxPendingRequests: 8 });
  await performHello(client, transport, ["request.control.v1"], ["request.control.v1"]);

  const firstApplicationIndex = transport.writable.frames.length;
  const controllers = Array.from({ length: 6 }, () => new AbortController());
  const handles = controllers.map((controller) =>
    client.beginCall("device.observe", {}, { signal: controller.signal }),
  );
  await transport.writable.waitForFrames(firstApplicationIndex + handles.length);
  controllers.forEach((controller) => controller.abort());

  const cancellationTargets: RpcId[] = [];
  let nextRequestIndex = firstApplicationIndex + handles.length;
  while (cancellationTargets.length < handles.length) {
    const request = await transport.requestAt(nextRequestIndex);
    nextRequestIndex += 1;
    assert.equal(request.method, "request.cancel");
    assert.equal(typeof request.params, "object");
    assert.notEqual(request.params, null);
    const requestId = (request.params as { requestId?: unknown }).requestId;
    assert.ok(typeof requestId === "string" || typeof requestId === "number");
    cancellationTargets.push(requestId);
    transport.send(success(request.id, { requestId, status: "requested" }));
  }

  assert.deepEqual(
    new Set(cancellationTargets),
    new Set(handles.map((handle) => handle.id)),
  );
  assert.equal(cancellationTargets.length, handles.length);

  for (let index = 0; index < handles.length; index += 1) {
    const request = await transport.requestAt(firstApplicationIndex + index);
    transport.send(success(request.id, observation(index)));
  }
  await Promise.all(handles.map((handle) => handle.result));
  assert.equal(client.pendingRequests, 0);
  await client.close();
});

test(
  "automatic cancellation waits for a full write queue instead of retry-spinning",
  { timeout: 2_000 },
  async () => {
    const transport = new FakeTransport();
    const client = new DeviceRailClient(transport, {
      maxPendingRequests: 8,
      maxQueuedFrames: 1,
    });
    await performHello(client, transport, ["request.control.v1"], ["request.control.v1"]);
    await nextTurn();

    transport.writable.stalled = true;
    const requestIndex = transport.writable.frames.length;
    const controller = new AbortController();
    const handle = client.beginCall("device.observe", {}, { signal: controller.signal });
    const request = await transport.requestAt(requestIndex);
    controller.abort();
    await new Promise<void>((resolve) => setTimeout(resolve, 10));
    assert.equal(transport.writable.frames.length, requestIndex + 1);

    transport.writable.releaseOne();
    const cancellation = await within(
      transport.requestAt(requestIndex + 1),
      "deferred cancellation frame",
    );
    assert.equal(cancellation.method, "request.cancel");
    transport.send(
      success(cancellation.id, {
        requestId: handle.id,
        status: "requested",
      }),
    );
    transport.send(success(request.id, observation(1)));
    await within(handle.result, "original request result");

    transport.writable.releaseOne();
    transport.writable.stalled = false;
    await within(client.close(), "client close");
  },
);

test("automatic cancellation observes queue progress that races its overflow handler", async () => {
  const transport = new FakeTransport();
  const client = new DeviceRailClient(transport, {
    maxPendingRequests: 8,
    maxQueuedFrames: 1,
  });
  await performHello(client, transport, ["request.control.v1"], ["request.control.v1"]);
  await nextTurn();

  const requestIndex = transport.writable.frames.length;
  const controller = new AbortController();
  const handle = client.beginCall("device.observe", {}, { signal: controller.signal });
  controller.abort();

  const request = await transport.requestAt(requestIndex);
  const cancellation = await within(
    transport.requestAt(requestIndex + 1),
    "racing cancellation frame",
  );
  assert.equal(cancellation.method, "request.cancel");
  transport.send(success(cancellation.id, { requestId: handle.id, status: "requested" }));
  transport.send(success(request.id, observation(2)));
  await handle.result;
  await client.close();
});

test("outbound admission validates final serialized numbers, timeout fields, ids, and aborts", async () => {
  assert.throws(
    () => new DeviceRailClient(new FakeTransport(), { maxPendingRequests: 1 }),
    RangeError,
  );
  const transport = new FakeTransport();
  const client = new DeviceRailClient(transport);
  await performHello(client, transport);
  const frameCount = transport.writable.frames.length;

  assert.throws(
    () =>
      client.beginCall("device.execute", {
        actionTimeoutMs: 0,
        arguments: {},
        id: "00000000-0000-4000-8000-000000000001",
        name: "tap",
      }),
    RangeError,
  );

  assert.throws(
    () =>
      client.beginCall("device.execute", {
        arguments: {
          toJSON: () => ({ unsafe: 9_007_199_254_740_992 }),
        },
        id: "00000000-0000-4000-8000-000000000002",
        name: "tap",
      }),
    ProtocolViolationError,
  );

  const controller = new AbortController();
  controller.abort();
  assert.throws(
    () => client.beginCall("device.observe", undefined, { signal: controller.signal }),
    RequestAbortedError,
  );
  await assert.rejects(client.cancel(-1), ProtocolViolationError);
  assert.equal(transport.writable.frames.length, frameCount);

  await client.close();
});

test("call turns synchronous feature admission failures into rejected promises", async () => {
  const transport = new FakeTransport();
  const client = new DeviceRailClient(transport);
  await performHello(client, transport, [], []);

  const unavailable = client.call("events.list");
  await assert.rejects(unavailable, FeatureNotNegotiatedError);
  await client.close();
});

test("session.export pagination requires its additive feature while legacy export does not", async () => {
  const transport = new FakeTransport();
  const client = new DeviceRailClient(transport);
  await performHello(
    client,
    transport,
    ["events.snapshot.v1"],
    ["events.snapshot.v1"],
  );

  const sessionId = "33333333-3333-4333-8333-333333333333";
  const requestIndex = transport.writable.frames.length;
  const legacy = client.beginCall("session.export", { sessionId });
  const legacyRequest = await transport.requestAt(requestIndex);
  assert.deepEqual(legacyRequest.params, { sessionId });
  transport.send(success(legacyRequest.id, emptySessionExport(sessionId)));
  await legacy.result;

  const frameCount = transport.writable.frames.length;
  assert.throws(
    () => client.beginCall("session.export", { sessionId, limit: 1 }),
    (error: unknown) =>
      error instanceof FeatureNotNegotiatedError &&
      error.feature === "session.export.page.v1",
  );
  assert.equal(transport.writable.frames.length, frameCount);
  await client.close();

  const pagingTransport = new FakeTransport();
  const pagingClient = new DeviceRailClient(pagingTransport);
  const hello = pagingClient.hello({
    client: { name: "paging-client", version: "1.0.0" },
    features: {
      optional: ["events.snapshot.v1", "session.export.page.v1"],
    },
    protocol: { ranges: [{ major: 1, minMinor: 4, maxMinor: 4 }] },
  });
  const helloRequest = await pagingTransport.requestAt(0);
  pagingTransport.send(
    success(helloRequest.id, {
      ...helloResult(["events.snapshot.v1", "session.export.page.v1"]),
      protocol: { selected: { major: 1, minor: 4 } },
    }),
  );
  await hello;

  const page = pagingClient.beginCall("session.export", {
    afterSequence: 1,
    limit: 2,
    sessionId,
  });
  const pageRequest = await pagingTransport.requestAt(1);
  assert.deepEqual(pageRequest.params, {
    afterSequence: 1,
    limit: 2,
    sessionId,
  });
  pagingTransport.send(success(pageRequest.id, emptySessionExport(sessionId)));
  await page.result;
  await pagingClient.close();
});

test("empty array no-params calls match the generated method contract", async () => {
  const transport = new FakeTransport();
  const client = new DeviceRailClient(transport);
  await performHello(client, transport);

  const requestIndex = transport.writable.frames.length;
  const handle = client.beginCall("system.describe", []);
  const request = await transport.requestAt(requestIndex);
  assert.deepEqual(request.params, []);
  transport.send(success(request.id, describeResult(1)));
  await handle.result;
  assert.throws(
    () => client.beginCall("system.describe", [1] as never),
    ProtocolViolationError,
  );
  await client.close();
});

test("terminal cleanup guards late child stream errors", async () => {
  const transport = new FakeTransport();
  const client = new DeviceRailClient(transport);
  const pending = client.hello(helloParams());
  const request = await transport.requestAt(0);
  transport.send({ id: request.id, jsonrpc: "1.0", result: {} });
  await assert.rejects(pending, ProtocolViolationError);

  assert.doesNotThrow(() => transport.readable.emit("error", new Error("late stdout")));
  assert.doesNotThrow(() => transport.stderr.emit("error", new Error("late stderr")));
  assert.doesNotThrow(() => transport.writable.emit("error", new Error("late stdin")));
});

test("an idle writable failure immediately fails the client", async (context) => {
  for (const event of ["error", "close", "finish"] as const) {
    await context.test(event, async () => {
      const transport = new FakeTransport();
      const client = new DeviceRailClient(transport);
      await performHello(client, transport);
      await nextTurn();

      if (event === "error") {
        transport.writable.emit("error", new Error("idle stdin failure"));
      } else if (event === "close") {
        transport.writable.emit("close");
      } else {
        transport.writable.emit("finish");
      }
      await nextTurn();
      assert.equal(client.state, "failed");
      assert.equal(client.pendingRequests, 0);
      assert.equal(transport.terminated, true);
    });
  }
});

test("spawn preserves a hello error and closes the rejected child process", async () => {
  const childScript = String.raw`
    process.stdin.setEncoding("utf8");
    let buffered = "";
    process.stdin.on("data", (chunk) => {
      buffered += chunk;
      const newline = buffered.indexOf("\n");
      if (newline < 0) return;
      const request = JSON.parse(buffered.slice(0, newline));
      process.stdout.write(JSON.stringify({
        error: {
          code: -32000,
          data: { code: "hello_rejected", message: "no", retryable: false },
          message: "hello rejected"
        },
        id: request.id,
        jsonrpc: "2.0"
      }) + "\n");
    });
    process.stdin.on("end", () => process.exit(0));
    setTimeout(() => process.exit(97), 5000);
  `;

  await assert.rejects(
    DeviceRailClient.spawn({
      args: ["-e", childScript],
      closeGraceMs: 1_000,
      command: process.execPath,
      hello: helloParams(),
    }),
    (error: unknown) =>
      error instanceof RpcRemoteError && error.rpcError.data.code === "hello_rejected",
  );
});
