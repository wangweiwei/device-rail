import assert from "node:assert/strict";
import { access, readFile, stat, writeFile } from "node:fs/promises";
import { join } from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";

import type {
  ProtocolVersion,
  SessionExportResult,
  SystemDescribeResult,
  TestEvent,
} from "@devicerail/protocol";

import {
  DeviceRailRecorderEventSource,
  ExecutionRecorder,
  loadRecorderCheckpoint,
  readBundleSource,
  RecorderError,
  type RecorderEventSource,
} from "../src/index.js";
import {
  requireBundleExecutable,
  spawnTestDaemon,
} from "./daemon-harness.js";

test(
  "Mock daemon recording resumes and closes through the real Bundle validator",
  { timeout: 45_000 },
  async (context) => {
    const daemon = await spawnTestDaemon(context);
    const { client } = daemon;
    const description = await client.call("system.describe");
    assert.ok(
      description.connection.features.enabled.includes("session.export.page.v1"),
      "real daemon handshake must negotiate bounded authoritative export",
    );
    const device = (await client.call("devices.list")).devices[0];
    assert.ok(device);
    await client.call("device.select", { deviceId: device.id });
    await client.call("device.connect");
    const session = await client.call("session.start");

    const checkpointPath = join(daemon.temporaryDirectory, "recording.checkpoint.json");
    const firstRecorder = await ExecutionRecorder.open({
      checkpointPath,
      client,
      sessionId: session.id,
    });
    assert.equal((await firstRecorder.captureOnce()).lastSequence, 1);

    const daemonSource = new DeviceRailRecorderEventSource(client);
    let legacyExportCalls = 0;
    let pagedExportCalls = 0;
    const instrumentedSource: RecorderEventSource = {
      describe: async () => await daemonSource.describe(),
      exportSession: async (requestedSessionId) => {
        legacyExportCalls += 1;
        return await daemonSource.exportSession(requestedSessionId);
      },
      exportSessionPage: async (requestedSessionId, afterSequence, limit) => {
        pagedExportCalls += 1;
        return await daemonSource.exportSessionPage(requestedSessionId, afterSequence, limit);
      },
      listEvents: async (requestedSessionId, afterSequence, limit) =>
        await daemonSource.listEvents(requestedSessionId, afterSequence, limit),
    };
    const recorder = await ExecutionRecorder.open({
      checkpointPath,
      eventSource: instrumentedSource,
      sessionId: session.id,
    });
    const [tap, input] = await Promise.all([
      client.call("device.execute", {
        arguments: { x: 12, y: 34 },
        id: "33333333-3333-4333-8333-333333333331",
        name: "tap",
      }),
      client.call("device.execute", {
        arguments: { text: "Recorder" },
        id: "33333333-3333-4333-8333-333333333332",
        name: "inputText",
      }),
    ]);
    assert.equal(tap.callId, "33333333-3333-4333-8333-333333333331");
    assert.equal(input.callId, "33333333-3333-4333-8333-333333333332");
    const actionBatch = await recorder.captureOnce();
    assert.equal(actionBatch.accepted, 4);
    assert.equal(actionBatch.phase, "recording");

    const ended = await client.call("session.end", {
      outcome: "completed",
      reason: "DR-018 Recorder E2E",
    });
    const terminal = await recorder.captureOnce();
    assert.equal(terminal.phase, "sealed");
    assert.equal(terminal.lastSequence, ended.lastSequence);
    assert.ok(pagedExportCalls > 0);
    assert.equal(legacyExportCalls, 0);

    const firstExportPage = await client.call("session.export", {
      limit: 2,
      sessionId: session.id,
    });
    assert.equal(firstExportPage.events.length, 2);
    assert.equal(firstExportPage.nextAfterSequence, 2);
    const finalExportPage = await client.call("session.export", {
      afterSequence: firstExportPage.nextAfterSequence,
      limit: 100,
      sessionId: session.id,
    });
    assert.equal(finalExportPage.nextAfterSequence, undefined);

    const authoritative = await client.call("session.export", { sessionId: session.id });
    assert.deepEqual(
      [...firstExportPage.events, ...finalExportPage.events],
      authoritative.events,
    );
    const source = recorder.bundleSource();
    assert.deepEqual(source.sessionExport, authoritative);
    assert.deepEqual(source.eventProtocolVersion, { major: 1, minor: 4 });
    assert.equal(
      source.sessionExport.events.filter((event) => event.payload.type === "actionStarted").length,
      2,
    );
    assert.equal(
      source.sessionExport.events.filter((event) => event.payload.type === "actionCompleted").length,
      2,
    );

    const sourcePath = join(daemon.temporaryDirectory, "session-source.json");
    const outputDirectory = join(daemon.temporaryDirectory, "session.bundle");
    await recorder.publishSource(sourcePath);
    if (process.platform !== "win32") {
      assert.equal((await stat(sourcePath)).mode & 0o077, 0);
    }
    assert.deepEqual(await readBundleSource(sourcePath), source);

    await client.call("device.disconnect");
    await client.close();
    const receipt = await recorder.finalize({
      evidenceDirectory: daemon.evidenceDirectory,
      executable: requireBundleExecutable(),
      outputDirectory,
      sourcePath,
    });
    assert.equal(receipt.sessionId, session.id);
    assert.equal(receipt.eventCount, authoritative.events.length);
    assert.ok(receipt.assetCount > 0);
    assert.ok(receipt.assetBytes > 0);
    const manifest = JSON.parse(await readFile(join(outputDirectory, "manifest.json"), "utf8"));
    assert.deepEqual(manifest.events, authoritative.events);
    assert.deepEqual(manifest.session, authoritative.session);

    const completed = await ExecutionRecorder.openOffline({ checkpointPath });
    assert.equal(completed.phase, "completed");
    assert.deepEqual(completed.checkpoint.bundle, receipt);
    assert.deepEqual(
      await completed.finalize({
        evidenceDirectory: daemon.evidenceDirectory,
        executable: requireBundleExecutable(),
        outputDirectory,
        sourcePath,
      }),
      receipt,
    );
  },
);

test(
  "protected omission survives Recorder and the real zero-asset Bundle boundary",
  { timeout: 45_000 },
  async (context) => {
    const daemon = await spawnTestDaemon(context);
    await daemon.client.close();
    const fixturePath = fileURLToPath(
      new URL(
        "../../../../crates/session-bundle/tests/fixtures/protected-omission/manifest.json",
        import.meta.url,
      ),
    );
    const fixture = JSON.parse(await readFile(fixturePath, "utf8")) as {
      eventProtocolVersion: ProtocolVersion;
      events: TestEvent[];
      session: SessionExportResult["session"];
    };
    const exported: SessionExportResult = {
      events: fixture.events,
      session: fixture.session,
    };
    const described = {
      activeSessionId: null,
      client: { name: "protected-fixture", version: "0.1.0" },
      connection: {
        connectionId: "55555555-5555-4555-8555-555555555555",
        features: { enabled: ["action.protected.v1", "events.snapshot.v1"] },
        protocol: { selected: fixture.eventProtocolVersion },
        server: { name: "fixture", version: "0.1.0" },
        transport: { framing: "fixture", kind: "offline-test" },
      },
      deviceId: null,
    } satisfies SystemDescribeResult;
    const eventSource = {
      async describe() {
        return described;
      },
      async listEvents(_sessionId: string, afterSequence: number | null) {
        return fixture.events.filter(
          (event) => afterSequence === null || event.sequence > afterSequence,
        );
      },
      async exportSession() {
        return exported;
      },
    } satisfies RecorderEventSource;

    const checkpointPath = join(daemon.temporaryDirectory, "protected.checkpoint.json");
    const recorder = await ExecutionRecorder.open({
      checkpointPath,
      eventSource,
      sessionId: fixture.session.id,
    });
    assert.equal((await recorder.captureOnce()).phase, "sealed");
    const source = recorder.bundleSource();
    assert.deepEqual(source.sessionExport, exported);
    const actionStarted = source.sessionExport.events.find(
      (event) => event.payload.type === "actionStarted",
    );
    assert.equal(actionStarted?.payload.type, "actionStarted");
    if (actionStarted?.payload.type === "actionStarted") {
      assert.equal(actionStarted.payload.call.arguments, null);
      assert.equal(actionStarted.payload.call.argumentsRedacted, true);
    }
    const actionCompleted = source.sessionExport.events.find(
      (event) => event.payload.type === "actionCompleted",
    );
    assert.equal(actionCompleted?.payload.type, "actionCompleted");
    if (
      actionCompleted?.payload.type === "actionCompleted" &&
      actionCompleted.payload.outcome.outcome === "succeeded"
    ) {
      const result = actionCompleted.payload.outcome.result;
      assert.deepEqual(result.evidence, []);
      assert.equal(result.before?.screenshot, null);
      assert.equal(result.before?.screenshotOmission, "protectedAction");
      assert.equal(result.after?.screenshot, null);
      assert.equal(result.after?.screenshotOmission, "protectedAction");
    } else {
      assert.fail("protected fixture must contain one successful Action");
    }

    const sourcePath = join(daemon.temporaryDirectory, "protected-source.json");
    const outputDirectory = join(daemon.temporaryDirectory, "protected.bundle");
    const receipt = await recorder.finalize({
      evidenceDirectory: daemon.evidenceDirectory,
      executable: requireBundleExecutable(),
      outputDirectory,
      sourcePath,
    });
    assert.equal(receipt.assetCount, 0);
    assert.equal(receipt.assetBytes, 0);
    assert.equal((await loadRecorderCheckpoint(checkpointPath))?.phase, "completed");
  },
);

test(
  "corrupt Evidence cannot publish or complete a sealed recording",
  { timeout: 45_000 },
  async (context) => {
    const daemon = await spawnTestDaemon(context);
    const { client } = daemon;
    const device = (await client.call("devices.list")).devices[0];
    assert.ok(device);
    await client.call("device.select", { deviceId: device.id });
    await client.call("device.connect");
    const session = await client.call("session.start");
    const observation = await client.call("device.observe");
    assert.ok(observation.screenshot?.sha256);
    await client.call("session.end");

    const checkpointPath = join(daemon.temporaryDirectory, "corrupt.checkpoint.json");
    const recorder = await ExecutionRecorder.open({
      checkpointPath,
      client,
      sessionId: session.id,
    });
    assert.equal((await recorder.captureOnce()).phase, "sealed");
    const sourcePath = join(daemon.temporaryDirectory, "corrupt-source.json");
    const outputDirectory = join(daemon.temporaryDirectory, "corrupt.bundle");
    await recorder.publishSource(sourcePath);
    await client.close();

    const digest = observation.screenshot.sha256;
    const objectData = join(
      daemon.evidenceDirectory,
      "v1",
      "objects",
      "sha256",
      digest.slice(0, 2),
      digest.slice(2, 4),
      digest,
      "data",
    );
    await writeFile(objectData, "corrupt Evidence bytes", "utf8");
    await assert.rejects(
      recorder.finalize({
        evidenceDirectory: daemon.evidenceDirectory,
        executable: requireBundleExecutable(),
        outputDirectory,
        sourcePath,
      }),
      (error: unknown) => error instanceof RecorderError && error.code === "bundle_cli_failed",
    );
    await assert.rejects(access(outputDirectory));
    assert.equal((await loadRecorderCheckpoint(checkpointPath))?.phase, "sealed");
  },
);
