import assert from "node:assert/strict";
import { mkdir, mkdtemp, rm, stat } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import test from "node:test";

import { RpcRemoteError } from "@devicerail/client";
import type {
  SessionExportResult,
  SystemDescribeResult,
  TestEvent,
} from "@devicerail/protocol";

import {
  BUNDLE_SOURCE_MAX_BYTES,
  commitRecorderCheckpoint,
  ExecutionRecorder,
  loadRecorderCheckpoint,
  readBundleSource,
  RECORDER_CHECKPOINT_HEADROOM_BYTES,
  RECORDER_CHECKPOINT_MAX_BYTES,
  RecorderError,
  type RecordingCheckpoint,
  type RecorderEventSource,
} from "../src/index.js";
import { requireBundleExecutable } from "./daemon-harness.js";

const sessionId = "11111111-1111-4111-8111-111111111111";

const started = {
  atMs: 100,
  eventId: "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaa1",
  payload: { type: "sessionStarted" },
  sequence: 1,
  sessionId,
} satisfies TestEvent;

const ended = {
  atMs: 200,
  eventId: "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaa2",
  payload: { outcome: "completed", reason: null, type: "sessionEnded" },
  sequence: 2,
  sessionId,
} satisfies TestEvent;

const description = {
  activeSessionId: sessionId,
  client: { name: "recorder-test", version: "0.1.0" },
  connection: {
    connectionId: "22222222-2222-4222-8222-222222222222",
    features: {
      enabled: ["events.snapshot.v1", "media.stream.v1", "session.export.page.v1"],
    },
    protocol: { selected: { major: 1, minor: 4 } },
    server: { name: "devicerail-daemon", version: "0.1.0" },
    transport: { framing: "ndjson", kind: "stdio" },
  },
  deviceId: "mock-1",
} satisfies SystemDescribeResult;

function endedSessionFor(events: readonly TestEvent[]): SessionExportResult["session"] {
  const first = events[0];
  const last = events.at(-1);
  assert.ok(first);
  assert.ok(last);
  return {
    endedAtMs: last.atMs,
    eventCount: events.length,
    id: sessionId,
    lastSequence: last.sequence,
    startedAtMs: first.atMs,
    state: "ended",
  };
}

function exportFor(events: readonly TestEvent[]): SessionExportResult {
  return {
    events: structuredClone(events) as TestEvent[],
    session: endedSessionFor(events),
  };
}

function errorSession(middleCount: number, message = "paged event"): TestEvent[] {
  const middle = Array.from({ length: middleCount }, (_, index): TestEvent => {
    const sequence = index + 2;
    return {
      atMs: 100 + sequence,
      eventId: `eeeeeeee-eeee-4eee-8eee-${sequence.toString().padStart(12, "0")}`,
      payload: {
        error: {
          code: "paged_event",
          details: null,
          message,
          retryable: false,
        },
        type: "error",
      },
      sequence,
      sessionId,
    };
  });
  const sequence = middleCount + 2;
  return [
    started,
    ...middle,
    {
      atMs: 100 + sequence,
      eventId: `ffffffff-ffff-4fff-8fff-${sequence.toString().padStart(12, "0")}`,
      payload: { outcome: "completed", reason: null, type: "sessionEnded" },
      sequence,
      sessionId,
    },
  ];
}

class FakeSource implements RecorderEventSource {
  events: TestEvent[];
  exportOverride: unknown | undefined;
  exportPageHook: (() => Promise<void>) | undefined;
  exportPageTransform:
    | ((page: unknown, callIndex: number) => unknown)
    | undefined;
  exportMaxSuccessfulLimit: number | undefined;
  listHook: (() => Promise<void>) | undefined;
  maxSuccessfulLimit: number | undefined;
  pageFeatureEnabled = true;
  legacyExportCalls = 0;
  readonly exportLimits: number[] = [];
  readonly exportRequests: Array<number | null> = [];
  readonly limits: number[] = [];
  readonly requests: Array<number | null> = [];

  constructor(events: readonly TestEvent[]) {
    this.events = structuredClone(events) as TestEvent[];
  }

  async describe(): Promise<SystemDescribeResult> {
    const result = structuredClone(description);
    if (!this.pageFeatureEnabled) {
      result.connection.features.enabled = result.connection.features.enabled.filter(
        (feature) => feature !== "session.export.page.v1",
      );
    }
    return result;
  }

  async listEvents(
    _sessionId: string,
    afterSequence: number | null,
    limit: number,
  ): Promise<readonly unknown[]> {
    this.requests.push(afterSequence);
    this.limits.push(limit);
    await this.listHook?.();
    if (this.maxSuccessfulLimit !== undefined && limit > this.maxSuccessfulLimit) {
      throw new RpcRemoteError("fake-events-list", {
        code: -32_012,
        data: {
          code: "response_frame_too_large",
          details: { limitBytes: 1024 * 1024 },
          message: "response frame is too large",
          retryable: false,
        },
        message: "response frame is too large",
      });
    }
    return structuredClone(
      this.events
        .filter((event) => afterSequence === null || event.sequence > afterSequence)
        .slice(0, limit),
    );
  }

  async exportSession(): Promise<unknown> {
    this.legacyExportCalls += 1;
    return this.exportOverride ?? exportFor(this.events);
  }

  async exportSessionPage(
    _sessionId: string,
    afterSequence: number | null,
    limit: number,
  ): Promise<unknown> {
    const callIndex = this.exportRequests.length;
    this.exportRequests.push(afterSequence);
    this.exportLimits.push(limit);
    await this.exportPageHook?.();
    if (this.exportMaxSuccessfulLimit !== undefined && limit > this.exportMaxSuccessfulLimit) {
      throw new RpcRemoteError("fake-session-export", {
        code: -32_012,
        data: {
          code: "response_frame_too_large",
          details: { limitBytes: 1024 * 1024 },
          message: "response frame is too large",
          retryable: false,
        },
        message: "response frame is too large",
      });
    }
    let page: unknown;
    if (this.exportOverride === undefined) {
      const remaining = this.events.filter(
        (event) => afterSequence === null || event.sequence > afterSequence,
      );
      const events = remaining.slice(0, limit);
      page = {
        events: structuredClone(events),
        session: structuredClone(endedSessionFor(this.events)),
        ...(remaining.length > events.length
          ? { nextAfterSequence: events.at(-1)!.sequence }
          : {}),
      };
      return structuredClone(this.exportPageTransform?.(page, callIndex) ?? page);
    }
    const full = structuredClone(this.exportOverride);
    page = full;
    if (
      full !== null
      && typeof full === "object"
      && "events" in full
      && Array.isArray(full.events)
    ) {
      const remaining = full.events.filter(
        (event: TestEvent) =>
          afterSequence === null || event.sequence > afterSequence,
      );
      const events = remaining.slice(0, limit);
      page = {
        ...full,
        events,
        ...(remaining.length > events.length
          ? { nextAfterSequence: (events.at(-1) as TestEvent).sequence }
          : {}),
      };
    }
    return structuredClone(this.exportPageTransform?.(page, callIndex) ?? page);
  }
}

async function temporaryCheckpoint(): Promise<{ directory: string; path: string }> {
  const directory = await mkdtemp(join(tmpdir(), "devicerail-recorder-unit-"));
  return { directory, path: join(directory, "checkpoint.json") };
}

function terminalRecording(events: readonly TestEvent[]): RecordingCheckpoint {
  return {
    eventProtocolVersion: { major: 1, minor: 4 },
    events,
    format: "devicerail.execution-recorder-checkpoint",
    phase: "recording",
    revision: 1,
    sessionId,
    version: 1,
  };
}

test("Recorder resumes at the last durable sequence and seals an exact export", async () => {
  const temporary = await temporaryCheckpoint();
  try {
    const source = new FakeSource([started]);
    const first = await ExecutionRecorder.open({
      checkpointPath: temporary.path,
      eventSource: source,
      sessionId,
    });
    const captured = await first.captureOnce();
    assert.deepEqual(captured, {
      accepted: 1,
      duplicates: 0,
      lastSequence: 1,
      phase: "recording",
    });
    assert.deepEqual(source.requests, [null]);

    source.events.push(structuredClone(ended));
    const resumed = await ExecutionRecorder.open({
      checkpointPath: temporary.path,
      eventSource: source,
      sessionId,
    });
    const terminal = await resumed.captureOnce();
    assert.equal(terminal.accepted, 1);
    assert.equal(terminal.phase, "sealed");
    assert.deepEqual(source.requests, [null, 1]);
    assert.deepEqual(resumed.bundleSource(), {
      eventProtocolVersion: { major: 1, minor: 4 },
      sessionExport: exportFor([started, ended]),
    });

    const checkpoint = await loadRecorderCheckpoint(temporary.path);
    assert.equal(checkpoint?.phase, "sealed");
    assert.equal(checkpoint?.revision, 4);
    assert.deepEqual(checkpoint?.events, [started, ended]);
  } finally {
    await rm(temporary.directory, { force: true, recursive: true });
  }
});

test("Recorder halves an oversized page and reuses the successful limit", async () => {
  const temporary = await temporaryCheckpoint();
  const middle = Array.from({ length: 1_000 }, (_, index): TestEvent => {
    const sequence = index + 2;
    return {
      atMs: 100 + sequence,
      eventId: `eeeeeeee-eeee-4eee-8eee-${sequence.toString().padStart(12, "0")}`,
      payload: {
        error: {
          code: "paged_event",
          details: null,
          message: "paged event",
          retryable: false,
        },
        type: "error",
      },
      sequence,
      sessionId,
    };
  });
  const terminal: TestEvent = {
    atMs: 1_102,
    eventId: "eeeeeeee-eeee-4eee-8eee-000000001002",
    payload: { outcome: "completed", reason: null, type: "sessionEnded" },
    sequence: 1_002,
    sessionId,
  };
  const events = [started, ...middle, terminal];
  const source = new FakeSource(events);
  source.maxSuccessfulLimit = 500;
  try {
    const recorder = await ExecutionRecorder.open({
      checkpointPath: temporary.path,
      eventSource: source,
      sessionId,
    });
    const capture = await recorder.captureOnce();
    assert.deepEqual(capture, {
      accepted: 1_002,
      duplicates: 0,
      lastSequence: 1_002,
      phase: "sealed",
    });
    assert.deepEqual(source.requests, [null, null, 500, 1_000]);
    assert.deepEqual(source.limits, [1_000, 500, 500, 500]);
    assert.equal(recorder.bundleSource().sessionExport.events.length, 1_002);
    const checkpoint = await loadRecorderCheckpoint(temporary.path);
    assert.equal(
      checkpoint?.revision,
      5,
      "three accepted pages and sealing remain durable CAS steps",
    );
    assert.deepEqual(checkpoint?.events, events);
  } finally {
    await rm(temporary.directory, { force: true, recursive: true });
  }
});

test("Recorder reports one event that cannot fit as explicitly unrecoverable", async () => {
  const temporary = await temporaryCheckpoint();
  const source = new FakeSource([started]);
  source.maxSuccessfulLimit = 0;
  try {
    const recorder = await ExecutionRecorder.open({
      checkpointPath: temporary.path,
      eventSource: source,
      sessionId,
    });
    await assert.rejects(
      recorder.captureOnce(),
      (error: unknown) => {
        assert.ok(error instanceof RecorderError);
        assert.equal(error.code, "event_too_large");
        assert.deepEqual(error.details, {
          afterSequence: null,
          pageLimit: 1,
          upstreamCode: "response_frame_too_large",
        });
        return true;
      },
    );
    assert.deepEqual(source.limits, [1_000, 500, 250, 125, 62, 31, 15, 7, 3, 1]);
    const checkpoint = await loadRecorderCheckpoint(temporary.path);
    assert.equal(checkpoint?.revision, 1);
    assert.deepEqual(checkpoint?.events, []);
  } finally {
    await rm(temporary.directory, { force: true, recursive: true });
  }
});

test("Recorder adaptively pages the authoritative export and seals with one final CAS", async () => {
  const temporary = await temporaryCheckpoint();
  const events = errorSession(600);
  const source = new FakeSource(events);
  source.exportMaxSuccessfulLimit = 250;
  try {
    const recorder = await ExecutionRecorder.open({
      checkpointPath: temporary.path,
      eventSource: source,
      sessionId,
    });
    const capture = await recorder.captureOnce();
    assert.equal(capture.phase, "sealed");
    assert.deepEqual(source.exportLimits, [1_000, 500, 250, 250, 250]);
    assert.deepEqual(source.exportRequests, [null, null, null, 250, 500]);
    assert.equal(source.legacyExportCalls, 0);
    const checkpoint = await loadRecorderCheckpoint(temporary.path);
    assert.equal(
      checkpoint?.revision,
      3,
      "export pages are read-only and only the final seal advances the checkpoint",
    );
    assert.deepEqual(checkpoint?.events, events);
  } finally {
    await rm(temporary.directory, { force: true, recursive: true });
  }
});

test("Recorder accepts an exact full final export page only without a continuation", async () => {
  const temporary = await temporaryCheckpoint();
  const events = errorSession(998);
  const source = new FakeSource(events);
  try {
    const recorder = await ExecutionRecorder.open({
      checkpointPath: temporary.path,
      eventSource: source,
      sessionId,
    });
    assert.equal((await recorder.captureOnce()).phase, "sealed");
    assert.deepEqual(source.exportRequests, [null]);
    assert.deepEqual(source.exportLimits, [1_000]);
  } finally {
    await rm(temporary.directory, { force: true, recursive: true });
  }
});

test("Recorder requires an exact authoritative export continuation cursor", async () => {
  for (const mode of ["missing", "wrong", "final-extra"] as const) {
    const temporary = await temporaryCheckpoint();
    const source = new FakeSource(
      mode === "final-extra" ? [started, ended] : errorSession(1_000),
    );
    source.exportPageTransform = (page, callIndex) => {
      if (callIndex !== 0 || page === null || typeof page !== "object") {
        return page;
      }
      const record = page as Record<string, unknown>;
      if (mode === "missing") {
        const { nextAfterSequence: _ignored, ...withoutCursor } = record;
        return withoutCursor;
      }
      return {
        ...record,
        nextAfterSequence: mode === "wrong" ? 999 : 2,
      };
    };
    try {
      const recorder = await ExecutionRecorder.open({
        checkpointPath: temporary.path,
        eventSource: source,
        sessionId,
      });
      await assert.rejects(
        recorder.captureOnce(),
        (error: unknown) =>
          error instanceof RecorderError && error.code === "session_export_mismatch",
        mode,
      );
      const checkpoint = await loadRecorderCheckpoint(temporary.path);
      assert.equal(checkpoint?.phase, "recording", mode);
    } finally {
      await rm(temporary.directory, { force: true, recursive: true });
    }
  }
});

test(
  "Recorder seals an export larger than one RPC frame through bounded authoritative pages",
  { timeout: 30_000 },
  async () => {
    const temporary = await temporaryCheckpoint();
    const events = errorSession(1_000, "x".repeat(2_000));
    const fullExport = exportFor(events);
    const rpcBytes = Buffer.byteLength(
      JSON.stringify({ id: 7, jsonrpc: "2.0", result: fullExport }),
      "utf8",
    );
    const sourceBytes = Buffer.byteLength(
      JSON.stringify({
        eventProtocolVersion: { major: 1, minor: 4 },
        sessionExport: fullExport,
      }),
      "utf8",
    );
    assert.ok(rpcBytes > 2 * 1024 * 1024, "legacy session.export must clearly exceed one RPC frame");
    assert.ok(
      sourceBytes <= 8 * 1024 * 1024,
      "the v1 local Source must remain inside its independent hard limit",
    );
    const firstPageBytes = Buffer.byteLength(
      JSON.stringify({
        id: 7,
        jsonrpc: "2.0",
        result: { events: fullExport.events.slice(0, 250), session: fullExport.session },
      }),
      "utf8",
    );
    assert.ok(firstPageBytes <= 1024 * 1024, "each authoritative page must fit one RPC frame");
    const source = new FakeSource(events);
    source.maxSuccessfulLimit = 250;
    source.exportMaxSuccessfulLimit = 250;
    try {
      const recorder = await ExecutionRecorder.open({
        checkpointPath: temporary.path,
        eventSource: source,
        sessionId,
      });
      const capture = await recorder.captureOnce();
      assert.equal(capture.phase, "sealed");
      assert.equal(capture.lastSequence, events.length);
      assert.deepEqual(source.exportRequests, [null, null, null, 250, 500, 750, 1_000]);
      assert.equal(source.legacyExportCalls, 0);
      assert.deepEqual(recorder.bundleSource().sessionExport, fullExport);
      const sourcePath = join(temporary.directory, "large-session-source.json");
      await recorder.publishSource(sourcePath);
      assert.deepEqual(await readBundleSource(sourcePath), recorder.bundleSource());
      const evidenceDirectory = join(temporary.directory, "evidence");
      const outputDirectory = join(temporary.directory, "large-session.bundle");
      await mkdir(evidenceDirectory, { mode: 0o700 });
      const receipt = await recorder.finalize({
        evidenceDirectory,
        executable: requireBundleExecutable(),
        outputDirectory,
        sourcePath,
      });
      assert.equal(receipt.eventCount, events.length);
      assert.equal(receipt.assetCount, 0);
    } finally {
      await rm(temporary.directory, { force: true, recursive: true });
    }
  },
);

test(
  "a near-8-MiB recording seals, publishes, finalizes, and completes within checkpoint headroom",
  { timeout: 60_000 },
  async () => {
    const temporary = await temporaryCheckpoint();
    const events = errorSession(1_000, "x".repeat(8_156));
    await commitRecorderCheckpoint(temporary.path, 0, terminalRecording(events));
    const source = new FakeSource(events);
    source.exportMaxSuccessfulLimit = 62;
    try {
      assert.equal(
        RECORDER_CHECKPOINT_MAX_BYTES,
        BUNDLE_SOURCE_MAX_BYTES + RECORDER_CHECKPOINT_HEADROOM_BYTES,
      );
      const recorder = await ExecutionRecorder.open({
        checkpointPath: temporary.path,
        eventSource: source,
        sessionId,
      });
      await recorder.seal();
      const sourceBytes = Buffer.byteLength(JSON.stringify(recorder.bundleSource()), "utf8") + 1;
      assert.ok(sourceBytes > BUNDLE_SOURCE_MAX_BYTES - RECORDER_CHECKPOINT_HEADROOM_BYTES);
      assert.ok(sourceBytes <= BUNDLE_SOURCE_MAX_BYTES);

      const sourcePath = join(temporary.directory, "near-limit-source.json");
      const evidenceDirectory = join(temporary.directory, "near-limit-evidence");
      const outputDirectory = join(temporary.directory, "near-limit.bundle");
      await mkdir(evidenceDirectory, { mode: 0o700 });
      await recorder.publishSource(sourcePath);
      assert.deepEqual(await readBundleSource(sourcePath), recorder.bundleSource());
      const receipt = await recorder.finalize({
        evidenceDirectory,
        executable: requireBundleExecutable(),
        outputDirectory,
        sourcePath,
      });
      assert.equal(receipt.eventCount, events.length);
      assert.equal(receipt.assetCount, 0);
      const completed = await loadRecorderCheckpoint(temporary.path);
      assert.equal(completed?.phase, "completed");
      assert.equal(completed?.revision, 3);
      const completedCheckpointBytes = (await stat(temporary.path)).size;
      assert.ok(
        completedCheckpointBytes > BUNDLE_SOURCE_MAX_BYTES,
        "completed metadata and checksum must exercise the independent checkpoint headroom",
      );
      assert.ok(completedCheckpointBytes <= RECORDER_CHECKPOINT_MAX_BYTES);
    } finally {
      await rm(temporary.directory, { force: true, recursive: true });
    }
  },
);

test(
  "seal rejects a BundleSource over 8 MiB before publishing a sealed checkpoint",
  { timeout: 60_000 },
  async () => {
    const temporary = await temporaryCheckpoint();
    const events = errorSession(1_000, "x".repeat(8_160));
    await commitRecorderCheckpoint(temporary.path, 0, terminalRecording(events));
    const source = new FakeSource(events);
    source.exportMaxSuccessfulLimit = 62;
    try {
      const recorder = await ExecutionRecorder.open({
        checkpointPath: temporary.path,
        eventSource: source,
        sessionId,
      });
      await assert.rejects(
        recorder.seal(),
        (error: unknown) => error instanceof RecorderError && error.code === "source_too_large",
      );
      const checkpoint = await loadRecorderCheckpoint(temporary.path);
      assert.equal(checkpoint?.phase, "recording");
      assert.equal(checkpoint?.revision, 1);
    } finally {
      await rm(temporary.directory, { force: true, recursive: true });
    }
  },
);

test("Recorder rejects SessionInfo drift between authoritative export pages", async () => {
  const temporary = await temporaryCheckpoint();
  const events = errorSession(1_000);
  const source = new FakeSource(events);
  source.exportPageTransform = (page, callIndex) => {
    if (callIndex !== 1) {
      return page;
    }
    const result = page as SessionExportResult;
    return {
      ...result,
      session: { ...result.session, endedAtMs: (result.session.endedAtMs ?? 0) + 1 },
    };
  };
  try {
    const recorder = await ExecutionRecorder.open({
      checkpointPath: temporary.path,
      eventSource: source,
      sessionId,
    });
    await assert.rejects(
      recorder.captureOnce(),
      (error: unknown) =>
        error instanceof RecorderError && error.code === "session_export_mismatch",
    );
    const checkpoint = await loadRecorderCheckpoint(temporary.path);
    assert.equal(checkpoint?.phase, "recording");
    assert.equal(checkpoint?.revision, 3);
    assert.deepEqual(checkpoint?.events, events);
  } finally {
    await rm(temporary.directory, { force: true, recursive: true });
  }
});

test("Recorder cancellation after an export page cannot seal the checkpoint", async () => {
  const temporary = await temporaryCheckpoint();
  const source = new FakeSource([started, ended]);
  let release!: () => void;
  source.exportPageHook = async () =>
    await new Promise<void>((resolve) => {
      release = resolve;
    });
  try {
    const recorder = await ExecutionRecorder.open({
      checkpointPath: temporary.path,
      eventSource: source,
      sessionId,
    });
    const controller = new AbortController();
    const capture = recorder.captureOnce({ signal: controller.signal });
    while (!release) {
      await new Promise((resolve) => setImmediate(resolve));
    }
    controller.abort();
    release();
    await assert.rejects(
      capture,
      (error: unknown) => error instanceof RecorderError && error.code === "operation_cancelled",
    );
    const checkpoint = await loadRecorderCheckpoint(temporary.path);
    assert.equal(checkpoint?.phase, "recording");
    assert.equal(checkpoint?.revision, 2);
    assert.deepEqual(checkpoint?.events, [started, ended]);
  } finally {
    release?.();
    await rm(temporary.directory, { force: true, recursive: true });
  }
});

test("concurrent seal and capture share one authoritative verification and one seal CAS", async () => {
  const temporary = await temporaryCheckpoint();
  const source = new FakeSource([started, ended]);
  await commitRecorderCheckpoint(
    temporary.path,
    0,
    terminalRecording([started, ended]),
  );
  let release!: () => void;
  let entered!: () => void;
  const pageEntered = new Promise<void>((resolve) => {
    entered = resolve;
  });
  const pageRelease = new Promise<void>((resolve) => {
    release = resolve;
  });
  let pageCalls = 0;
  source.exportPageHook = async () => {
    pageCalls += 1;
    entered();
    await pageRelease;
  };
  try {
    const recorder = await ExecutionRecorder.open({
      checkpointPath: temporary.path,
      eventSource: source,
      sessionId,
    });
    const sealing = recorder.seal();
    await pageEntered;
    const capturing = recorder.captureOnce();
    release();
    const [sealedSource, capture] = await Promise.all([sealing, capturing]);
    assert.equal(capture.phase, "sealed");
    assert.deepEqual(recorder.bundleSource(), sealedSource);
    assert.equal(pageCalls, 1);
    assert.equal(source.exportRequests.length, 1);
    assert.equal((await loadRecorderCheckpoint(temporary.path))?.revision, 2);

    assert.deepEqual(await recorder.seal(), sealedSource);
    assert.equal((await loadRecorderCheckpoint(temporary.path))?.revision, 2);
  } finally {
    release?.();
    await rm(temporary.directory, { force: true, recursive: true });
  }
});

test("a non-cancelled seal waiter retries after the owning seal is cancelled", async () => {
  const temporary = await temporaryCheckpoint();
  const source = new FakeSource([started, ended]);
  await commitRecorderCheckpoint(
    temporary.path,
    0,
    terminalRecording([started, ended]),
  );
  let release!: () => void;
  let entered!: () => void;
  const firstPageEntered = new Promise<void>((resolve) => {
    entered = resolve;
  });
  const firstPageRelease = new Promise<void>((resolve) => {
    release = resolve;
  });
  let pageCalls = 0;
  source.exportPageHook = async () => {
    pageCalls += 1;
    if (pageCalls === 1) {
      entered();
      await firstPageRelease;
    }
  };
  try {
    const recorder = await ExecutionRecorder.open({
      checkpointPath: temporary.path,
      eventSource: source,
      sessionId,
    });
    const ownerController = new AbortController();
    const owner = recorder.seal({ signal: ownerController.signal });
    await firstPageEntered;
    const waiter = recorder.seal();
    ownerController.abort();
    await assert.rejects(
      owner,
      (error: unknown) => error instanceof RecorderError && error.code === "operation_cancelled",
    );
    assert.equal((await loadRecorderCheckpoint(temporary.path))?.phase, "recording");
    release();
    const sealedSource = await waiter;
    assert.deepEqual(recorder.bundleSource(), sealedSource);
    assert.equal(pageCalls, 2);
    assert.equal((await loadRecorderCheckpoint(temporary.path))?.revision, 2);
  } finally {
    release?.();
    await rm(temporary.directory, { force: true, recursive: true });
  }
});

test("an aborted seal waiter returns promptly without cancelling the owning seal", async () => {
  const temporary = await temporaryCheckpoint();
  const source = new FakeSource([started, ended]);
  await commitRecorderCheckpoint(
    temporary.path,
    0,
    terminalRecording([started, ended]),
  );
  let release!: () => void;
  let entered!: () => void;
  const pageEntered = new Promise<void>((resolve) => {
    entered = resolve;
  });
  const pageRelease = new Promise<void>((resolve) => {
    release = resolve;
  });
  let pageCalls = 0;
  source.exportPageHook = async () => {
    pageCalls += 1;
    entered();
    await pageRelease;
  };
  try {
    const recorder = await ExecutionRecorder.open({
      checkpointPath: temporary.path,
      eventSource: source,
      sessionId,
    });
    const owner = recorder.seal();
    await pageEntered;
    const waiterController = new AbortController();
    const waiter = recorder.seal({ signal: waiterController.signal });
    waiterController.abort();
    await assert.rejects(
      waiter,
      (error: unknown) => error instanceof RecorderError && error.code === "operation_cancelled",
    );
    assert.equal((await loadRecorderCheckpoint(temporary.path))?.phase, "recording");
    release();
    await owner;
    assert.equal(pageCalls, 1);
    assert.equal((await loadRecorderCheckpoint(temporary.path))?.revision, 2);
  } finally {
    release?.();
    await rm(temporary.directory, { force: true, recursive: true });
  }
});

test("Recorder reports an authoritative export event that cannot fit at limit one", async () => {
  const temporary = await temporaryCheckpoint();
  const source = new FakeSource([started, ended]);
  source.exportMaxSuccessfulLimit = 0;
  try {
    const recorder = await ExecutionRecorder.open({
      checkpointPath: temporary.path,
      eventSource: source,
      sessionId,
    });
    await assert.rejects(
      recorder.captureOnce(),
      (error: unknown) => {
        assert.ok(error instanceof RecorderError);
        assert.equal(error.code, "event_too_large");
        assert.deepEqual(error.details, {
          afterSequence: null,
          pageLimit: 1,
          upstreamCode: "response_frame_too_large",
        });
        return true;
      },
    );
    assert.deepEqual(source.exportLimits, [1_000, 500, 250, 125, 62, 31, 15, 7, 3, 1]);
    const checkpoint = await loadRecorderCheckpoint(temporary.path);
    assert.equal(checkpoint?.phase, "recording");
    assert.equal(checkpoint?.revision, 2);
  } finally {
    await rm(temporary.directory, { force: true, recursive: true });
  }
});

test("Recorder keeps legacy full export compatibility without a page-capable source", async () => {
  const temporary = await temporaryCheckpoint();
  const backing = new FakeSource([started, ended]);
  const legacySource: RecorderEventSource = {
    describe: async () => await backing.describe(),
    exportSession: async () => await backing.exportSession(),
    listEvents: async (requestedSessionId, afterSequence, limit) =>
      await backing.listEvents(requestedSessionId, afterSequence, limit),
  };
  try {
    const recorder = await ExecutionRecorder.open({
      checkpointPath: temporary.path,
      eventSource: legacySource,
      sessionId,
    });
    assert.equal((await recorder.captureOnce()).phase, "sealed");
    assert.equal(backing.legacyExportCalls, 1);
    assert.deepEqual(backing.exportRequests, []);
  } finally {
    await rm(temporary.directory, { force: true, recursive: true });
  }
});

test("Recorder keeps legacy full export compatibility when paging was not negotiated", async () => {
  const temporary = await temporaryCheckpoint();
  const source = new FakeSource([started, ended]);
  source.pageFeatureEnabled = false;
  try {
    const recorder = await ExecutionRecorder.open({
      checkpointPath: temporary.path,
      eventSource: source,
      sessionId,
    });
    assert.equal((await recorder.captureOnce()).phase, "sealed");
    assert.equal(source.legacyExportCalls, 1);
    assert.deepEqual(source.exportRequests, []);
  } finally {
    await rm(temporary.directory, { force: true, recursive: true });
  }
});

test("Recorder durably captures and seals a protocol 1.4 media lifecycle", async () => {
  const temporary = await temporaryCheckpoint();
  const streamId = "33333333-3333-4333-8333-333333333333";
  const digest = "a".repeat(64);
  const mediaEvents = [
    started,
    {
      atMs: 120,
      eventId: "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaa2",
      payload: {
        type: "mediaStreamStarted",
        stream: { id: streamId, kind: "video", mediaType: "video/webm" },
      },
      sequence: 2,
      sessionId,
    },
    {
      atMs: 140,
      eventId: "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaa3",
      payload: {
        type: "mediaFrameCaptured",
        frame: {
          streamId,
          frameIndex: 1,
          keyFrame: true,
          durationMs: 20,
          evidence: {
            id: `sha256:${digest}`,
            mediaType: "video/webm",
            sha256: digest,
            uri: `devicerail://assets/sha256/${digest}`,
          },
        },
      },
      sequence: 3,
      sessionId,
    },
    {
      atMs: 160,
      eventId: "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaa4",
      payload: { type: "mediaStreamEnded", streamId, frameCount: 1 },
      sequence: 4,
      sessionId,
    },
    {
      atMs: 180,
      eventId: "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaa5",
      payload: { outcome: "completed", reason: null, type: "sessionEnded" },
      sequence: 5,
      sessionId,
    },
  ] satisfies TestEvent[];
  try {
    const recorder = await ExecutionRecorder.open({
      checkpointPath: temporary.path,
      eventSource: new FakeSource(mediaEvents),
      sessionId,
    });
    const capture = await recorder.captureOnce();
    assert.equal(capture.phase, "sealed");
    assert.equal(capture.accepted, 5);
    assert.deepEqual(recorder.bundleSource(), {
      eventProtocolVersion: { major: 1, minor: 4 },
      sessionExport: exportFor(mediaEvents),
    });
    const checkpoint = await loadRecorderCheckpoint(temporary.path);
    assert.equal(checkpoint?.eventProtocolVersion.minor, 4);
    assert.deepEqual(checkpoint?.events, mediaEvents);
  } finally {
    await rm(temporary.directory, { force: true, recursive: true });
  }
});

test("cancellation after an upstream read cannot advance the checkpoint", async () => {
  const temporary = await temporaryCheckpoint();
  try {
    const source = new FakeSource([started]);
    let release!: () => void;
    source.listHook = async () =>
      await new Promise<void>((resolve) => {
        release = resolve;
      });
    const recorder = await ExecutionRecorder.open({
      checkpointPath: temporary.path,
      eventSource: source,
      sessionId,
    });
    const controller = new AbortController();
    const capture = recorder.captureOnce({ signal: controller.signal });
    while (!release) {
      await new Promise((resolve) => setImmediate(resolve));
    }
    controller.abort();
    release();
    await assert.rejects(
      capture,
      (error: unknown) => error instanceof RecorderError && error.code === "operation_cancelled",
    );
    const checkpoint = await loadRecorderCheckpoint(temporary.path);
    assert.equal(checkpoint?.revision, 1);
    assert.deepEqual(checkpoint?.events, []);
  } finally {
    await rm(temporary.directory, { force: true, recursive: true });
  }
});

test("a changed authoritative export leaves the terminal checkpoint unsealed", async () => {
  const temporary = await temporaryCheckpoint();
  try {
    const source = new FakeSource([started, ended]);
    source.exportOverride = {
      ...exportFor([started, ended]),
      events: [started, { ...ended, atMs: 201 }],
    };
    const recorder = await ExecutionRecorder.open({
      checkpointPath: temporary.path,
      eventSource: source,
      sessionId,
    });
    await assert.rejects(
      recorder.captureOnce(),
      (error: unknown) =>
        error instanceof RecorderError && error.code === "session_export_mismatch",
    );
    const checkpoint = await loadRecorderCheckpoint(temporary.path);
    assert.equal(checkpoint?.phase, "recording");
    assert.deepEqual(checkpoint?.events, [started, ended]);
  } finally {
    await rm(temporary.directory, { force: true, recursive: true });
  }
});

test("two stale Recorder instances cannot both commit the same revision", async () => {
  const temporary = await temporaryCheckpoint();
  try {
    const empty = new FakeSource([]);
    await ExecutionRecorder.open({
      checkpointPath: temporary.path,
      eventSource: empty,
      sessionId,
    });
    const source = new FakeSource([started]);
    const left = await ExecutionRecorder.open({
      checkpointPath: temporary.path,
      eventSource: source,
      sessionId,
    });
    const right = await ExecutionRecorder.open({
      checkpointPath: temporary.path,
      eventSource: source,
      sessionId,
    });
    await left.captureOnce();
    await assert.rejects(
      right.captureOnce(),
      (error: unknown) =>
        error instanceof RecorderError && error.code === "checkpoint_conflict",
    );
    const checkpoint = await loadRecorderCheckpoint(temporary.path);
    assert.equal(checkpoint?.revision, 2);
    assert.deepEqual(checkpoint?.events, [started]);
  } finally {
    await rm(temporary.directory, { force: true, recursive: true });
  }
});
