import assert from "node:assert/strict";
import { accessSync, constants, mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { isAbsolute, join, resolve } from "node:path";
import test, { type TestContext } from "node:test";
import { fileURLToPath } from "node:url";

import { DeviceRailClient } from "@devicerail/client";
import type { HelloParams } from "@devicerail/protocol";

import {
  actionToolName,
  DeviceRailToolAdapter,
  OBSERVATION_TOOL_NAME,
} from "../src/index.js";

const hello = {
  client: {
    name: "devicerail-tool-adapter-e2e",
    version: "0.1.0",
  },
  protocol: {
    ranges: [{ major: 1, minMinor: 0, maxMinor: 2 }],
  },
  features: {
    required: ["device.routing.v1", "events.snapshot.v1", "request.control.v1"],
  },
} satisfies HelloParams;

function workspaceRoot(): string {
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
  return join(
    target,
    "debug",
    process.platform === "win32" ? "devicerail-daemon.exe" : "devicerail-daemon",
  );
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
  environment: Readonly<Record<string, string>> = {},
): Promise<DeviceRailClient> {
  const evidenceDir = mkdtempSync(
    join(tmpdir(), "devicerail-tool-adapter-e2e-"),
  );
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
      hello,
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
  "Tool Catalog discovers and invokes every Mock action without owning lifecycle",
  { timeout: 30_000 },
  async (context) => {
    const client = await spawnTestDaemon(context);

    const devices = await client.call("devices.list");
    const device = devices.devices[0];
    assert.ok(device);
    await client.call("device.select", { deviceId: device.id });
    await client.call("device.connect");
    const session = await client.call("session.start");

    const adapter = new DeviceRailToolAdapter(client);
    const catalog = await adapter.discover({ requestTimeoutMs: 5_000 });
    assert.equal(catalog.revision, 1);
    assert.equal(Object.isFrozen(catalog), true);
    assert.equal(Object.isFrozen(catalog.tools), true);
    assert.deepEqual(
      catalog.tools.map((tool) => tool.name),
      [
        OBSERVATION_TOOL_NAME,
        actionToolName("inputText"),
        actionToolName("scroll"),
        actionToolName("tap"),
      ],
    );
    assert.ok(catalog.tools.every((tool) => Object.isFrozen(tool.inputSchema)));

    const observed = await catalog.invoke(
      {
        invocationId: "agent-observe-1",
        name: OBSERVATION_TOOL_NAME,
      },
      { requestTimeoutMs: 5_000 },
    );
    assert.equal(observed.kind, "observation");
    assert.equal(observed.invocationId, "agent-observe-1");
    assert.equal(observed.observation.deviceId, device.id);
    assertCanonicalAsset(observed.observation.screenshot);

    const calls = [
      ["tap", { x: 12.5, y: 34.5 }, "agent-action-tap"],
      ["inputText", { text: "DeviceRail" }, "agent-action-input"],
      ["scroll", { deltaX: 0, deltaY: 240 }, "agent-action-scroll"],
    ] as const;
    for (const [actionName, argumentsValue, invocationId] of calls) {
      const result = await catalog.invoke(
        {
          arguments: argumentsValue,
          invocationId,
          name: actionToolName(actionName),
        },
        { actionTimeoutMs: 5_000, requestTimeoutMs: 10_000 },
      );
      assert.equal(result.kind, "action");
      assert.equal(result.actionName, actionName);
      assert.equal(result.invocationId, invocationId);
      assert.notEqual(result.actionCallId, invocationId);
      assert.match(
        result.actionCallId,
        /^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/iu,
      );
      assert.equal(result.action.callId, result.actionCallId);
      assert.equal(result.action.after?.deviceId, device.id);
      assert.equal(result.action.evidence?.length, 1);
      assertCanonicalAsset(result.action.evidence?.[0]);
    }

    const events = await client.call("events.list", { sessionId: session.id });
    assert.deepEqual(
      events.map((event) => event.payload.type),
      [
        "sessionStarted",
        "observationCaptured",
        "actionStarted",
        "actionCompleted",
        "actionStarted",
        "actionCompleted",
        "actionStarted",
        "actionCompleted",
      ],
    );

    await client.call("session.end", {
      outcome: "completed",
      reason: "Tool Adapter E2E complete",
    });
    await client.call("device.disconnect");
    await client.close();
    assert.equal(client.state, "closed");
  },
);

test(
  "Tool Adapter accepts explicitly omitted screenshots without inventing evidence",
  { timeout: 30_000 },
  async (context) => {
    const client = await spawnTestDaemon(context, {
      DEVICERAIL_SCREENSHOT_POLICY: "omit",
    });
    const devices = await client.call("devices.list");
    const device = devices.devices[0];
    assert.ok(device);
    await client.call("device.select", { deviceId: device.id });
    await client.call("device.connect");
    await client.call("session.start");

    const catalog = await new DeviceRailToolAdapter(client).discover();
    const observed = await catalog.invoke({ name: OBSERVATION_TOOL_NAME });
    assert.equal(observed.kind, "observation");
    const observation = observed.observation as typeof observed.observation & {
      readonly screenshotOmission?: string;
    };
    assert.equal(observation.screenshot, null);
    assert.equal(observation.screenshotOmission, "policy");

    const action = await catalog.invoke({
      arguments: { x: 10, y: 20 },
      name: actionToolName("tap"),
    });
    assert.equal(action.kind, "action");
    assert.deepEqual(action.action.evidence, []);
    const after = action.action.after as (typeof action.action.after & {
      readonly screenshotOmission?: string;
    });
    assert.equal(after?.screenshot, null);
    assert.equal(after?.screenshotOmission, "policy");

    await client.call("session.end");
    await client.call("device.disconnect");
    await client.close();
  },
);
