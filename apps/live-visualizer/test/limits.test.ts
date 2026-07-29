import assert from "node:assert/strict";
import test from "node:test";

import {
  bindLiveVisualizer,
  type LiveVisualizerClient,
} from "../src/index.js";

const SESSION_ID = "11111111-1111-4111-8111-111111111111";

test("HTTP API bytes must cover every page admitted by the timeline limits", async () => {
  const client = {
    async openEventStream() {
      throw new Error("limit validation must run before opening a stream");
    },
    state: "ready",
  } satisfies LiveVisualizerClient;

  await assert.rejects(
    bindLiveVisualizer({
      client,
      limits: {
        http: { maxApiBytes: 4_000 },
        timeline: {
          maxEventBytes: 512,
          maxEvents: 1,
          maxJsonBytes: 128,
          maxTextBytes: 64,
          maxTotalBytes: 512,
        },
      },
      sessionId: SESSION_ID,
    }),
    (error: unknown) =>
      error instanceof RangeError && error.message.includes("http.maxApiBytes"),
  );
});
