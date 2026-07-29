import assert from "node:assert/strict";
import test from "node:test";

import type { HelloParams } from "@devicerail/protocol";

import { DeviceRailClient } from "../src/index.js";

function compileOnly(client: DeviceRailClient, hello: HelloParams): void {
  if (false) {
    // @ts-expect-error system.hello is only available through client.hello().
    void client.call("system.hello", hello);
    // @ts-expect-error events.subscribe is available only inside the WebSocket stream handshake.
    void client.call("events.subscribe", { sessionId: "session" });
    // @ts-expect-error system.describe does not support timeoutMs.
    void client.call("system.describe", undefined, { timeoutMs: 1 });
    void client.call("device.observe", undefined, { timeoutMs: 1 });
    void client.call(
      "media.stream.capture",
      { frameIndex: 1, streamId: "00000000-0000-4000-8000-000000000001" },
      { timeoutMs: 1 },
    );
    void client.call(
      "media.stream.start",
      { kind: "screenshot", streamId: "00000000-0000-4000-8000-000000000001" },
      // @ts-expect-error media.stream.start is not a frame-capture operation.
      { timeoutMs: 1 },
    );
  }
}

test("client method types reserve hello and expose timeout only where supported", () => {
  assert.equal(typeof compileOnly, "function");
});
