import assert from "node:assert/strict";
import { accessSync, constants, mkdirSync, mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { isAbsolute, join, resolve } from "node:path";
import type { TestContext } from "node:test";
import { fileURLToPath } from "node:url";

import { DeviceRailClient } from "@devicerail/client";
import type { HelloParams } from "@devicerail/protocol";

export const recorderHello = {
  client: {
    name: "devicerail-recorder-e2e",
    version: "0.1.0",
  },
  protocol: {
    ranges: [{ major: 1, minMinor: 0, maxMinor: 4 }],
  },
  features: {
    required: ["device.routing.v1", "events.snapshot.v1", "request.control.v1"],
    optional: ["action.protected.v1", "media.stream.v1", "session.export.page.v1"],
  },
} satisfies HelloParams;

function workspaceRoot(): string {
  return fileURLToPath(new URL("../../../../", import.meta.url));
}

function executablePath(environmentName: string, executableName: string): string {
  const configured = process.env[environmentName];
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
  const filename = process.platform === "win32" ? `${executableName}.exe` : executableName;
  return join(target, "debug", filename);
}

function requireExecutable(environmentName: string, executableName: string): string {
  const executable = executablePath(environmentName, executableName);
  try {
    accessSync(executable, process.platform === "win32" ? constants.F_OK : constants.X_OK);
  } catch (cause) {
    assert.fail(
      `${executableName} is unavailable at ${executable}; build it or set ${environmentName}` +
        (cause instanceof Error ? ` (${cause.message})` : ""),
    );
  }
  return executable;
}

export function requireBundleExecutable(): string {
  return requireExecutable("DEVICERAIL_BUNDLE_BIN", "devicerail-bundle");
}

export interface TestDaemon {
  readonly client: DeviceRailClient;
  readonly evidenceDirectory: string;
  readonly temporaryDirectory: string;
}

export async function spawnTestDaemon(
  context: TestContext,
  environment: Readonly<Record<string, string>> = {},
): Promise<TestDaemon> {
  const temporaryDirectory = mkdtempSync(join(tmpdir(), "devicerail-recorder-e2e-"));
  const evidenceDirectory = join(temporaryDirectory, "evidence");
  mkdirSync(evidenceDirectory, { mode: 0o700 });
  let client: DeviceRailClient | undefined;

  context.after(async () => {
    if (client?.state !== "closed") {
      await client?.close().catch(() => {});
    }
    rmSync(temporaryDirectory, { force: true, recursive: true });
  });

  try {
    client = await DeviceRailClient.spawn({
      closeGraceMs: 5_000,
      command: requireExecutable("DEVICERAIL_DAEMON_BIN", "devicerail-daemon"),
      hello: recorderHello,
      spawn: {
        env: {
          ...process.env,
          DEVICERAIL_ANDROID: "off",
          DEVICERAIL_EVIDENCE_DIR: evidenceDirectory,
          ...environment,
        },
      },
    });
    return { client, evidenceDirectory, temporaryDirectory };
  } catch (error) {
    rmSync(temporaryDirectory, { force: true, recursive: true });
    throw error;
  }
}
