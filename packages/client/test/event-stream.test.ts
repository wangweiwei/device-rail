import assert from "node:assert/strict";
import test from "node:test";

import type {
  EventStreamCursor,
  EventStreamOriginPolicy,
  EventsStreamOpenResult,
} from "@devicerail/protocol";

import {
  EventStreamAbortedError,
  EventStreamClosedError,
  EventStreamCursorError,
  EventStreamQueueOverflowError,
  ProtocolViolationError,
  RequestAbortedError,
} from "../src/errors.js";
import {
  connectEventStream,
  type DeviceRailEventStream,
  type EventStreamOptions,
  type EventStreamWebSocket,
  type EventStreamWebSocketEvent,
  type EventStreamWebSocketEventType,
} from "../src/event-stream.js";

const SESSION_ID = "00000000-0000-4000-8000-000000000001";
const STREAM_EPOCH = "00000000-0000-4000-8000-000000000002";
const SUBSCRIPTION_ID = "00000000-0000-4000-8000-000000000003";
const ENDPOINT = `ws://127.0.0.1:45123/events/${"a".repeat(64)}`;

interface SentMessage {
  readonly id: string;
  readonly jsonrpc: "2.0";
  readonly method: string;
  readonly params: Record<string, unknown>;
}

class FakeWebSocket implements EventStreamWebSocket {
  readonly protocol = "devicerail.events.v1";
  readonly sent: string[] = [];
  readonly closes: Array<{ readonly code?: number; readonly reason?: string }> = [];
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

  close(code?: number, reason?: string): void {
    this.closes.push({ ...(code === undefined ? {} : { code }), ...(reason ? { reason } : {}) });
  }

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
    this.#emit("message", { data: typeof value === "string" ? value : JSON.stringify(value) });
  }

  closeFromServer(code = 1006, reason = "connection lost"): void {
    this.#emit("close", { code, reason });
  }

  sentAt(index: number): SentMessage {
    const serialized = this.sent[index];
    assert.ok(serialized, `sent WebSocket message ${index} must exist`);
    return JSON.parse(serialized) as SentMessage;
  }

  #emit(type: EventStreamWebSocketEventType, event: EventStreamWebSocketEvent): void {
    for (const listener of this.#listeners.get(type) ?? []) {
      listener(event);
    }
  }
}

interface Harness {
  readonly capabilityCalls: Array<{
    readonly originPolicy: EventStreamOriginPolicy;
    readonly sessionId: string;
  }>;
  readonly opener: (
    sessionId: string,
    originPolicy: EventStreamOriginPolicy,
  ) => Promise<EventsStreamOpenResult>;
  readonly sockets: FakeWebSocket[];
  readonly subprotocols: string[];
}

function harness(): Harness {
  const capabilityCalls: Harness["capabilityCalls"] = [];
  const sockets: FakeWebSocket[] = [];
  const subprotocols: string[] = [];
  return {
    capabilityCalls,
    sockets,
    subprotocols,
    opener: async (sessionId, originPolicy) => {
      capabilityCalls.push({ originPolicy, sessionId });
      return {
        endpoint: ENDPOINT,
        expiresAtMs: Date.now() + 30_000,
        streamEpoch: STREAM_EPOCH,
      };
    },
  };
}

function cursor(sequence: number): EventStreamCursor {
  return { sequence, sessionId: SESSION_ID, streamEpoch: STREAM_EPOCH };
}

function protocol(minor: 3 | 4 | 5 = 3) {
  return { major: 1, minor } as const;
}

function helloResponse(minor: 3 | 4 | 5 = 3): unknown {
  return {
    id: "devicerail:event-stream:hello",
    jsonrpc: "2.0",
    result: {
      connectionId: "00000000-0000-4000-8000-000000000004",
      features: { enabled: ["events.stream.v1"] },
      protocol: { selected: protocol(minor) },
      server: { name: "devicerail-daemon", version: "0.1.0" },
      transport: { framing: "jsonMessage", kind: "webSocket" },
    },
  };
}

interface SubscribeFixture {
  readonly replayThroughSequence?: number;
  readonly sessionState?: "active" | "ended";
}

function subscribeResponse(
  afterCursor?: EventStreamCursor,
  fixture: SubscribeFixture = {},
): unknown {
  return {
    id: "devicerail:event-stream:subscribe",
    jsonrpc: "2.0",
    result: {
      replayThrough: cursor(fixture.replayThroughSequence ?? afterCursor?.sequence ?? 1),
      sessionId: SESSION_ID,
      sessionState: fixture.sessionState ?? "active",
      subscriptionId: SUBSCRIPTION_ID,
    },
  };
}

function eventNotification(sequence: number, extraEvent: Record<string, unknown> = {}): unknown {
  return {
    jsonrpc: "2.0",
    method: "events.stream.event",
    params: {
      cursor: cursor(sequence),
      event: {
        atMs: sequence,
        eventId: `00000000-0000-4000-8000-${String(sequence).padStart(12, "0")}`,
        payload: { type: "sessionStarted" },
        sequence,
        sessionId: SESSION_ID,
        ...extraEvent,
      },
      subscriptionId: SUBSCRIPTION_ID,
    },
  };
}

function terminalNotification(lastSequence?: number, reason = "sessionEnded"): unknown {
  return {
    jsonrpc: "2.0",
    method: "events.stream.terminal",
    params: {
      ...(lastSequence === undefined ? {} : { lastEmittedCursor: cursor(lastSequence) }),
      sessionId: SESSION_ID,
      subscriptionId: SUBSCRIPTION_ID,
      termination: { reason },
    },
  };
}

async function nextTurn(): Promise<void> {
  await new Promise<void>((resolve) => setImmediate(resolve));
}

async function start(
  state: Harness,
  afterCursor?: EventStreamCursor,
  options: EventStreamOptions = {},
  subscribeFixture: SubscribeFixture = {},
  protocolMinor: 3 | 4 | 5 = 3,
): Promise<{ readonly socket: FakeWebSocket; readonly stream: DeviceRailEventStream }> {
  const pending = connectEventStream(
    state.opener,
    { name: "event-stream-test", version: "1.0.0" },
    protocol(protocolMinor),
    { sessionId: SESSION_ID, ...(afterCursor ? { afterCursor } : {}) },
    {
      ...options,
      webSocketFactory: (endpoint, subprotocol) => {
        assert.equal(endpoint, ENDPOINT);
        state.subprotocols.push(subprotocol);
        const socket = new FakeWebSocket();
        state.sockets.push(socket);
        return socket;
      },
    },
  );
  await nextTurn();
  const socket = state.sockets.at(-1);
  assert.ok(socket);
  socket.open();
  const hello = socket.sentAt(0);
  assert.equal(hello.method, "system.hello");
  assert.deepEqual(hello.params.features, { required: ["events.stream.v1"] });
  assert.deepEqual(hello.params.protocol, {
    ranges: [{ major: 1, maxMinor: protocolMinor, minMinor: protocolMinor }],
  });
  socket.message(helloResponse(protocolMinor));
  const subscribe = socket.sentAt(1);
  assert.equal(subscribe.method, "events.subscribe");
  assert.deepEqual(subscribe.params, {
    sessionId: SESSION_ID,
    ...(afterCursor ? { afterCursor } : {}),
  });
  socket.message(subscribeResponse(afterCursor, subscribeFixture));
  return { socket, stream: await pending };
}

test("stream uses the required subprotocol and separates receipt, delivery, and confirmation", async () => {
  const state = harness();
  const { socket, stream } = await start(state);

  assert.deepEqual(state.subprotocols, ["devicerail.events.v1"]);
  assert.equal(stream.receivedCursor, undefined);
  assert.equal(stream.confirmedCursor, undefined);

  const firstPending = stream.next();
  socket.message(eventNotification(1));
  const first = await firstPending;
  assert.equal(first.done, false);
  assert.deepEqual(stream.receivedCursor, cursor(1));
  assert.equal(stream.confirmedCursor, undefined);
  assert.throws(() => stream.confirm(cursor(2)), EventStreamCursorError);
  assert.deepEqual(first.value?.confirm(), cursor(1));
  assert.deepEqual(stream.confirmedCursor, cursor(1));
  assert.throws(() => first.value?.confirm(), EventStreamCursorError);

  socket.message(
    eventNotification(2, {
      payload: { outcome: "completed", type: "sessionEnded" },
    }),
  );
  socket.message(terminalNotification(2));
  const second = await stream.next();
  assert.equal(second.value?.event.sequence, 2);
  assert.deepEqual(second.value?.confirm(), cursor(2));
  assert.deepEqual(await stream.next(), { done: true, value: undefined });
  assert.equal(stream.terminal?.termination.reason, "sessionEnded");
});

test("event stream accepts protocol 1.5", async () => {
  const state = harness();
  const { socket, stream } = await start(state, undefined, {}, {}, 5);
  socket.message(
    eventNotification(1, {
      payload: { outcome: "completed", type: "sessionEnded" },
    }),
  );
  socket.message(terminalNotification(1));

  const event = await stream.next();
  assert.equal(event.value?.event.sequence, 1);
  assert.deepEqual(event.value?.confirm(), cursor(1));
  assert.deepEqual(await stream.next(), { done: true, value: undefined });
});

test("protocol 1.5 validates UI Snapshot references and execution channels while 1.4 rejects them", async () => {
  const digest = "b".repeat(64);
  const context = {
    contextKind: "web",
    contextId: "WEBVIEW_1",
    documentEpoch: "document-7",
  };
  const observation = {
    capturedAtMs: 10,
    deviceId: "ios-1",
    id: "00000000-0000-4000-8000-000000000077",
    metadata: {},
    screenshot: null,
    uiSnapshot: {
      formatVersion: 1,
      context,
      nodeCount: 2,
      byteLength: 512,
      evidence: {
        id: `sha256:${digest}`,
        mediaType: "application/vnd.devicerail.ui-tree+json;version=1",
        sha256: digest,
        uri: `devicerail://assets/sha256/${digest}`,
      },
    },
    viewport: { height: 844, scaleFactor: 3, width: 390 },
  };

  const current = harness();
  const { socket, stream } = await start(current, undefined, {}, {}, 5);
  socket.message(eventNotification(1, {
    payload: { observation, type: "observationCaptured" },
  }));
  const item = await stream.next();
  assert.equal(item.value?.event.payload.type, "observationCaptured");

  const legacy = harness();
  const legacyStream = await start(legacy, undefined, {}, {}, 4);
  legacyStream.socket.message(eventNotification(1, {
    payload: { observation, type: "observationCaptured" },
  }));
  await assert.rejects(legacyStream.stream.next(), ProtocolViolationError);
  assert.equal(legacyStream.socket.closes.at(-1)?.code, 1002);

  const malformed = harness();
  const malformedStream = await start(malformed, undefined, {}, {}, 5);
  malformedStream.socket.message(eventNotification(1, {
    payload: {
      observation: {
        ...observation,
        uiSnapshot: {
          ...observation.uiSnapshot,
          context: { ...context, documentEpoch: "" },
        },
      },
      type: "observationCaptured",
    },
  }));
  await assert.rejects(malformedStream.stream.next(), ProtocolViolationError);
  assert.equal(malformedStream.socket.closes.at(-1)?.code, 1002);

  const emptyTree = harness();
  const emptyTreeStream = await start(emptyTree, undefined, {}, {}, 5);
  emptyTreeStream.socket.message(eventNotification(1, {
    payload: {
      observation: {
        ...observation,
        uiSnapshot: { ...observation.uiSnapshot, nodeCount: 0 },
      },
      type: "observationCaptured",
    },
  }));
  await assert.rejects(emptyTreeStream.stream.next(), ProtocolViolationError);
  assert.equal(emptyTreeStream.socket.closes.at(-1)?.code, 1002);
});

test("event stream accepts ordered Evidence-referenced media lifecycle events", async () => {
  const state = harness();
  const { socket, stream } = await start(state, undefined, {}, {}, 4);
  const streamId = "00000000-0000-4000-8000-000000000099";
  const digest = "a".repeat(64);
  const evidence = {
    id: `sha256:${digest}`,
    mediaType: "video/webm",
    sha256: digest,
    uri: `devicerail://assets/sha256/${digest}`,
  };
  const payloads = [
    { type: "sessionStarted" },
    {
      type: "mediaStreamStarted",
      stream: { id: streamId, kind: "video", mediaType: "video/webm" },
    },
    {
      type: "mediaFrameCaptured",
      frame: { streamId, frameIndex: 1, keyFrame: true, durationMs: 100, evidence },
    },
    { type: "mediaStreamEnded", streamId, frameCount: 1 },
    { type: "sessionEnded", outcome: "completed" },
  ];
  for (const [index, payload] of payloads.entries()) {
    const sequence = index + 1;
    socket.message(eventNotification(sequence, { payload }));
    const item = await stream.next();
    assert.equal(item.done, false);
    assert.equal(item.value?.event.payload.type, payload.type);
    item.value?.confirm();
  }
  socket.message(terminalNotification(5));
  assert.deepEqual(await stream.next(), { done: true, value: undefined });
});

test("protocol 1.3 rejects protocol 1.4 media payloads", async () => {
  const state = harness();
  const { socket, stream } = await start(state);
  socket.message(eventNotification(1, {
    payload: {
      type: "mediaStreamStarted",
      stream: {
        id: "00000000-0000-4000-8000-000000000099",
        kind: "video",
        mediaType: "video/webm",
      },
    },
  }));
  await assert.rejects(stream.next(), ProtocolViolationError);
  assert.equal(socket.closes.at(-1)?.code, 1002);
});

test("WebSocket hello must select the exact control-connection protocol", async () => {
  const state = harness();
  const opening = connectEventStream(
    state.opener,
    { name: "event-stream-test", version: "1.0.0" },
    protocol(3),
    { sessionId: SESSION_ID },
    {
      webSocketFactory: () => {
        const socket = new FakeWebSocket();
        state.sockets.push(socket);
        return socket;
      },
    },
  );
  await nextTurn();
  const socket = state.sockets[0];
  assert.ok(socket);
  socket.open();
  assert.deepEqual(socket.sentAt(0).params.protocol, {
    ranges: [{ major: 1, maxMinor: 3, minMinor: 3 }],
  });
  socket.message(helloResponse(4));
  await assert.rejects(opening, ProtocolViolationError);
  assert.equal(socket.closes.at(-1)?.code, 1002);
});

test("WebSocket hello runs the canonical response Schema before semantic checks", async () => {
  const state = harness();
  const opening = connectEventStream(
    state.opener,
    { name: "event-stream-test", version: "1.0.0" },
    protocol(),
    { sessionId: SESSION_ID },
    {
      webSocketFactory: () => {
        const socket = new FakeWebSocket();
        state.sockets.push(socket);
        return socket;
      },
    },
  );
  await nextTurn();
  const socket = state.sockets[0];
  assert.ok(socket);
  socket.open();
  socket.message({
    id: "devicerail:event-stream:hello",
    jsonrpc: "2.0",
    result: {
      connectionId: "00000000-0000-4000-8000-000000000004",
      features: { enabled: ["events.stream.v1"] },
      protocol: { selected: protocol() },
      server: { name: "devicerail-daemon", version: 1 },
      transport: { framing: "jsonMessage", kind: "webSocket" },
    },
  });
  await assert.rejects(opening, (error) => {
    assert.ok(error instanceof ProtocolViolationError);
    assert.match(error.message, /^system\.hello response was rejected at /u);
    return true;
  });
  assert.equal(socket.sent.length, 1, "Schema rejection must prevent subscription");
  assert.equal(socket.closes.at(-1)?.code, 1002);
});

test("normal termination rejects an incomplete replay prefix and a missing sessionEnded event", async () => {
  const replayState = harness();
  const replay = await start(replayState);
  replay.socket.message(terminalNotification());
  await assert.rejects(replay.stream.next(), EventStreamCursorError);

  const endingState = harness();
  const ending = await start(endingState);
  ending.socket.message(eventNotification(1));
  ending.socket.message(terminalNotification(1));
  assert.equal((await ending.stream.next()).value?.event.payload.type, "sessionStarted");
  await assert.rejects(ending.stream.next(), ProtocolViolationError);

  const cursorState = harness();
  const cursorMismatch = await start(cursorState);
  const finalEvent = cursorMismatch.stream.next();
  cursorMismatch.socket.message(
    eventNotification(1, {
      payload: { outcome: "completed", type: "sessionEnded" },
    }),
  );
  assert.equal((await finalEvent).value?.event.payload.type, "sessionEnded");
  cursorMismatch.socket.message(terminalNotification());
  await assert.rejects(cursorMismatch.stream.next(), EventStreamCursorError);
});

test("an ended subscription requires a final sessionEnded event only when this connection owes replay", async () => {
  const owedState = harness();
  const owed = await start(owedState, cursor(1), {}, {
    replayThroughSequence: 2,
    sessionState: "ended",
  });
  owed.socket.message(eventNotification(2));
  owed.socket.message(terminalNotification(2));
  assert.equal((await owed.stream.next()).value?.event.payload.type, "sessionStarted");
  await assert.rejects(owed.stream.next(), ProtocolViolationError);

  const completeState = harness();
  const complete = await start(completeState, cursor(2), {}, {
    replayThroughSequence: 2,
    sessionState: "ended",
  });
  complete.socket.message(terminalNotification());
  assert.deepEqual(await complete.stream.next(), { done: true, value: undefined });
});

test("schema-open nested event DTOs accept extension fields while unsafe values still fail", async () => {
  const state = harness();
  const { socket, stream } = await start(state);
  const observation = {
    capturedAtMs: 1,
    deviceId: "mock-1",
    futureObservationField: { enabled: true },
    id: "00000000-0000-4000-8000-000000000101",
    metadata: { nested: { count: 1 } },
    screenshot: {
      futureAssetField: "accepted",
      id: "asset-1",
      mediaType: "image/png",
      uri: "devicerail://assets/asset-1",
    },
    viewport: {
      futureViewportField: [1, 2, 3],
      height: 720,
      scaleFactor: 1,
      width: 1280,
    },
  };
  socket.message(
    eventNotification(1, {
      payload: { observation, type: "observationCaptured" },
    }),
  );
  socket.message(
    eventNotification(2, {
      payload: {
        call: {
          arguments: { x: 1 },
          futureCallField: { source: "extension" },
          id: "00000000-0000-4000-8000-000000000102",
          name: "tap",
        },
        type: "actionStarted",
      },
    }),
  );
  socket.message(
    eventNotification(3, {
      payload: {
        callId: "00000000-0000-4000-8000-000000000102",
        outcome: {
          outcome: "succeeded",
          result: {
            callId: "00000000-0000-4000-8000-000000000102",
            evidence: [
              {
                futureAssetField: { version: 2 },
                id: "asset-2",
                mediaType: "application/json",
                uri: "devicerail://assets/asset-2",
              },
            ],
            finishedAtMs: 3,
            futureResultField: { durationClass: "short" },
            output: { ok: true },
            startedAtMs: 2,
          },
        },
        type: "actionCompleted",
      },
    }),
  );
  socket.message(
    eventNotification(4, {
      payload: { outcome: "completed", type: "sessionEnded" },
    }),
  );
  socket.message(terminalNotification(4));

  const payloadTypes: string[] = [];
  for await (const item of stream) {
    payloadTypes.push(item.event.payload.type);
  }
  assert.deepEqual(payloadTypes, [
    "observationCaptured",
    "actionStarted",
    "actionCompleted",
    "sessionEnded",
  ]);

  const unsafeState = harness();
  const unsafe = await start(unsafeState);
  unsafe.socket.message(
    eventNotification(1, {
      payload: {
        observation: {
          ...observation,
          viewport: {
            ...observation.viewport,
            futureUnsafeInteger: Number.MAX_SAFE_INTEGER + 1,
          },
        },
        type: "observationCaptured",
      },
    }),
  );
  await assert.rejects(unsafe.stream.next(), ProtocolViolationError);
});

test("resume opens a fresh capability from only the last explicitly confirmed cursor", async () => {
  const state = harness();
  const { socket, stream } = await start(state, cursor(2));
  assert.deepEqual(stream.confirmedCursor, cursor(2));

  const pending = stream.next();
  socket.message(eventNotification(3));
  const third = await pending;
  third.value?.confirm();
  socket.closeFromServer(1006, "network reset");
  await assert.rejects(stream.next(), EventStreamClosedError);

  const resumedPending = stream.resume();
  await nextTurn();
  const resumedSocket = state.sockets[1];
  assert.ok(resumedSocket);
  resumedSocket.open();
  resumedSocket.message(helloResponse());
  assert.deepEqual(resumedSocket.sentAt(1).params, {
    afterCursor: cursor(3),
    sessionId: SESSION_ID,
  });
  resumedSocket.message(subscribeResponse(cursor(3)));
  const resumed = await resumedPending;
  assert.deepEqual(resumed.confirmedCursor, cursor(3));
  assert.equal(state.capabilityCalls.length, 2);
  resumed.cancel();
});

test("receive queue enforces event and byte bounds without advancing confirmation", async () => {
  const countState = harness();
  const { socket, stream } = await start(countState, undefined, {
    maxQueuedEvents: 1,
    maxQueuedBytes: 8_192,
  });
  socket.message(eventNotification(1));
  socket.message(eventNotification(2));
  assert.equal(socket.closes.at(-1)?.code, 1002);
  assert.equal((await stream.next()).value?.event.sequence, 1);
  await assert.rejects(stream.next(), EventStreamQueueOverflowError);
  assert.equal(stream.confirmedCursor, undefined);
  assert.deepEqual(stream.receivedCursor, cursor(2));

  const byteState = harness();
  const byteStream = await start(byteState, undefined, {
    maxMessageBytes: 8_192,
    maxQueuedBytes: 128,
    maxQueuedEvents: 8,
  });
  byteStream.socket.message(eventNotification(1));
  await assert.rejects(byteStream.stream.next(), EventStreamQueueOverflowError);
});

test("sequence gaps, unknown fields, unknown messages, and early close fail explicitly", async () => {
  const gapState = harness();
  const gap = await start(gapState);
  gap.socket.message(eventNotification(2));
  await assert.rejects(gap.stream.next(), EventStreamCursorError);

  const fieldState = harness();
  const field = await start(fieldState);
  field.socket.message(eventNotification(1, { unknown: true }));
  await assert.rejects(field.stream.next(), ProtocolViolationError);

  const unknownState = harness();
  const unknown = await start(unknownState);
  unknown.socket.message({ jsonrpc: "2.0", method: "events.stream.future", params: {} });
  await assert.rejects(unknown.stream.next(), ProtocolViolationError);

  const earlyState = harness();
  const opening = connectEventStream(
    earlyState.opener,
    { name: "event-stream-test", version: "1.0.0" },
    protocol(),
    { sessionId: SESSION_ID },
    {
      webSocketFactory: (_endpoint, subprotocol) => {
        earlyState.subprotocols.push(subprotocol);
        const socket = new FakeWebSocket();
        earlyState.sockets.push(socket);
        return socket;
      },
    },
  );
  await nextTurn();
  const earlySocket = earlyState.sockets[0];
  assert.ok(earlySocket);
  earlySocket.open();
  earlySocket.closeFromServer(1002, "hello required");
  await assert.rejects(opening, EventStreamClosedError);
});

test("AbortSignal is explicit, closes locally, and a pre-aborted signal issues no capability", async () => {
  const state = harness();
  const controller = new AbortController();
  const { socket, stream } = await start(state, undefined, { signal: controller.signal });
  const pending = stream.next();
  controller.abort();
  await assert.rejects(pending, EventStreamAbortedError);
  assert.deepEqual(socket.closes.at(-1), { code: 1000, reason: "client aborted" });

  const preAborted = new AbortController();
  preAborted.abort();
  await assert.rejects(
    connectEventStream(
      state.opener,
      { name: "event-stream-test", version: "1.0.0" },
      protocol(),
      { sessionId: SESSION_ID },
      { signal: preAborted.signal },
    ),
    RequestAbortedError,
  );
  assert.equal(state.capabilityCalls.length, 1);
});

test("AbortSignal promptly settles a pending capability request and absorbs its late result", async () => {
  const controller = new AbortController();
  let resolveCapability!: (result: EventsStreamOpenResult) => void;
  const capability = new Promise<EventsStreamOpenResult>((resolve) => {
    resolveCapability = resolve;
  });
  let factoryCalls = 0;
  const opening = connectEventStream(
    async () => await capability,
    { name: "event-stream-test", version: "1.0.0" },
    protocol(),
    { sessionId: SESSION_ID },
    {
      signal: controller.signal,
      webSocketFactory: () => {
        factoryCalls += 1;
        return new FakeWebSocket();
      },
    },
  );
  controller.abort();
  await assert.rejects(opening, EventStreamAbortedError);
  resolveCapability({
    endpoint: ENDPOINT,
    expiresAtMs: Date.now() + 30_000,
    streamEpoch: STREAM_EPOCH,
  });
  await nextTurn();
  assert.equal(factoryCalls, 0);
});

test("breaking async iteration cancels the socket without confirming the delivered event", async () => {
  const state = harness();
  const { socket, stream } = await start(state);
  socket.message(eventNotification(1));
  for await (const item of stream) {
    assert.equal(item.event.sequence, 1);
    break;
  }
  assert.equal(stream.confirmedCursor, undefined);
  assert.deepEqual(socket.closes.at(-1), { code: 1000, reason: "client aborted" });
  assert.deepEqual(await stream.next(), { done: true, value: undefined });
});

test("exact browser Origin is canonical, loopback-only, and passed to capability issuance", async () => {
  const state = harness();
  const originPolicy = { kind: "exact", origin: "http://127.0.0.1:4173" } as const;
  const { stream } = await start(state, undefined, { originPolicy });
  assert.deepEqual(state.capabilityCalls[0], { originPolicy, sessionId: SESSION_ID });
  stream.cancel();

  for (const origin of [
    "http://localhost:4173",
    "http://127.0.0.1",
    "http://127.0.0.1:04173",
    "http://127.0.0.1:4173/",
    "http://127.0.0.1:65536",
    "http://127.0.0.1:80",
    "https://127.0.0.1:443",
  ]) {
    await assert.rejects(
      connectEventStream(
        state.opener,
        { name: "event-stream-test", version: "1.0.0" },
        protocol(),
        { sessionId: SESSION_ID },
        { originPolicy: { kind: "exact", origin } },
      ),
      ProtocolViolationError,
    );
  }
});
