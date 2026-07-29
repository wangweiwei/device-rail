import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import test from "node:test";

import type { TestEvent } from "@devicerail/protocol";

import {
  LIVE_TIMELINE_MAX_PAGE_SIZE,
  LiveTimeline,
  LiveTimelineError,
  type PreparedTimelineEvent,
  type TimelineCommit,
  type TimelineEntry,
} from "../src/index.js";

const SESSION_ID = "20000000-0000-4000-8000-000000000001";

function uuid(sequence: number, prefix = "21"): string {
  return `${prefix}000000-0000-4000-8000-${String(sequence).padStart(12, "0")}`;
}

function event(
  sequence: number,
  payload: TestEvent["payload"],
  overrides: Partial<TestEvent> = {},
): TestEvent {
  return {
    atMs: 1_000 + sequence,
    eventId: uuid(sequence),
    payload,
    sequence,
    sessionId: SESSION_ID,
    ...overrides,
  };
}

function actionStarted(sequence: number, callSequence: number, name = "tap"): TestEvent {
  return event(sequence, {
    call: {
      arguments: { x: callSequence, y: callSequence + 1 },
      id: uuid(callSequence, "22"),
      name,
    },
    type: "actionStarted",
  });
}

function actionCompleted(sequence: number, callSequence: number): TestEvent {
  const callId = uuid(callSequence, "22");
  return event(sequence, {
    callId,
    outcome: {
      outcome: "succeeded",
      result: {
        callId,
        evidence: [],
        finishedAtMs: 2_000 + sequence,
        output: { accepted: true },
        startedAtMs: 1_000 + sequence,
      },
    },
    type: "actionCompleted",
  });
}

function append(timeline: LiveTimeline, value: TestEvent): void {
  const prepared = timeline.prepare(value);
  const commit = timeline.commit(prepared);
  timeline.confirm(commit);
}

function hasCode(code: string): (error: unknown) => boolean {
  return (error) => error instanceof LiveTimelineError && error.code === code;
}

test("shared offline/live presentation fixture has category, title, status, and filter parity", async () => {
  const fixtureRoot = new URL(
    "../../../../visualizer/fixtures/presentation-semantics/",
    import.meta.url,
  );
  const [manifestText, expectationsText] = await Promise.all([
    readFile(new URL("manifest.json", fixtureRoot), "utf8"),
    readFile(new URL("../presentation-semantics.expectations.json", fixtureRoot), "utf8"),
  ]);
  const manifest = JSON.parse(manifestText) as Record<string, unknown>;
  const expectations = JSON.parse(expectationsText) as Record<string, unknown>;
  const events = manifest.events as TestEvent[];
  const expectedEvents = expectations.events as Array<{
    readonly category: string;
    readonly sequence: number;
    readonly status?: string;
    readonly title: string;
  }>;
  const timeline = new LiveTimeline(events[0]?.sessionId ?? "missing");
  events.forEach((value) => append(timeline, value));

  const actual = timeline.page({ pageSize: 50 }).items.map((item) => ({
    category: item.category,
    sequence: item.sequence,
    ...(item.status ? { status: item.status } : {}),
    title: item.title,
  }));
  assert.deepEqual(actual, expectedEvents);
  const filters = expectations.filters as Record<string, number[]>;
  for (const filter of ["all", "observations", "actions", "errors", "verdicts"] as const) {
    assert.deepEqual(
      timeline.page({ filter, pageSize: 50 }).items.map((item) => item.sequence),
      filters[filter],
    );
  }

  const protectedStart = timeline.page().items[2];
  assert.equal(protectedStart?.presentation.type, "actionStarted");
  if (protectedStart?.presentation.type === "actionStarted") {
    assert.deepEqual(protectedStart.presentation.arguments, { omitted: "protected" });
  }
  const protectedEnd = timeline.page().items[3];
  assert.equal(protectedEnd?.presentation.type, "actionCompleted");
  if (
    protectedEnd?.presentation.type === "actionCompleted" &&
    protectedEnd.presentation.completion.outcome === "succeeded"
  ) {
    assert.equal(protectedEnd.presentation.completion.before?.screenshotOmission, "protectedAction");
    assert.equal(protectedEnd.presentation.completion.after?.screenshotOmission, "protectedAction");
  }
  assert.equal(timeline.status, "sessionEnded");
});

test("prepare, commit, external confirm, and model confirm keep publication in the required order", () => {
  const timeline = new LiveTimeline(SESSION_ID);
  const order: string[] = [];
  const prepared = timeline.prepare(event(1, { type: "sessionStarted" }));
  order.push("prepare");
  const commit = timeline.commit(prepared);
  order.push("commit");
  assert.equal(timeline.revision, 0);
  assert.equal(timeline.page().items.length, 0);
  assert.deepEqual(timeline.state().pending, {
    fingerprint: prepared.fingerprint,
    sequence: 1,
  });
  order.push("daemon-confirm");
  const confirmed = timeline.confirm(commit);
  order.push("model-confirm");
  assert.deepEqual(order, ["prepare", "commit", "daemon-confirm", "model-confirm"]);
  assert.deepEqual(confirmed, { revision: 1, sequence: 1, status: "active" });
  assert.equal(timeline.page().items.length, 1);
  assert.equal(timeline.state().pending, undefined);
});

test("an exact pending replay is idempotent while conflicting content fails closed", () => {
  const timeline = new LiveTimeline(SESSION_ID);
  const original = event(1, { type: "sessionStarted" });
  const first = timeline.commit(timeline.prepare(original));
  const reordered = {
    sessionId: original.sessionId,
    sequence: original.sequence,
    payload: original.payload,
    eventId: original.eventId,
    atMs: original.atMs,
  } as TestEvent;
  const replay = timeline.commit(timeline.prepare(reordered));
  assert.equal(replay.kind, "pendingReplay");
  assert.equal(replay.fingerprint, first.fingerprint);
  assert.equal(timeline.revision, 0);

  assert.throws(
    () => timeline.commit(timeline.prepare({ ...original, atMs: original.atMs + 1 })),
    hasCode("event_conflict"),
  );
  assert.equal(timeline.state().eventCount, 0);
  timeline.confirm(replay);
  assert.equal(timeline.state().confirmedSequence, 1);
  assert.throws(() => timeline.confirm(first), hasCode("invalid_confirmation"));
});

test("a second sequence cannot commit before the first pending sequence is confirmed", () => {
  const timeline = new LiveTimeline(SESSION_ID);
  const first = timeline.commit(timeline.prepare(event(1, { type: "sessionStarted" })));
  assert.throws(
    () => timeline.commit(timeline.prepare(actionStarted(2, 1))),
    hasCode("pending_confirmation"),
  );
  assert.deepEqual(timeline.state().pending?.sequence, 1);
  timeline.confirm(first);
  append(timeline, actionStarted(2, 1));
  assert.equal(timeline.state().confirmedSequence, 2);
});

test("101 complete Actions remain ordered and pages never exceed fifty items", () => {
  const timeline = new LiveTimeline(SESSION_ID);
  append(timeline, event(1, { type: "sessionStarted" }));
  let sequence = 2;
  for (let call = 1; call <= 101; call += 1) {
    append(timeline, actionStarted(sequence, call));
    sequence += 1;
    append(timeline, actionCompleted(sequence, call));
    sequence += 1;
  }
  append(
    timeline,
    event(sequence, { outcome: "completed", reason: "load complete", type: "sessionEnded" }),
  );

  const first = timeline.page({ filter: "actions", page: 1, pageSize: 50 });
  const fifth = timeline.page({ filter: "actions", page: 5, pageSize: 50 });
  assert.equal(first.items.length, LIVE_TIMELINE_MAX_PAGE_SIZE);
  assert.equal(first.totalItems, 202);
  assert.equal(first.totalPages, 5);
  assert.equal(fifth.items.length, 2);
  assert.deepEqual(
    timeline.page({ filter: "actions", pageSize: 50 }).items.map((item) => item.sequence),
    Array.from({ length: 50 }, (_unused, index) => index + 2),
  );
  assert.equal(timeline.status, "sessionEnded");
  assert.equal(timeline.revision, sequence);
  assert.throws(() => timeline.page({ pageSize: 51 }), hasCode("invalid_page"));
});

test("capacity rejection preserves all confirmed data and does not reserve the current event", () => {
  const timeline = new LiveTimeline(SESSION_ID, { limits: { maxEvents: 2 } });
  append(timeline, event(1, { type: "sessionStarted" }));
  append(timeline, actionStarted(2, 1));
  const before = timeline.state();
  const pageBefore = timeline.page();
  assert.throws(
    () => timeline.commit(timeline.prepare(actionCompleted(3, 1))),
    hasCode("viewer_capacity_exceeded"),
  );
  const after = timeline.state();
  assert.equal(after.status, "viewerCapacityExceeded");
  assert.equal(after.revision, before.revision);
  assert.equal(after.eventCount, before.eventCount);
  assert.equal(after.totalBytes, before.totalBytes);
  assert.equal(after.pending, undefined);
  assert.deepEqual(timeline.page().items, pageBefore.items);
});

test("input, per-event, and aggregate byte ceilings fail closed without partial publication", () => {
  const input = new LiveTimeline(SESSION_ID, { limits: { maxInputEventBytes: 256 } });
  assert.throws(
    () => input.prepare(actionStarted(1, 1, "x".repeat(1_000))),
    hasCode("viewer_capacity_exceeded"),
  );
  assert.deepEqual(input.state(), {
    eventCount: 0,
    revision: 0,
    sessionId: SESSION_ID,
    status: "viewerCapacityExceeded",
    totalBytes: 0,
  });

  const oneEvent = new LiveTimeline(SESSION_ID, {
    limits: {
      maxEventBytes: 512,
      maxJsonBytes: 128,
      maxTextBytes: 128,
      maxTotalBytes: 1_024,
    },
  });
  append(oneEvent, event(1, { type: "sessionStarted" }));
  const oversizedEvent = actionStarted(2, 1, "y".repeat(1_000));
  oversizedEvent.deviceId = "device-" + "d".repeat(1_000);
  if (oversizedEvent.payload.type === "actionStarted") {
    oversizedEvent.payload.call.arguments = { value: "a".repeat(1_000) };
  }
  const oversized = oneEvent.prepare(oversizedEvent);
  assert.ok(oversized.byteSize > oneEvent.limits.maxEventBytes);
  assert.throws(() => oneEvent.commit(oversized), hasCode("viewer_capacity_exceeded"));
  assert.equal(oneEvent.state().eventCount, 1);
  assert.equal(oneEvent.state().confirmedSequence, 1);
  assert.equal(oneEvent.state().pending, undefined);

  const aggregate = new LiveTimeline(SESSION_ID, {
    limits: {
      maxEventBytes: 1_024,
      maxJsonBytes: 128,
      maxTextBytes: 128,
      maxTotalBytes: 1_024,
    },
  });
  append(aggregate, event(1, { type: "sessionStarted" }));
  let rejectedSequence: number | undefined;
  for (let sequence = 2; sequence <= 10; sequence += 1) {
    const value = event(sequence, {
      error: {
        code: "bounded_error",
        message: "aggregate capacity probe",
        retryable: false,
      },
      type: "error",
    });
    const prepared = aggregate.prepare(value);
    try {
      aggregate.confirm(aggregate.commit(prepared));
    } catch (error) {
      assert.ok(hasCode("viewer_capacity_exceeded")(error));
      rejectedSequence = sequence;
      break;
    }
  }
  assert.notEqual(rejectedSequence, undefined);
  assert.equal(aggregate.status, "viewerCapacityExceeded");
  assert.equal(aggregate.state().pending, undefined);
  assert.equal(aggregate.state().confirmedSequence, (rejectedSequence as number) - 1);
  assert.equal(aggregate.page().items.length, (rejectedSequence as number) - 1);

  const boundedJson = new LiveTimeline(SESSION_ID, {
    limits: { maxJsonBytes: 64, maxTextBytes: 16 },
  });
  append(boundedJson, event(1, { type: "sessionStarted" }));
  const jsonEvent = actionStarted(2, 1);
  if (jsonEvent.payload.type === "actionStarted") {
    jsonEvent.payload.call.arguments = { text: "z".repeat(10_000) };
  }
  append(boundedJson, jsonEvent);
  const jsonEntry = boundedJson.page().items[1];
  if (jsonEntry?.presentation.type === "actionStarted" && "json" in jsonEntry.presentation.arguments) {
    assert.equal(jsonEntry.presentation.arguments.truncated, true);
    assert.ok(Buffer.byteLength(jsonEntry.presentation.arguments.json) <= 64);
  } else {
    assert.fail("expected bounded Action arguments");
  }
});

test("reference-only Evidence drops every URI across Observation, Action, and Verdict", () => {
  const timeline = new LiveTimeline(SESSION_ID);
  const secretUri = "file:///private/tmp/do-not-leak.png";
  append(timeline, event(1, { type: "sessionStarted" }));
  append(
    timeline,
    event(2, {
      observation: {
        capturedAtMs: 2,
        deviceId: "device",
        id: uuid(1, "23"),
        screenshot: {
          id: "screen",
          mediaType: "image/png",
          sha256: "a".repeat(64),
          uri: secretUri,
        },
        viewport: { height: 720, scaleFactor: 1, width: 1280 },
      },
      type: "observationCaptured",
    }),
  );
  append(
    timeline,
    event(3, {
      type: "verdictRecorded",
      verdict: {
        evidence: [{ id: "verdict", mediaType: "text/plain", uri: secretUri }],
        status: "fail",
        summary: "expected failure",
      },
    }),
  );
  const serialized = JSON.stringify(timeline.page());
  assert.equal(serialized.includes(secretUri), false);
  assert.equal(serialized.includes("\"uri\""), false);
  assert.equal(serialized.includes("referenceOnly"), true);
  const observation = timeline.page({ filter: "observations" }).items[0];
  if (observation?.presentation.type === "observationCaptured") {
    assert.deepEqual(Object.keys(observation.presentation.observation.screenshot ?? {}).sort(), [
      "availability",
      "id",
      "mediaType",
      "sha256",
    ]);
  } else {
    assert.fail("expected Observation presentation");
  }

  const many = new LiveTimeline(SESSION_ID, { limits: { maxEvidencePerEvent: 3 } });
  append(many, event(1, { type: "sessionStarted" }));
  append(
    many,
    event(2, {
      type: "verdictRecorded",
      verdict: {
        evidence: Array.from({ length: 8 }, (_unused, index) => ({
          id: `asset-${index}`,
          mediaType: "application/octet-stream",
          uri: `${secretUri}#${index}`,
        })),
        status: "unknown",
        summary: "bounded Evidence list",
      },
    }),
  );
  const verdict = many.page().items[1];
  if (verdict?.presentation.type === "verdictRecorded") {
    assert.equal(verdict.presentation.evidence.length, 3);
    assert.equal(verdict.presentation.evidenceOmitted, 5);
  } else {
    assert.fail("expected Verdict presentation");
  }
});

test("malicious text is visibly escaped, truncated, and never retained through the raw event", () => {
  const timeline = new LiveTimeline(SESSION_ID, {
    limits: { maxJsonBytes: 512, maxTextBytes: 64 },
  });
  const malicious = "<script>alert(1)</script>\u0000\u0085\u202e" + "x".repeat(1_000);
  append(timeline, event(1, { type: "sessionStarted" }));
  const raw = actionStarted(2, 1, malicious);
  const prepared = timeline.prepare(raw);
  (raw.payload as Extract<TestEvent["payload"], { type: "actionStarted" }>).call.name =
    "mutated after prepare";
  const commit = timeline.commit(prepared);
  timeline.confirm(commit);
  const entry = timeline.page().items[1];
  assert.equal(entry?.presentation.type, "actionStarted");
  if (entry?.presentation.type === "actionStarted") {
    const name = entry.presentation.name;
    assert.equal(name.text.includes("<script>"), false);
    assert.equal(name.text.includes("\\u{003c}"), true);
    assert.equal(name.text.includes("\u0000"), false);
    assert.equal(name.text.includes("\u0085"), false);
    assert.equal(name.text.includes("\u202e"), false);
    assert.equal(name.text.includes("mutated after prepare"), false);
    assert.equal(name.truncated, true);
    assert.ok(Buffer.byteLength(name.text) <= 64);
  }
  assert.equal(JSON.stringify(timeline.page()).includes("<script>"), false);
});

test("prototype keys are rendered as data while exotic prototypes, cycles, and excess depth fail", () => {
  const timeline = new LiveTimeline(SESSION_ID, { limits: { maxJsonDepth: 8 } });
  const argumentsValue = JSON.parse(
    '{"__proto__":{"polluted":true},"[unsafe-key:__proto__]":{"also":true},"constructor":{"prototype":{"x":1}}}',
  ) as Record<string, unknown>;
  append(timeline, event(1, { type: "sessionStarted" }));
  const safe = actionStarted(2, 1);
  if (safe.payload.type === "actionStarted") safe.payload.call.arguments = argumentsValue;
  append(timeline, safe);
  const first = timeline.page().items[1];
  if (first?.presentation.type === "actionStarted" && "json" in first.presentation.arguments) {
    assert.match(first.presentation.arguments.json, /unsafe-key/);
    assert.match(first.presentation.arguments.json, /collision-safe object entries/);
  } else {
    assert.fail("expected bounded arguments JSON");
  }
  assert.equal((Object.prototype as unknown as Record<string, unknown>).polluted, undefined);

  const exotic = actionStarted(3, 2) as TestEvent & Record<string, unknown>;
  Object.setPrototypeOf(exotic, { hostile: true });
  assert.throws(() => timeline.prepare(exotic), hasCode("invalid_event"));

  let getterCalls = 0;
  const accessor = actionStarted(3, 2) as TestEvent & Record<string, unknown>;
  Object.defineProperty(accessor, "hidden", {
    enumerable: true,
    get() {
      getterCalls += 1;
      return "must not execute";
    },
  });
  assert.throws(() => timeline.prepare(accessor), hasCode("invalid_event"));
  assert.equal(getterCalls, 0);

  const cycle: Record<string, unknown> = {};
  cycle.self = cycle;
  const cyclic = actionStarted(3, 2);
  if (cyclic.payload.type === "actionStarted") cyclic.payload.call.arguments = cycle;
  assert.throws(() => timeline.prepare(cyclic), hasCode("invalid_event"));

  let deep: unknown = "end";
  for (let index = 0; index < 20; index += 1) deep = { child: deep };
  const tooDeep = actionStarted(3, 2);
  if (tooDeep.payload.type === "actionStarted") tooDeep.payload.call.arguments = deep;
  assert.throws(() => timeline.prepare(tooDeep), hasCode("invalid_event"));
  assert.equal(timeline.state().eventCount, 2);
  assert.equal(timeline.state().pending, undefined);
});

test("prepared and commit tokens are bound to exact object identity", () => {
  const timeline = new LiveTimeline(SESSION_ID);
  const prepared = timeline.prepare(event(1, { type: "sessionStarted" }));
  const forgedPrepared = {
    ...prepared,
    byteSize: 0,
  } as PreparedTimelineEvent;
  for (const symbol of Object.getOwnPropertySymbols(prepared)) {
    Object.defineProperty(forgedPrepared, symbol, { value: Reflect.get(prepared, symbol) });
  }
  Object.freeze(forgedPrepared);
  assert.throws(() => timeline.commit(forgedPrepared), hasCode("stale_prepared_event"));

  const commit = timeline.commit(prepared);
  const forgedCommit = { ...commit } as TimelineCommit;
  for (const symbol of Object.getOwnPropertySymbols(commit)) {
    Object.defineProperty(forgedCommit, symbol, { value: Reflect.get(commit, symbol) });
  }
  Object.freeze(forgedCommit);
  assert.throws(() => timeline.confirm(forgedCommit), hasCode("invalid_confirmation"));
  assert.equal(timeline.state().pending?.sequence, 1);
  timeline.confirm(commit);
  assert.equal(timeline.state().confirmedSequence, 1);
});

test("Session and Action lifecycle violations fail before acknowledgement", () => {
  const firstMustStart = new LiveTimeline(SESSION_ID);
  const wrongFirst = firstMustStart.prepare(
    event(1, {
      error: { code: "wrong_first", message: "not a start", retryable: false },
      type: "error",
    }),
  );
  assert.throws(() => firstMustStart.commit(wrongFirst), hasCode("invalid_event"));

  const timeline = new LiveTimeline(SESSION_ID);
  const started = event(1, { type: "sessionStarted" });
  append(timeline, started);
  const duplicateEventId = timeline.prepare(
    event(
      2,
      { error: { code: "duplicate", message: "duplicate id", retryable: false }, type: "error" },
      { eventId: started.eventId },
    ),
  );
  assert.throws(() => timeline.commit(duplicateEventId), hasCode("invalid_event"));

  const missingStart = timeline.prepare(actionCompleted(2, 1));
  assert.throws(() => timeline.commit(missingStart), hasCode("invalid_event"));

  append(timeline, actionStarted(2, 1));
  const duplicateStart = timeline.prepare(actionStarted(3, 1));
  assert.throws(() => timeline.commit(duplicateStart), hasCode("invalid_event"));
  const prematureEnd = timeline.prepare(
    event(3, { outcome: "completed", type: "sessionEnded" }),
  );
  assert.throws(() => timeline.commit(prematureEnd), hasCode("invalid_event"));

  const mismatchedResult = actionCompleted(3, 1);
  if (
    mismatchedResult.payload.type === "actionCompleted" &&
    mismatchedResult.payload.outcome.outcome === "succeeded"
  ) {
    mismatchedResult.payload.outcome.result.callId = uuid(99, "22");
  }
  assert.throws(() => timeline.prepare(mismatchedResult), hasCode("invalid_event"));

  append(timeline, actionCompleted(3, 1));
  append(timeline, event(4, { outcome: "completed", type: "sessionEnded" }));
  assert.equal(timeline.status, "sessionEnded");
  assert.throws(
    () => timeline.prepare(event(5, { type: "sessionStarted" })),
    hasCode("timeline_closed"),
  );
});

test("media Evidence is reference-only and stream lifecycle is ordered", () => {
  const timeline = new LiveTimeline(SESSION_ID);
  const streamId = uuid(1, "33");
  const digest = "a".repeat(64);
  const uri = `devicerail://assets/sha256/${digest}`;
  append(timeline, event(1, { type: "sessionStarted" }));
  append(timeline, event(2, {
    type: "mediaStreamStarted",
    stream: {
      id: streamId,
      kind: "video",
      mediaType: "video/webm",
      viewport: { width: 1280, height: 720, scaleFactor: 1 },
    },
  }));
  const outOfOrder = timeline.prepare(event(3, {
    type: "mediaFrameCaptured",
    frame: {
      streamId,
      frameIndex: 2,
      evidence: { id: `sha256:${digest}`, mediaType: "video/webm", sha256: digest, uri },
    },
  }));
  assert.throws(() => timeline.commit(outOfOrder), hasCode("invalid_event"));
  append(timeline, event(3, {
    type: "mediaFrameCaptured",
    frame: {
      streamId,
      frameIndex: 1,
      keyFrame: true,
      durationMs: 100,
      evidence: { id: `sha256:${digest}`, mediaType: "video/webm", sha256: digest, uri },
    },
  }));
  append(timeline, event(4, { type: "mediaStreamEnded", streamId, frameCount: 1 }));
  append(timeline, event(5, { type: "sessionEnded", outcome: "completed", reason: null }));

  const all = timeline.page({ filter: "all" }).items;
  assert.deepEqual(
    all.map((entry) => entry.presentation.type),
    [
      "sessionStarted",
      "mediaStreamStarted",
      "mediaFrameCaptured",
      "mediaStreamEnded",
      "sessionEnded",
    ],
  );
  const observations = timeline.page({ filter: "observations" }).items;
  assert.deepEqual(
    observations.map((entry) => entry.presentation.type),
    ["mediaFrameCaptured"],
  );
  const [frame] = observations;
  assert.ok(frame);
  assert.equal(JSON.stringify(frame).includes(uri), false);
  assert.equal(frame.category, "media");
});

test("pages and state are immutable snapshots and stop/fail are bounded terminal states", () => {
  const stopped = new LiveTimeline(SESSION_ID);
  append(stopped, event(1, { type: "sessionStarted" }));
  const page = stopped.page();
  assert.ok(Object.isFrozen(page));
  assert.ok(Object.isFrozen(page.items));
  assert.ok(Object.isFrozen(page.items[0]));
  assert.throws(() =>
    (page.items as unknown as TimelineEntry[]).push(page.items[0] as TimelineEntry),
  );
  assert.equal(stopped.stop().status, "stopped");
  assert.throws(() => stopped.commit(stopped.prepare(actionStarted(2, 1))), hasCode("timeline_closed"));

  const failed = new LiveTimeline(SESSION_ID);
  const pending = failed.commit(failed.prepare(event(1, { type: "sessionStarted" })));
  assert.equal(pending.sequence, 1);
  const failure = failed.fail();
  assert.equal(failure.status, "failed");
  assert.equal(failure.pending, undefined);
  assert.equal(failure.revision, 0);
});
