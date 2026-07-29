import assert from "node:assert/strict";
import test from "node:test";

import { EventStreamClosedError, type EventStreamItem } from "@devicerail/client";
import type {
  EventStreamCursor,
  EventsStreamTerminalParams,
  EventsSubscribeParams,
  TestEvent,
} from "@devicerail/protocol";

import {
  LiveVisualizerController,
  type LiveVisualizerClient,
  type LiveVisualizerStream,
} from "../src/controller.js";

const SESSION_ID = "11111111-1111-4111-8111-111111111111";
const STREAM_EPOCH = "22222222-2222-4222-8222-222222222222";
const SUBSCRIPTION_ID = "33333333-3333-4333-8333-333333333333";

function event(sequence: number, payload: TestEvent["payload"]): TestEvent {
  return {
    atMs: 100 + sequence,
    eventId: `aaaaaaaa-aaaa-4aaa-8aaa-${String(sequence).padStart(12, "0")}`,
    payload,
    sequence,
    sessionId: SESSION_ID,
  };
}

function cursor(sequence: number): EventStreamCursor {
  return { sequence, sessionId: SESSION_ID, streamEpoch: STREAM_EPOCH };
}

class FakeStream implements LiveVisualizerStream {
  #cancelled = false;
  #terminal: EventsStreamTerminalParams | undefined;
  readonly #events: readonly TestEvent[];
  readonly #failure: Error | undefined;
  readonly #confirmationFailure: Error | undefined;
  readonly #itemCursor: ((event: TestEvent) => EventStreamCursor) | undefined;
  readonly #confirmationCursor: ((event: TestEvent) => EventStreamCursor) | undefined;
  readonly #onConfirm: (sequence: number) => void;

  constructor(options: {
    readonly events: readonly TestEvent[];
    readonly failure?: Error;
    readonly confirmationFailure?: Error;
    readonly itemCursor?: (event: TestEvent) => EventStreamCursor;
    readonly confirmationCursor?: (event: TestEvent) => EventStreamCursor;
    readonly onConfirm: (sequence: number) => void;
    readonly terminal?: "sessionEnded";
  }) {
    this.#events = options.events;
    this.#failure = options.failure;
    this.#confirmationFailure = options.confirmationFailure;
    this.#itemCursor = options.itemCursor;
    this.#confirmationCursor = options.confirmationCursor;
    this.#onConfirm = options.onConfirm;
    if (options.terminal === "sessionEnded") {
      const last = options.events.at(-1);
      this.#terminal = {
        ...(last ? { lastEmittedCursor: cursor(last.sequence) } : {}),
        sessionId: SESSION_ID,
        subscriptionId: SUBSCRIPTION_ID,
        termination: { reason: "sessionEnded" },
      };
    }
  }

  get terminal(): EventsStreamTerminalParams | undefined {
    return this.#terminal;
  }

  cancel(): void {
    this.#cancelled = true;
  }

  async *[Symbol.asyncIterator](): AsyncIterator<EventStreamItem> {
    for (const current of this.#events) {
      if (this.#cancelled) return;
      const currentCursor = this.#itemCursor?.(current) ?? cursor(current.sequence);
      yield Object.freeze({
        cursor: currentCursor,
        event: structuredClone(current),
        confirm: () => {
          this.#onConfirm(current.sequence);
          if (this.#confirmationFailure) throw this.#confirmationFailure;
          return { ...(this.#confirmationCursor?.(current) ?? currentCursor) };
        },
      });
    }
    if (this.#failure) throw this.#failure;
  }
}

class FakeClient implements LiveVisualizerClient {
  readonly calls: EventsSubscribeParams[] = [];
  closeCalls = 0;
  readonly #streams: LiveVisualizerStream[];
  state = "ready";

  constructor(streams: readonly LiveVisualizerStream[]) {
    this.#streams = [...streams];
  }

  async openEventStream(params: EventsSubscribeParams): Promise<LiveVisualizerStream> {
    this.calls.push(structuredClone(params));
    const stream = this.#streams.shift();
    if (!stream) throw new Error("no fake stream remains");
    return stream;
  }

  close(): void {
    this.closeCalls += 1;
  }
}

test("model commit precedes daemon confirmation and revision publication follows model confirmation", async () => {
  let controller!: LiveVisualizerController;
  const confirmationStates: Array<{ pending: number | undefined; confirmed: number | undefined }> = [];
  const published: number[] = [];
  const events = [
    event(1, { type: "sessionStarted" }),
    event(2, {
      error: { code: "test", message: "recorded", retryable: false },
      type: "error",
    }),
    event(3, { outcome: "completed", type: "sessionEnded" }),
  ];
  const stream = new FakeStream({
    events,
    onConfirm: (sequence) => {
      const state = controller.state();
      confirmationStates.push({
        confirmed: state.confirmedSequence,
        pending: state.pending?.sequence,
      });
      assert.equal(state.pending?.sequence, sequence, "model commit must precede item.confirm");
    },
    terminal: "sessionEnded",
  });
  const client = new FakeClient([stream]);
  controller = new LiveVisualizerController(client, SESSION_ID, {
    resumeInitialDelayMs: 1,
    resumeMaxDelayMs: 1,
  });
  controller.subscribe((revision) => {
    published.push(revision);
    const state = controller.state();
    if (state.transport.phase === "streaming" && state.confirmedSequence !== undefined) {
      assert.equal(state.pending, undefined, "publication must follow model.confirm");
    }
  });
  controller.start();
  await controller.waitUntilStopped();

  assert.deepEqual(
    confirmationStates,
    [
      { confirmed: undefined, pending: 1 },
      { confirmed: 1, pending: 2 },
      { confirmed: 2, pending: 3 },
    ],
  );
  assert.equal(controller.state().confirmedSequence, 3);
  assert.equal(controller.state().status, "sessionEnded");
  assert.equal(controller.state().transport.phase, "sessionEnded");
  assert.ok(published.length >= 5);
  await controller.stop();
  assert.equal(client.closeCalls, 0, "viewer teardown must not close the host client");
});

test("retry opens strictly after the last confirmed cursor", async () => {
  const confirmed: number[] = [];
  const first = new FakeStream({
    events: [event(1, { type: "sessionStarted" })],
    failure: new EventStreamClosedError(1006, "test disconnect"),
    onConfirm: (sequence) => confirmed.push(sequence),
  });
  const second = new FakeStream({
    events: [
      event(2, {
        error: { code: "test", message: "after resume", retryable: false },
        type: "error",
      }),
      event(3, { outcome: "completed", type: "sessionEnded" }),
    ],
    onConfirm: (sequence) => confirmed.push(sequence),
    terminal: "sessionEnded",
  });
  const client = new FakeClient([first, second]);
  const controller = new LiveVisualizerController(client, SESSION_ID, {
    resumeInitialDelayMs: 1,
    resumeMaxDelayMs: 1,
  });
  controller.start();
  await controller.waitUntilStopped();

  assert.deepEqual(confirmed, [1, 2, 3]);
  assert.equal(client.calls.length, 2);
  assert.equal(client.calls[0]?.afterCursor, undefined);
  assert.deepEqual(client.calls[1]?.afterCursor, cursor(1));
  assert.equal(controller.state().confirmedSequence, 3);
  await controller.stop();
});

test("an unconfirmed replay is idempotent and a changed replay fails closed", async () => {
  const original = event(1, { type: "sessionStarted" });
  const first = new FakeStream({
    confirmationFailure: new EventStreamClosedError(1006, "confirmation interrupted"),
    events: [original],
    onConfirm: () => {},
  });
  const resumed = new FakeStream({
    events: [original, event(2, { outcome: "completed", type: "sessionEnded" })],
    onConfirm: () => {},
    terminal: "sessionEnded",
  });
  const client = new FakeClient([first, resumed]);
  const controller = new LiveVisualizerController(client, SESSION_ID, {
    resumeInitialDelayMs: 1,
    resumeMaxDelayMs: 1,
  });
  controller.start();
  await controller.waitUntilStopped();
  assert.equal(controller.state().confirmedSequence, 2);
  assert.equal(controller.state().status, "sessionEnded");
  assert.equal(client.calls.length, 2);
  assert.equal(client.calls[1]?.afterCursor, undefined);
  await controller.stop();

  const conflictingFirst = new FakeStream({
    confirmationFailure: new EventStreamClosedError(1006, "confirmation interrupted"),
    events: [original],
    onConfirm: () => {},
  });
  const conflict = new FakeStream({
    events: [{ ...original, atMs: original.atMs + 1 }],
    onConfirm: () => assert.fail("a conflicting replay must not be confirmed"),
  });
  const conflictingController = new LiveVisualizerController(
    new FakeClient([conflictingFirst, conflict]),
    SESSION_ID,
    { resumeInitialDelayMs: 1, resumeMaxDelayMs: 1 },
  );
  conflictingController.start();
  await conflictingController.waitUntilStopped();
  assert.equal(conflictingController.state().status, "failed");
  assert.equal(conflictingController.state().confirmedSequence, undefined);
  assert.equal(conflictingController.state().eventCount, 0);
  await conflictingController.stop();
});

test("capacity rejection never confirms or evicts the current event", async () => {
  const confirmed: number[] = [];
  const stream = new FakeStream({
    events: [
      event(1, { type: "sessionStarted" }),
      event(2, {
        error: { code: "capacity", message: "must remain unconfirmed", retryable: false },
        type: "error",
      }),
    ],
    onConfirm: (sequence) => confirmed.push(sequence),
  });
  const client = new FakeClient([stream]);
  const controller = new LiveVisualizerController(client, SESSION_ID, {
    timeline: { maxEvents: 1 },
  });
  controller.start();
  await controller.waitUntilStopped();

  assert.deepEqual(confirmed, [1]);
  const state = controller.state();
  assert.equal(state.confirmedSequence, 1);
  assert.equal(state.eventCount, 1);
  assert.equal(state.status, "viewerCapacityExceeded");
  assert.equal(state.transport.phase, "viewerCapacityExceeded");
  assert.equal(client.calls.length, 1);
  await controller.stop();
});

test("injected item and confirmation cursors cannot desynchronize model and resume", async () => {
  for (const mismatch of ["item", "confirmation"] as const) {
    let confirms = 0;
    const stream = new FakeStream({
      ...(mismatch === "confirmation"
        ? {
            confirmationCursor: (current: TestEvent) => ({
              ...cursor(current.sequence),
              streamEpoch: SUBSCRIPTION_ID,
            }),
          }
        : {}),
      events: [event(1, { type: "sessionStarted" })],
      ...(mismatch === "item"
        ? {
            itemCursor: (current: TestEvent) => ({
              ...cursor(current.sequence),
              sequence: current.sequence + 1,
            }),
          }
        : {}),
      onConfirm: () => {
        confirms += 1;
      },
    });
    const controller = new LiveVisualizerController(
      new FakeClient([stream]),
      SESSION_ID,
    );
    controller.start();
    await controller.waitUntilStopped();
    const state = controller.state();
    assert.equal(state.status, "failed", mismatch);
    assert.equal(state.confirmedSequence, undefined, mismatch);
    assert.equal(state.eventCount, 0, mismatch);
    assert.equal(confirms, mismatch === "confirmation" ? 1 : 0, mismatch);
    await controller.stop();
  }
});
