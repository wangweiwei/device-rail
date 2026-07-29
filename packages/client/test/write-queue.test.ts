import assert from "node:assert/strict";
import { Writable } from "node:stream";
import test from "node:test";

import {
  NdjsonWriteQueue,
  TransportClosedError,
  WriteFrameTooLargeError,
  WriteQueueOverflowError,
} from "../src/index.js";

class ManualWritable extends Writable {
  readonly chunks: Buffer[] = [];
  readonly #callbacks: Array<(error?: Error | null) => void> = [];

  constructor() {
    super({ highWaterMark: 1 });
  }

  releaseOne(error?: Error): void {
    const callback = this.#callbacks.shift();
    assert.ok(callback, "a write must be waiting");
    callback(error);
  }

  override _write(
    chunk: Buffer,
    _encoding: BufferEncoding,
    callback: (error?: Error | null) => void,
  ): void {
    this.chunks.push(Buffer.from(chunk));
    this.#callbacks.push(callback);
  }
}

async function nextTurn(): Promise<void> {
  await new Promise<void>((resolve) => setImmediate(resolve));
}

test("write queue preserves FIFO and waits for callback plus drain", async () => {
  const writable = new ManualWritable();
  const queue = new NdjsonWriteQueue(writable, {
    maxQueuedBytes: 128,
    maxQueuedFrames: 4,
  });

  const first = queue.enqueue('{"id":1}');
  const second = queue.enqueue('{"id":2}');
  await nextTurn();
  assert.equal(queue.backpressured, true);
  assert.deepEqual(writable.chunks.map(String), ['{"id":1}\n']);

  writable.releaseOne();
  await first;
  await nextTurn();
  assert.deepEqual(writable.chunks.map(String), ['{"id":1}\n', '{"id":2}\n']);

  writable.releaseOne();
  await second;
  assert.equal(queue.backpressured, false);
  assert.equal(queue.queuedFrames, 0);
});

test("write queue enforces UTF-8 frame size and bounded admission", async () => {
  const writable = new ManualWritable();
  const queue = new NdjsonWriteQueue(writable, {
    maxFrameBytes: 3,
    maxQueuedBytes: 16,
    maxQueuedFrames: 1,
  });

  await assert.rejects(queue.enqueue("éé"), WriteFrameTooLargeError);
  const accepted = queue.enqueue("123");
  await assert.rejects(queue.enqueue("x"), WriteQueueOverflowError);
  writable.releaseOne();
  await accepted;
});

test("write queue turns stream failure into explicit transport failure", async () => {
  const writable = new ManualWritable();
  const queue = new NdjsonWriteQueue(writable);
  const write = queue.enqueue("pending");
  await nextTurn();
  writable.destroy(new Error("broken pipe"));
  await assert.rejects(write, TransportClosedError);
});
