import assert from "node:assert/strict";
import { accessSync, constants, mkdtempSync, rmSync } from "node:fs";
import { request as httpRequest } from "node:http";
import { tmpdir } from "node:os";
import { isAbsolute, join, resolve } from "node:path";
import test, { type TestContext } from "node:test";
import { fileURLToPath } from "node:url";

import { DeviceRailClient } from "@devicerail/client";
import type { HelloParams } from "@devicerail/protocol";

import {
  bindLiveVisualizer,
  type BoundLiveVisualizer,
} from "../src/index.js";

const hello = {
  client: { name: "devicerail-live-visualizer-e2e", version: "0.1.0" },
  features: {
    optional: ["events.stream.v1"],
    required: ["device.routing.v1", "events.snapshot.v1", "request.control.v1"],
  },
  protocol: { ranges: [{ major: 1, maxMinor: 3, minMinor: 0 }] },
} satisfies HelloParams;

function workspaceRoot(): string {
  return fileURLToPath(new URL("../../../../", import.meta.url));
}

function daemonExecutable(): string {
  const configured = process.env.DEVICERAIL_DAEMON_BIN;
  if (configured) return isAbsolute(configured) ? configured : resolve(configured);
  const root = workspaceRoot();
  const configuredTarget = process.env.CARGO_TARGET_DIR;
  const target = configuredTarget
    ? isAbsolute(configuredTarget)
      ? configuredTarget
      : resolve(root, configuredTarget)
    : join(root, "target");
  return join(target, "debug", process.platform === "win32" ? "devicerail-daemon.exe" : "devicerail-daemon");
}

function requireDaemonExecutable(): string {
  const executable = daemonExecutable();
  accessSync(executable, process.platform === "win32" ? constants.F_OK : constants.X_OK);
  return executable;
}

function isPermissionDenied(error: unknown): boolean {
  return (
    error !== null &&
    typeof error === "object" &&
    "code" in error &&
    (error.code === "EACCES" || error.code === "EPERM")
  );
}

async function json(endpoint: URL, suffix: string): Promise<unknown> {
  return await new Promise<unknown>((resolvePromise, reject) => {
    const request = httpRequest(
      {
        agent: false,
        headers: { Host: endpoint.host },
        host: "127.0.0.1",
        method: "GET",
        path: `${endpoint.pathname}${suffix}`,
        port: endpoint.port,
      },
      (response) => {
        const chunks: Buffer[] = [];
        response.on("data", (chunk: Buffer) => chunks.push(Buffer.from(chunk)));
        response.once("end", () => {
          if (response.statusCode !== 200) {
            reject(new Error(`live visualizer HTTP request returned ${response.statusCode}`));
            return;
          }
          try {
            resolvePromise(JSON.parse(Buffer.concat(chunks).toString("utf8")) as unknown);
          } catch (error) {
            reject(error);
          }
        });
      },
    );
    request.once("error", reject);
    request.end();
  });
}

interface SseProbe {
  close(): void;
  next(): Promise<string>;
}

async function openSse(endpoint: URL): Promise<SseProbe> {
  return await new Promise<SseProbe>((resolvePromise, reject) => {
    const frames: string[] = [];
    const waiters: Array<{ reject: (error: Error) => void; resolve: (frame: string) => void }> = [];
    let buffered = "";
    let opened = false;
    const operation = httpRequest(
      {
        agent: false,
        headers: { Accept: "text/event-stream", Host: endpoint.host },
        host: "127.0.0.1",
        method: "GET",
        path: `${endpoint.pathname}api/revisions`,
        port: endpoint.port,
      },
      (response) => {
        if (response.statusCode !== 200) {
          reject(new Error(`live visualizer SSE returned ${response.statusCode}`));
          operation.destroy();
          return;
        }
        response.on("data", (chunk: Buffer) => {
          buffered += chunk.toString("utf8");
          for (;;) {
            const boundary = buffered.indexOf("\n\n");
            if (boundary < 0) break;
            const frame = buffered.slice(0, boundary + 2);
            buffered = buffered.slice(boundary + 2);
            const waiter = waiters.shift();
            if (waiter) waiter.resolve(frame);
            else frames.push(frame);
          }
          if (!opened) {
            opened = true;
            resolvePromise({
              close: () => operation.destroy(),
              next: async () => {
                const frame = frames.shift();
                if (frame) return frame;
                return await new Promise<string>((resolve, rejectFrame) => {
                  waiters.push({ reject: rejectFrame, resolve });
                });
              },
            });
          }
        });
        response.once("close", () => {
          for (const waiter of waiters.splice(0)) {
            waiter.reject(new Error("live visualizer SSE closed before the next revision"));
          }
        });
      },
    );
    operation.once("error", (error) => {
      if (!opened) reject(error);
    });
    operation.end();
  });
}

function revision(frame: string): number {
  const match = /^data: ([0-9]+)$/mu.exec(frame);
  assert.ok(match);
  const value = Number(match[1]);
  assert.ok(Number.isSafeInteger(value));
  return value;
}

test(
  "daemon WebSocket events reach the bounded HTTP/SSE live visualizer without transferring client ownership",
  { timeout: 30_000 },
  async (context: TestContext) => {
    const evidenceDir = mkdtempSync(join(tmpdir(), "devicerail-live-visualizer-e2e-"));
    let client: DeviceRailClient | undefined;
    let viewer: BoundLiveVisualizer | undefined;
    let sse: SseProbe | undefined;
    context.after(async () => {
      sse?.close();
      await viewer?.close().catch(() => {});
      if (client?.state !== "closed") await client?.close().catch(() => {});
      rmSync(evidenceDir, { force: true, recursive: true });
    });

    const activeClient = await DeviceRailClient.spawn({
      closeGraceMs: 5_000,
      command: requireDaemonExecutable(),
      hello,
      spawn: {
        env: {
          ...process.env,
          DEVICERAIL_ANDROID: "off",
          DEVICERAIL_EVIDENCE_DIR: evidenceDir,
        },
      },
    });
    client = activeClient;
    if (!activeClient.enabledFeatures.has("events.stream.v1")) {
      if (process.env.DEVICERAIL_ALLOW_NO_LOOPBACK === "1") {
        context.skip("runner explicitly permits skipping forbidden AF_INET loopback binds");
        return;
      }
      assert.fail("daemon did not enable the required events.stream.v1 feature");
    }

    const listed = await activeClient.call("devices.list");
    const device = listed.devices[0];
    assert.ok(device);
    await activeClient.call("device.select", { deviceId: device.id });
    await activeClient.call("device.connect");
    const session = await activeClient.call("session.start");
    let boundViewer: BoundLiveVisualizer;
    try {
      boundViewer = await bindLiveVisualizer({ client: activeClient, sessionId: session.id });
    } catch (error) {
      if (isPermissionDenied(error) && process.env.DEVICERAIL_ALLOW_NO_LOOPBACK === "1") {
        context.skip("runner explicitly permits skipping forbidden AF_INET loopback binds");
        return;
      }
      throw error;
    }
    viewer = boundViewer;

    const endpoint = new URL(boundViewer.endpoint.exposeSecret());
    const probe = await openSse(endpoint);
    sse = probe;
    const firstRevision = revision(await probe.next());

    await activeClient.call("device.observe");
    const nextRevision = revision(await probe.next());
    assert.ok(nextRevision > firstRevision);
    await activeClient.call("device.execute", {
      arguments: { x: 20, y: 30 },
      id: "00000000-0000-4000-8000-000000000030",
      name: "tap",
    });
    await activeClient.call("session.end", {
      outcome: "completed",
      reason: "Live Visualizer end-to-end complete",
    });
    await boundViewer.waitUntilStopped();

    const page = (await json(endpoint, "api/page?filter=all&page=1")) as {
      readonly items?: Array<{ readonly presentation?: { readonly type?: string }; readonly sequence?: number }>;
    };
    assert.deepEqual(page.items?.map((item) => item.sequence), [1, 2, 3, 4, 5]);
    assert.deepEqual(page.items?.map((item) => item.presentation?.type), [
      "sessionStarted",
      "observationCaptured",
      "actionStarted",
      "actionCompleted",
      "sessionEnded",
    ]);
    assert.equal(JSON.stringify(page).includes("\"uri\""), false);

    const state = (await json(endpoint, "api/state")) as {
      readonly status?: string;
      readonly transport?: { readonly phase?: string };
    };
    assert.equal(state.status, "sessionEnded");
    assert.equal(state.transport?.phase, "sessionEnded");

    probe.close();
    sse = undefined;
    await boundViewer.close();
    viewer = undefined;
    assert.equal(activeClient.state, "ready", "viewer close must not close its host client");
    await activeClient.call("device.disconnect");
    await activeClient.close();
  },
);
