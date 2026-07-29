import type { Writable } from "node:stream";

import type { BridgeResponse } from "./types.js";
import { parseBridgeRequest } from "./validate.js";
import { handleBridgeRequest, type PlaywrightPort } from "./worker.js";

const MAX_INPUT_BYTES = 2 * 1024 * 1024;
const MAX_OUTPUT_BYTES = 24 * 1024 * 1024;

function invalidRequest(): BridgeResponse {
  return { error: { code: "invalid_request", retryable: false }, ok: false };
}

async function* requestLines(
  source: AsyncIterable<Uint8Array | string>,
): AsyncGenerator<Buffer> {
  let pending = Buffer.alloc(0);
  for await (const chunk of source) {
    const bytes = typeof chunk === "string" ? Buffer.from(chunk) : Buffer.from(chunk);
    let offset = 0;
    while (offset < bytes.length) {
      const newline = bytes.indexOf(0x0a, offset);
      const end = newline === -1 ? bytes.length : newline;
      const segment = bytes.subarray(offset, end);
      if (pending.length + segment.length > MAX_INPUT_BYTES) {
        throw new Error("input limit exceeded");
      }
      pending = pending.length === 0 ? Buffer.from(segment) : Buffer.concat([pending, segment]);
      if (newline === -1) break;
      if (pending.at(-1) === 0x0d) pending = pending.subarray(0, pending.length - 1);
      yield pending;
      pending = Buffer.alloc(0);
      offset = newline + 1;
    }
  }
  if (pending.length > 0) yield pending;
}

async function writeLine(sink: Writable, response: BridgeResponse): Promise<void> {
  let line = Buffer.from(`${JSON.stringify(response)}\n`);
  if (line.length > MAX_OUTPUT_BYTES) {
    line = Buffer.from(`${JSON.stringify({
      error: { code: "response_too_large", retryable: false },
      ok: false,
    } satisfies BridgeResponse)}\n`);
  }
  await new Promise<void>((resolve, reject) => {
    sink.write(line, (error) => {
      if (error) reject(error);
      else resolve();
    });
  });
}

export async function serveBridge(
  source: AsyncIterable<Uint8Array | string>,
  sink: Writable,
  port: PlaywrightPort,
): Promise<void> {
  for await (const line of requestLines(source)) {
    let response: BridgeResponse;
    try {
      const request = parseBridgeRequest(JSON.parse(line.toString("utf8")) as unknown);
      response = await handleBridgeRequest(request, port);
    } catch {
      response = invalidRequest();
    }
    await writeLine(sink, response);
  }
}
