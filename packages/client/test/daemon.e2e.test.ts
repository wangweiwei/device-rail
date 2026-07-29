import assert from "node:assert/strict";
import { accessSync, constants, mkdtempSync, rmSync } from "node:fs";
import { isAbsolute, join, resolve } from "node:path";
import test, { type TestContext } from "node:test";
import { tmpdir } from "node:os";
import { fileURLToPath } from "node:url";

import type { HelloParams } from "@devicerail/protocol";

import { DeviceRailClient } from "../src/index.js";

const hello = {
  client: {
    name: "devicerail-client-e2e",
    version: "0.1.0",
  },
  protocol: {
    ranges: [{ major: 1, minMinor: 0, maxMinor: 2 }],
  },
  features: {
    required: ["device.routing.v1", "events.snapshot.v1", "request.control.v1"],
  },
} satisfies HelloParams;

const streamHello = {
  client: {
    name: "devicerail-client-stream-e2e",
    version: "0.1.0",
  },
  protocol: {
    ranges: [{ major: 1, minMinor: 0, maxMinor: 3 }],
  },
  features: {
    required: ["device.routing.v1", "events.snapshot.v1", "request.control.v1"],
    optional: ["events.stream.v1"],
  },
} satisfies HelloParams;

const mediaHello = {
  client: {
    name: "devicerail-client-media-e2e",
    version: "0.1.0",
  },
  protocol: {
    ranges: [{ major: 1, minMinor: 4, maxMinor: 4 }],
  },
  features: {
    required: [
      "device.routing.v1",
      "events.snapshot.v1",
      "media.stream.v1",
      "request.control.v1",
    ],
  },
} satisfies HelloParams;

const legacyHello = {
  client: {
    name: "devicerail-client-e2e-legacy",
    version: "0.1.0",
  },
  protocol: {
    ranges: [{ major: 1, minMinor: 0, maxMinor: 0 }],
  },
  features: {
    optional: ["events.snapshot.v1"],
  },
} satisfies HelloParams;

function workspaceRoot(): string {
  // Tests execute from packages/client/.test-dist/test, independent of the
  // shell's current directory or pnpm's package working directory.
  return fileURLToPath(new URL("../../../../", import.meta.url));
}

function daemonExecutable(): string {
  const configured = process.env.DEVICERAIL_DAEMON_BIN;
  if (configured) {
    return isAbsolute(configured) ? configured : resolve(configured);
  }

  const root = workspaceRoot();
  const configuredTarget = process.env.CARGO_TARGET_DIR;
  const target = configuredTarget
    ? isAbsolute(configuredTarget)
      ? configuredTarget
      : resolve(root, configuredTarget)
    : join(root, "target");
  const executable =
    process.platform === "win32"
      ? "devicerail-daemon.exe"
      : "devicerail-daemon";
  return join(target, "debug", executable);
}

function requireDaemonExecutable(): string {
  const executable = daemonExecutable();
  try {
    accessSync(
      executable,
      process.platform === "win32" ? constants.F_OK : constants.X_OK,
    );
  } catch (cause) {
    assert.fail(
      `DeviceRail daemon executable is unavailable at ${executable}; ` +
        "run `cargo build -p devicerail-daemon` or set DEVICERAIL_DAEMON_BIN" +
        (cause instanceof Error ? ` (${cause.message})` : ""),
    );
  }
  return executable;
}

async function spawnTestDaemon(
  context: TestContext,
  helloParams: HelloParams,
  environment: Readonly<Record<string, string>> = {},
): Promise<DeviceRailClient> {
  const evidenceDir = mkdtempSync(join(tmpdir(), "devicerail-client-e2e-"));
  let client: DeviceRailClient | undefined;
  context.after(async () => {
    if (client?.state !== "closed") {
      await client?.close().catch(() => {});
    }
    rmSync(evidenceDir, { force: true, recursive: true });
  });
  try {
    client = await DeviceRailClient.spawn({
      closeGraceMs: 5_000,
      command: requireDaemonExecutable(),
      hello: helloParams,
      spawn: {
        env: {
          ...process.env,
          DEVICERAIL_ANDROID: "off",
          DEVICERAIL_EVIDENCE_DIR: evidenceDir,
          ...environment,
        },
      },
    });
    return client;
  } catch (error) {
    rmSync(evidenceDir, { force: true, recursive: true });
    throw error;
  }
}

function assertCanonicalAsset(
  asset:
    | { id: string; sha256?: string | null; uri: string }
    | null
    | undefined,
): void {
  assert.ok(asset);
  const match = /^devicerail:\/\/assets\/sha256\/([0-9a-f]{64})$/u.exec(
    asset.uri,
  );
  assert.ok(match);
  const digest = match[1];
  assert.ok(digest);
  assert.equal(asset.sha256, digest);
  assert.equal(asset.id, `sha256:${digest}`);
}

test(
  "stdio client completes the Mock daemon workflow and closes cleanly",
  { timeout: 30_000 },
  async (context) => {
    const client = await spawnTestDaemon(context, hello);

    assert.equal(client.state, "ready");
    assert.deepEqual([...client.enabledFeatures].sort(), [
      "device.routing.v1",
      "events.snapshot.v1",
      "request.control.v1",
    ]);

    const listed = await client.call("devices.list");
    assert.equal(listed.devices.length, 1);
    const device = listed.devices[0];
    assert.ok(device);
    assert.equal(device.id, "mock-1");
    assert.equal(device.platform.kind, "mock");

    const selected = await client.call("device.select", {
      deviceId: device.id,
    });
    assert.equal(selected.device.id, device.id);

    const described = await client.call("system.describe");
    assert.equal(described.connection.protocol.selected.major, 1);
    assert.equal(described.connection.protocol.selected.minor, 2);
    assert.equal(described.connection.transport.kind, "stdio");
    assert.equal(described.connection.transport.framing, "ndjson");
    assert.equal(described.client.name, hello.client.name);
    assert.equal(described.deviceId, device.id);

    const connected = await client.call("device.connect");
    assert.equal(connected.id, device.id);
    assert.equal(connected.connected, true);

    const capabilities = await client.call("device.capabilities");
    assert.deepEqual(
      capabilities.map((capability) => capability.name),
      ["tap", "inputText", "scroll"],
    );

    // Each response echoes a distinct target request ID. This verifies that 50
    // simultaneously pending JSON-RPC calls are associated with the right
    // promise without depending on response scheduling.
    const cancellationTargets = Array.from(
      { length: 50 },
      (_value, index) => `e2e-completed-or-missing-${index}`,
    );
    const cancellations = await Promise.all(
      cancellationTargets.map((requestId) => client.cancel(requestId)),
    );
    cancellations.forEach((result, index) => {
      assert.equal(result.requestId, cancellationTargets[index]);
      assert.equal(result.status, "notFound");
    });
    assert.equal(client.pendingRequests, 0);

    const session = await client.call("session.start");
    assert.equal(session.state, "active");
    assert.equal(session.eventCount, 1);
    assert.equal(session.lastSequence, 1);

    const current = await client.call("session.current");
    assert.equal(current.sessionId, session.id);

    const observation = await client.call("device.observe", undefined, {
      timeoutMs: 5_000,
    });
    assert.equal(observation.deviceId, device.id);
    assert.equal(observation.viewport.width, 1280);
    assert.equal(observation.viewport.height, 720);
    assertCanonicalAsset(observation.screenshot);

    const action = await client.call(
      "device.execute",
      {
        actionTimeoutMs: 5_000,
        arguments: { x: 12.5, y: 34.5 },
        id: "00000000-0000-4000-8000-000000000010",
        name: "tap",
      },
      { timeoutMs: 10_000 },
    );
    assert.equal(action.callId, "00000000-0000-4000-8000-000000000010");
    assert.deepEqual(action.output, { accepted: true, x: 12.5, y: 34.5 });
    assert.equal(action.after?.deviceId, device.id);
    assertCanonicalAsset(action.evidence?.[0]);

    const activeEvents = await client.call("events.list", {
      sessionId: session.id,
    });
    assert.deepEqual(
      activeEvents.map((event) => event.sequence),
      [1, 2, 3, 4],
    );
    assert.deepEqual(
      activeEvents.map((event) => event.payload.type),
      [
        "sessionStarted",
        "observationCaptured",
        "actionStarted",
        "actionCompleted",
      ],
    );
    assert.ok(activeEvents.every((event) => event.sessionId === session.id));
    assert.ok(
      activeEvents.every(
        (event) => event.deviceId === undefined || event.deviceId === device.id,
      ),
    );

    const afterObservation = await client.call("events.list", {
      afterSequence: 2,
      sessionId: session.id,
    });
    assert.deepEqual(
      afterObservation.map((event) => event.sequence),
      [3, 4],
    );

    const ended = await client.call("session.end", {
      outcome: "completed",
      reason: "TypeScript stdio client E2E complete",
    });
    assert.equal(ended.id, session.id);
    assert.equal(ended.state, "ended");
    assert.equal(ended.eventCount, 5);
    assert.equal(ended.lastSequence, 5);

    const sessions = await client.call("sessions.list");
    assert.ok(
      sessions.some(
        (candidate) =>
          candidate.id === session.id && candidate.state === "ended",
      ),
    );

    const exported = await client.call("session.export", {
      sessionId: session.id,
    });
    assert.equal(exported.session.id, session.id);
    assert.equal(exported.session.state, "ended");
    assert.deepEqual(
      exported.events.map((event) => event.payload.type),
      [
        "sessionStarted",
        "observationCaptured",
        "actionStarted",
        "actionCompleted",
        "sessionEnded",
      ],
    );

    const cleared = await client.call("events.clear", {
      sessionId: session.id,
    });
    assert.deepEqual(cleared, { deleted: true, sessionId: session.id });

    const disconnected = await client.call("device.disconnect");
    assert.equal(disconnected.disconnected, true);

    await client.close();
    assert.equal(client.state, "closed");
    assert.equal(client.pendingRequests, 0);
    await client.close();
  },
);

test(
  "global screenshot omission policy crosses the stdio client boundary",
  { timeout: 30_000 },
  async (context) => {
    const client = await spawnTestDaemon(context, hello, {
      DEVICERAIL_SCREENSHOT_POLICY: "omit",
    });
    const listed = await client.call("devices.list");
    const device = listed.devices[0];
    assert.ok(device);
    await client.call("device.select", { deviceId: device.id });
    await client.call("device.connect");
    await client.call("session.start");

    const observation = await client.call("device.observe");
    const omission = observation as typeof observation & {
      readonly screenshotOmission?: string;
    };
    assert.equal(observation.screenshot, null);
    assert.equal(omission.screenshotOmission, "policy");

    const action = await client.call("device.execute", {
      arguments: { x: 10, y: 20 },
      id: "00000000-0000-4000-8000-000000000020",
      name: "tap",
    });
    const after = action.after as (typeof action.after & {
      readonly screenshotOmission?: string;
    });
    assert.equal(after?.screenshot, null);
    assert.equal(after?.screenshotOmission, "policy");
    assert.deepEqual(action.evidence, []);

    await client.call("session.end");
    await client.call("device.disconnect");
    await client.close();
  },
);

test(
  "real daemon media RPC records one retry-safe Evidence-backed lifecycle",
  { timeout: 30_000 },
  async (context) => {
    const client = await spawnTestDaemon(context, mediaHello);
    assert.equal(client.enabledFeatures.has("media.stream.v1"), true);

    const listed = await client.call("devices.list");
    const device = listed.devices[0];
    assert.ok(device);
    await client.call("device.select", { deviceId: device.id });
    await client.call("device.connect");
    const session = await client.call("session.start");
    const streamId = "77777777-7777-4777-8777-777777777778";

    const started = await client.call("media.stream.start", {
      streamId,
      kind: "screenshot",
    });
    assert.deepEqual(started.stream, {
      id: streamId,
      kind: "screenshot",
      mediaType: "image/png",
    });

    const captureParams = { streamId, frameIndex: 1 } as const;
    const captured = await client.call("media.stream.capture", captureParams, {
      timeoutMs: 5_000,
    });
    assert.equal(captured.frame.streamId, streamId);
    assert.equal(captured.frame.frameIndex, 1);
    assertCanonicalAsset(captured.frame.evidence);

    const retry = await client.call("media.stream.capture", captureParams, {
      timeoutMs: 5_000,
    });
    assert.deepEqual(retry, captured);

    const ended = await client.call("media.stream.end", { streamId });
    assert.deepEqual(ended, { streamId, frameCount: 1 });
    const events = await client.call("events.list", { sessionId: session.id });
    assert.deepEqual(
      events.map((event) => event.payload.type),
      [
        "sessionStarted",
        "mediaStreamStarted",
        "observationCaptured",
        "mediaFrameCaptured",
        "mediaStreamEnded",
      ],
    );
    const frameEvents = events.filter(
      (event) => event.payload.type === "mediaFrameCaptured",
    );
    assert.equal(frameEvents.length, 1, "an exact retry must not append a second frame event");

    await client.call("session.end", {
      outcome: "completed",
      reason: "TypeScript media RPC E2E complete",
    });
    await client.call("device.disconnect");
    await client.close();
  },
);

test(
  "event stream forms one confirmed snapshot-to-live prefix and resumes an ended Session",
  { timeout: 30_000 },
  async (context) => {
    const client = await spawnTestDaemon(context, streamHello);
    if (!client.enabledFeatures.has("events.stream.v1")) {
      await client.close();
      if (process.env.DEVICERAIL_ALLOW_NO_LOOPBACK === "1") {
        context.skip("this hermetic runner explicitly permits no loopback WebSocket bind");
        return;
      }
      assert.fail(
        "daemon did not enable events.stream.v1; set DEVICERAIL_ALLOW_NO_LOOPBACK=1 only in a runner that forbids loopback binds",
      );
    }

    const listed = await client.call("devices.list");
    const device = listed.devices[0];
    assert.ok(device);
    await client.call("device.select", { deviceId: device.id });
    await client.call("device.connect");
    const session = await client.call("session.start");
    const stream = await client.openEventStream({ sessionId: session.id });

    await client.call("device.observe");
    await client.call("device.execute", {
      arguments: { x: 20, y: 30 },
      id: "00000000-0000-4000-8000-000000000030",
      name: "tap",
    });
    await client.call("session.end", {
      outcome: "completed",
      reason: "TypeScript WebSocket stream E2E complete",
    });

    const sequences: number[] = [];
    const eventTypes: string[] = [];
    for await (const item of stream) {
      sequences.push(item.event.sequence);
      eventTypes.push(item.event.payload.type);
      assert.deepEqual(item.confirm(), item.cursor);
    }
    assert.deepEqual(sequences, [1, 2, 3, 4, 5]);
    assert.deepEqual(eventTypes, [
      "sessionStarted",
      "observationCaptured",
      "actionStarted",
      "actionCompleted",
      "sessionEnded",
    ]);
    assert.equal(stream.confirmedCursor?.sequence, 5);
    assert.equal(stream.terminal?.termination.reason, "sessionEnded");

    const resumed = await stream.resume();
    assert.equal(resumed.confirmedCursor?.sequence, 5);
    assert.deepEqual(await resumed.next(), { done: true, value: undefined });
    assert.equal(resumed.terminal?.termination.reason, "sessionEnded");

    await client.call("device.disconnect");
    await client.close();
  },
);

test(
  "protocol 1.0 retains the single-device legacy route",
  { timeout: 30_000 },
  async (context) => {
    const client = await spawnTestDaemon(context, legacyHello);

    const described = await client.call("system.describe");
    assert.deepEqual(described.connection.protocol.selected, {
      major: 1,
      minor: 0,
    });
    assert.equal(described.deviceId, "mock-1");
    assert.equal(client.enabledFeatures.has("device.routing.v1"), false);
    assert.equal(client.enabledFeatures.has("request.control.v1"), false);

    const connected = await client.call("device.connect");
    assert.equal(connected.id, "mock-1");
    assert.equal(connected.connected, true);

    const session = await client.call("session.start");
    const observation = await client.call("device.observe");
    assert.equal(observation.deviceId, "mock-1");
    const ended = await client.call("session.end");
    assert.equal(ended.id, session.id);
    assert.equal(ended.state, "ended");

    const disconnected = await client.call("device.disconnect");
    assert.equal(disconnected.disconnected, true);
    await client.close();
    assert.equal(client.state, "closed");
  },
);
