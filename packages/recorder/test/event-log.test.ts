import assert from "node:assert/strict";
import test from "node:test";

import type {
  ActionOutcome,
  TestEvent,
  TestEventPayload,
} from "@devicerail/protocol";

import { RecorderError } from "../src/errors.js";
import { EventLog, validateTestEvent } from "../src/event-log.js";

const SESSION_ID = "11111111-1111-4111-8111-111111111111";
const OTHER_SESSION_ID = "22222222-2222-4222-8222-222222222222";
const CALL_A = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaa1";
const CALL_B = "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbb2";
const MEDIA_STREAM = "cccccccc-cccc-4ccc-8ccc-ccccccccccc3";

function eventId(sequence: number): string {
  return `eeeeeeee-eeee-4eee-8eee-${sequence.toString().padStart(12, "0")}`;
}

function event(
  sequence: number,
  payload: TestEventPayload,
  options: {
    readonly deviceId?: string;
    readonly eventId?: string;
    readonly requestId?: string | number;
    readonly sessionId?: string;
  } = {},
): TestEvent {
  return {
    atMs: 100 - sequence,
    eventId: options.eventId ?? eventId(sequence),
    payload,
    sequence,
    sessionId: options.sessionId ?? SESSION_ID,
    ...(options.deviceId === undefined ? {} : { deviceId: options.deviceId }),
    ...(options.requestId === undefined ? {} : { requestId: options.requestId }),
  };
}

function started(sequence = 1): TestEvent {
  return event(sequence, { type: "sessionStarted" });
}

function frameEvidence(mediaType = "image/png") {
  const digest = "a".repeat(64);
  return {
    id: `sha256:${digest}`,
    mediaType,
    sha256: digest,
    uri: `devicerail://assets/sha256/${digest}`,
  };
}

function actionStarted(
  sequence: number,
  callId: string,
  options: {
    readonly arguments?: unknown;
    readonly argumentsRedacted?: true;
    readonly deviceId?: string;
    readonly requestId?: string | number;
  } = {},
): TestEvent {
  return event(
    sequence,
    {
      call: {
        arguments: Object.hasOwn(options, "arguments") ? options.arguments : {},
        id: callId,
        name: "tap",
        ...(options.argumentsRedacted === true ? { argumentsRedacted: true } : {}),
      },
      type: "actionStarted",
    },
    {
      ...(options.deviceId === undefined ? {} : { deviceId: options.deviceId }),
      ...(options.requestId === undefined ? {} : { requestId: options.requestId }),
    },
  );
}

function errorInfo(code: string) {
  return { code, details: null, message: code, retryable: false } as const;
}

function actionCompleted(
  sequence: number,
  callId: string,
  outcome: "succeeded" | "failed" | "cancelled" | "timedOut" = "succeeded",
  options: {
    readonly deviceId?: string;
    readonly requestId?: string | number;
    readonly resultCallId?: string;
  } = {},
): TestEvent {
  let terminal: ActionOutcome;
  if (outcome === "succeeded") {
    terminal = {
      outcome,
      result: {
        after: null,
        before: null,
        callId: options.resultCallId ?? callId,
        evidence: [],
        finishedAtMs: 20,
        output: { accepted: true },
        startedAtMs: 10,
      },
    };
  } else if (outcome === "timedOut") {
    terminal = { error: errorInfo("timed_out"), outcome, timeoutMs: 5 };
  } else {
    terminal = { error: errorInfo(outcome), outcome };
  }
  return event(
    sequence,
    { callId, outcome: terminal, type: "actionCompleted" },
    {
      ...(options.deviceId === undefined ? {} : { deviceId: options.deviceId }),
      ...(options.requestId === undefined ? {} : { requestId: options.requestId }),
    },
  );
}

function ended(sequence: number): TestEvent {
  return event(sequence, {
    outcome: "completed",
    reason: null,
    type: "sessionEnded",
  });
}

function errorEvent(sequence: number, id = eventId(sequence)): TestEvent {
  return event(
    sequence,
    { error: errorInfo("recorded_error"), type: "error" },
    { eventId: id },
  );
}

function throwsCode(block: () => unknown, code: RecorderError["code"]): void {
  assert.throws(
    block,
    (error: unknown) => error instanceof RecorderError && error.code === code,
  );
}

test("records concurrent Actions by sequence and preserves every explicit terminal outcome", () => {
  const log = new EventLog(SESSION_ID);
  const result = log.acceptBatch([
    started(),
    actionStarted(2, CALL_A, { deviceId: "mock-1", requestId: "request-a" }),
    actionStarted(3, CALL_B, { deviceId: "mock-2", requestId: 7 }),
    actionCompleted(4, CALL_B, "timedOut", { deviceId: "mock-2", requestId: 7 }),
    actionCompleted(5, CALL_A, "succeeded", {
      deviceId: "mock-1",
      requestId: "request-a",
    }),
    actionStarted(6, "cccccccc-cccc-4ccc-8ccc-ccccccccccc3"),
    actionCompleted(7, "cccccccc-cccc-4ccc-8ccc-ccccccccccc3", "failed"),
    actionStarted(8, "dddddddd-dddd-4ddd-8ddd-ddddddddddd4"),
    actionCompleted(9, "dddddddd-dddd-4ddd-8ddd-ddddddddddd4", "cancelled"),
    ended(10),
  ]);

  assert.deepEqual(result, {
    accepted: 10,
    duplicates: 0,
    lastSequence: 10,
    terminal: true,
  });
  assert.equal(log.openActionCount, 0);
  assert.equal(log.nextSequence, 11);
  assert.deepEqual(
    log.events
      .filter((entry) => entry.payload.type === "actionCompleted")
      .map((entry) =>
        entry.payload.type === "actionCompleted" ? entry.payload.outcome.outcome : assert.fail(),
      ),
    ["timedOut", "succeeded", "failed", "cancelled"],
  );
});

test("exact repeated delivery is idempotent and retained events are immutable snapshots", () => {
  const raw = started();
  const log = new EventLog(SESSION_ID);
  assert.equal(log.accept(raw), "accepted");
  raw.atMs = 999;
  assert.equal(log.events[0]?.atMs, 99);
  assert.equal(Object.isFrozen(log.events), true);
  assert.equal(Object.isFrozen(log.events[0]), true);

  const reordered = {
    sessionId: SESSION_ID,
    sequence: 1,
    payload: { type: "sessionStarted" },
    eventId: eventId(1),
    atMs: 99,
  };
  assert.equal(log.accept(reordered), "duplicate");
  assert.equal(log.events.length, 1);
  assert.deepEqual(log.acceptBatch([reordered, errorEvent(2)]), {
    accepted: 1,
    duplicates: 1,
    lastSequence: 2,
    terminal: false,
  });
});

test("sequence conflict, gap, and out-of-order batches fail atomically", () => {
  const log = EventLog.replay(SESSION_ID, [started()]);
  throwsCode(() => log.accept({ ...started(), atMs: 98 }), "sequence_conflict");
  throwsCode(() => log.accept(errorEvent(3)), "sequence_gap");
  throwsCode(() => log.acceptBatch([errorEvent(3), errorEvent(2)]), "out_of_order");
  assert.equal(log.lastSequence, 1);
  assert.equal(log.events.length, 1);

  const accepted = log.acceptBatch([errorEvent(2), errorEvent(3)]);
  assert.equal(accepted.accepted, 2);
  assert.equal(log.lastSequence, 3);
});

test("forked scale append preserves historical event identity and branch isolation", () => {
  const historicalCount = 5_000;
  const history = [
    started(),
    ...Array.from({ length: historicalCount - 1 }, (_, index) => ({
      ...errorEvent(index + 2),
      atMs: index + 101,
    })),
  ];
  const log = EventLog.replay(SESSION_ID, history);
  const durablePrefix = log.events;
  const candidate = log.fork();
  const appended = { ...errorEvent(historicalCount + 1), atMs: historicalCount + 101 };

  assert.deepEqual(candidate.acceptBatch([appended]), {
    accepted: 1,
    duplicates: 0,
    lastSequence: historicalCount + 1,
    terminal: false,
  });
  assert.equal(log.events, durablePrefix);
  assert.equal(log.events.length, historicalCount);
  assert.equal(candidate.events.length, historicalCount + 1);
  for (let index = 0; index < historicalCount; index += 1) {
    assert.equal(
      candidate.events[index],
      durablePrefix[index],
      `historical event ${String(index + 1)} must be structurally shared`,
    );
  }
  assert.notEqual(candidate.events.at(-1), appended, "the new event remains a detached snapshot");

  const rejected = log.fork();
  throwsCode(
    () => rejected.accept({ ...errorEvent(historicalCount + 2), atMs: historicalCount + 102 }),
    "sequence_gap",
  );
  assert.equal(rejected.events, durablePrefix);
  assert.equal(log.events, durablePrefix);

  assert.equal(
    log.accept(appended),
    "accepted",
    "a fork must not reserve identity in its source",
  );
  assert.equal(log.lastSequence, historicalCount + 1);
});

test("rejects cross-Session events and eventId or callId reuse", () => {
  const log = EventLog.replay(SESSION_ID, [started()]);
  throwsCode(
    () => log.accept(event(2, { type: "sessionStarted" }, { sessionId: OTHER_SESSION_ID })),
    "session_mismatch",
  );
  throwsCode(() => log.accept(errorEvent(2, eventId(1))), "duplicate_event_id");

  log.accept(actionStarted(2, CALL_A));
  log.accept(actionCompleted(3, CALL_A));
  throwsCode(() => log.accept(actionStarted(4, CALL_A)), "action_call_reused");
  assert.equal(log.lastSequence, 3);
});

test("Action completion requires an open call and exact correlation/result ids", () => {
  const withoutStart = EventLog.replay(SESSION_ID, [started()]);
  throwsCode(() => withoutStart.accept(actionCompleted(2, CALL_A)), "action_not_started");

  const log = EventLog.replay(SESSION_ID, [
    started(),
    actionStarted(2, CALL_A, { deviceId: "mock-1", requestId: "request-a" }),
  ]);
  throwsCode(
    () =>
      log.accept(
        actionCompleted(3, CALL_A, "succeeded", {
          deviceId: "mock-1",
          requestId: "request-b",
        }),
      ),
    "action_correlation_mismatch",
  );
  throwsCode(
    () =>
      log.accept(
        actionCompleted(3, CALL_A, "succeeded", {
          deviceId: "mock-1",
          requestId: "request-a",
          resultCallId: CALL_B,
        }),
      ),
    "action_result_mismatch",
  );
  assert.equal(log.openActionCount, 1);
  assert.equal(log.lastSequence, 2);
  log.accept(
    actionCompleted(3, CALL_A, "succeeded", {
      deviceId: "mock-1",
      requestId: "request-a",
    }),
  );
  assert.equal(log.openActionCount, 0);
});

test("Session cannot end with an open Action and no new sequence follows its terminal event", () => {
  const log = EventLog.replay(SESSION_ID, [started(), actionStarted(2, CALL_A)]);
  throwsCode(() => log.accept(ended(3)), "action_in_flight");
  assert.equal(log.lastSequence, 2);
  log.accept(actionCompleted(3, CALL_A));
  const terminal = ended(4);
  log.accept(terminal);
  assert.equal(log.terminal, true);
  assert.equal(log.accept(structuredClone(terminal)), "duplicate");
  throwsCode(() => log.accept(errorEvent(5)), "terminal_append");
  assert.equal(log.lastSequence, 4);
});

test("media streams require ordered Evidence frames and a matching terminal count", () => {
  const log = new EventLog(SESSION_ID);
  log.accept(started());
  log.accept(event(2, {
    type: "mediaStreamStarted",
    stream: { id: MEDIA_STREAM, kind: "screenshot", mediaType: "image/png" },
  }));
  throwsCode(
    () => log.accept(event(3, { outcome: "completed", reason: null, type: "sessionEnded" })),
    "invalid_lifecycle",
  );
  throwsCode(
    () => log.accept(event(3, {
      type: "mediaFrameCaptured",
      frame: {
        streamId: MEDIA_STREAM,
        frameIndex: 2,
        evidence: frameEvidence(),
      },
    })),
    "invalid_lifecycle",
  );
  log.accept(event(3, {
    type: "mediaFrameCaptured",
    frame: {
      streamId: MEDIA_STREAM,
      frameIndex: 1,
      keyFrame: true,
      durationMs: 100,
      evidence: frameEvidence(),
    },
  }));
  throwsCode(
    () => log.accept(event(4, {
      type: "mediaStreamEnded",
      streamId: MEDIA_STREAM,
      frameCount: 2,
    })),
    "invalid_lifecycle",
  );
  log.accept(event(4, {
    type: "mediaStreamEnded",
    streamId: MEDIA_STREAM,
    frameCount: 1,
  }));
  throwsCode(
    () => log.accept(event(5, {
      type: "mediaStreamStarted",
      stream: { id: MEDIA_STREAM, kind: "screenshot", mediaType: "image/png" },
    })),
    "invalid_lifecycle",
  );
  log.accept(event(5, { outcome: "completed", reason: null, type: "sessionEnded" }));
  assert.equal(log.terminal, true);
});

test("runtime validation is fail-closed and keeps the canonical daemon event shape", () => {
  assert.deepEqual(validateTestEvent(started()), started());
  throwsCode(() => validateTestEvent(null), "invalid_event");
  throwsCode(
    () => validateTestEvent({ ...started(), unknown: true }),
    "invalid_event",
  );
  throwsCode(
    () => validateTestEvent({ ...started(), requestId: null }),
    "invalid_event",
  );
  throwsCode(
    () =>
      validateTestEvent(
        event(2, {
          call: { id: CALL_A, name: "tap" },
          type: "actionStarted",
        }),
      ),
    "invalid_event",
  );
  throwsCode(
    () =>
      validateTestEvent(
        event(2, {
          call: {
            arguments: { secret: "must-not-survive" },
            argumentsRedacted: true,
            id: CALL_A,
            name: "inputSecret",
          },
          type: "actionStarted",
        }),
      ),
    "invalid_event",
  );
  throwsCode(
    () =>
      validateTestEvent(
        event(2, {
          call: {
            arguments: null,
            argumentsRedacted: false,
            id: CALL_A,
            name: "tap",
          },
          type: "actionStarted",
        }),
      ),
    "invalid_event",
  );

  assert.equal(
    validateTestEvent(actionStarted(2, CALL_A, { arguments: null })).payload.type,
    "actionStarted",
  );
  assert.equal(
    validateTestEvent(
      actionStarted(2, CALL_A, { arguments: null, argumentsRedacted: true }),
    ).payload.type,
    "actionStarted",
  );
});

test("Verdict validation matches Protocol 1.5 summary and Evidence bounds", () => {
  const verdictEvent = (summary: string, evidence = [frameEvidence()]): TestEvent =>
    event(2, {
      type: "verdictRecorded",
      verdict: { status: "unknown", summary, evidence },
    });

  assert.equal(
    validateTestEvent(verdictEvent("🧪".repeat(16_384))).payload.type,
    "verdictRecorded",
  );
  throwsCode(() => validateTestEvent(verdictEvent(" \n\t")), "invalid_event");
  throwsCode(
    () => validateTestEvent(verdictEvent("x".repeat(16_385))),
    "invalid_event",
  );
  throwsCode(
    () => validateTestEvent(verdictEvent("bounded", Array(65).fill(frameEvidence()))),
    "invalid_event",
  );
});

test("rejects non-canonical nested DTOs, unsafe JSON and cyclic arguments", () => {
  const missingDetails = errorEvent(2) as unknown as Record<string, unknown>;
  const payload = (missingDetails.payload as Record<string, unknown>);
  const error = payload.error as Record<string, unknown>;
  delete error.details;
  throwsCode(() => validateTestEvent(missingDetails), "invalid_event");

  throwsCode(
    () => validateTestEvent({ ...started(), atMs: Number.MAX_SAFE_INTEGER + 1 }),
    "invalid_event",
  );
  throwsCode(() => validateTestEvent({ ...started(), atMs: -0 }), "invalid_event");
  const cyclic: Record<string, unknown> = {};
  cyclic.self = cyclic;
  throwsCode(() => validateTestEvent(actionStarted(2, CALL_A, { arguments: cyclic })), "invalid_event");

  const prototypeKey = JSON.parse('{"__proto__":{"polluted":true}}') as Record<string, unknown>;
  const protectedSnapshot = validateTestEvent(
    actionStarted(2, CALL_A, { arguments: prototypeKey }),
  );
  assert.equal(protectedSnapshot.payload.type, "actionStarted");
  if (protectedSnapshot.payload.type === "actionStarted") {
    const arguments_ = protectedSnapshot.payload.call.arguments as Record<string, unknown>;
    assert.ok(Object.hasOwn(arguments_, "__proto__"));
    assert.equal(Object.getPrototypeOf(arguments_), Object.prototype);
  }

  const observation = event(2, {
    observation: {
      capturedAtMs: 10,
      deviceId: "mock-1",
      id: "99999999-9999-4999-8999-999999999999",
      metadata: {},
      screenshot: null,
      viewport: { height: 720, scaleFactor: 1, width: 1280 },
    },
    type: "observationCaptured",
  });
  assert.equal(validateTestEvent(observation).payload.type, "observationCaptured");
  const missingScreenshot = structuredClone(observation) as unknown as Record<string, unknown>;
  const observationPayload = missingScreenshot.payload as Record<string, unknown>;
  delete (observationPayload.observation as Record<string, unknown>).screenshot;
  throwsCode(() => validateTestEvent(missingScreenshot), "invalid_event");

  const uiObservation = structuredClone(observation) as unknown as Record<string, unknown>;
  const uiPayload = uiObservation.payload as Record<string, unknown>;
  const uiValue = uiPayload.observation as Record<string, unknown>;
  const digest = "b".repeat(64);
  uiValue.uiSnapshot = {
    formatVersion: 1,
    context: {
      contextKind: "native",
      contextId: "NATIVE_APP",
      documentEpoch: "wda-session-1",
    },
    nodeCount: 1,
    byteLength: 256,
    evidence: {
      id: `sha256:${digest}`,
      mediaType: "application/vnd.devicerail.ui-tree+json;version=1",
      sha256: digest,
      uri: `devicerail://assets/sha256/${digest}`,
    },
  };
  assert.equal(
    validateTestEvent(uiObservation, "event", { major: 1, minor: 5 }).payload.type,
    "observationCaptured",
  );
  throwsCode(
    () => validateTestEvent(uiObservation, "event", { major: 1, minor: 4 }),
    "invalid_event",
  );

  const malformedUi = structuredClone(uiObservation) as Record<string, unknown>;
  const malformedPayload = malformedUi.payload as Record<string, unknown>;
  const malformedObservation = malformedPayload.observation as Record<string, unknown>;
  const malformedSnapshot = malformedObservation.uiSnapshot as Record<string, unknown>;
  malformedSnapshot.context = {
    contextKind: "native",
    contextId: "NATIVE_APP",
    documentEpoch: "",
  };
  throwsCode(() => validateTestEvent(malformedUi), "invalid_event");

  const emptyTree = structuredClone(uiObservation) as Record<string, unknown>;
  const emptyTreePayload = emptyTree.payload as Record<string, unknown>;
  const emptyTreeObservation = emptyTreePayload.observation as Record<string, unknown>;
  const emptyTreeSnapshot = emptyTreeObservation.uiSnapshot as Record<string, unknown>;
  emptyTreeSnapshot.nodeCount = 0;
  throwsCode(() => validateTestEvent(emptyTree), "invalid_event");
});

test("strict checkpoint replay rejects duplicate rows instead of normalizing corruption", () => {
  throwsCode(() => EventLog.replay(SESSION_ID, [started(), started()]), "sequence_conflict");
});
