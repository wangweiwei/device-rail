import assert from "node:assert/strict";
import { Writable } from "node:stream";
import test from "node:test";

import { TransportClosedError, WriteQueueOverflowError } from "../src/errors.js";
import { NdjsonWriteQueue } from "../src/write-queue.js";

class ControlledWritable extends Writable {
  readonly chunks: Buffer[] = [];
  readonly #callbacks: Array<(error?: Error | null) => void> = [];

  constructor() {
    super({ highWaterMark: 1 });
  }

  get pendingCallbacks(): number {
    return this.#callbacks.length;
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

test("queue byte admission includes newlines and counts the active write", async () => {
  const writable = new ControlledWritable();
  const queue = new NdjsonWriteQueue(writable, {
    maxFrameBytes: 8,
    maxQueuedBytes: 4,
    maxQueuedFrames: 2,
  });

  const accepted = queue.enqueue("abc");
  assert.equal(queue.queuedBytes, 4);
  assert.equal(queue.queuedFrames, 1);
  await assert.rejects(queue.enqueue(""), WriteQueueOverflowError);
  assert.equal(queue.queuedBytes, 4);
  assert.equal(queue.queuedFrames, 1);

  writable.releaseOne();
  await accepted;
  assert.equal(queue.queuedBytes, 0);
  assert.equal(queue.queuedFrames, 0);
  await queue.close();
});

test("close waits for all admitted writes and then rejects new work", async () => {
  const writable = new ControlledWritable();
  const queue = new NdjsonWriteQueue(writable, {
    maxQueuedBytes: 64,
    maxQueuedFrames: 4,
  });
  const first = queue.enqueue("first");
  const second = queue.enqueue("second");
  let closeSettled = false;
  const close = queue.close().then(() => {
    closeSettled = true;
  });

  await nextTurn();
  assert.equal(closeSettled, false);
  assert.equal(writable.pendingCallbacks, 1);
  writable.releaseOne();
  await first;
  await nextTurn();
  assert.equal(closeSettled, false);
  assert.equal(writable.pendingCallbacks, 1);
  writable.releaseOne();
  await second;
  await close;

  assert.equal(closeSettled, true);
  assert.equal(queue.queuedBytes, 0);
  assert.equal(queue.queuedFrames, 0);
  await assert.rejects(queue.enqueue("late"), TransportClosedError);
  await assert.rejects(queue.idle(), TransportClosedError);
});

test("close stops admitting new frames as soon as draining begins", async () => {
  const writable = new ControlledWritable();
  const queue = new NdjsonWriteQueue(writable, {
    maxQueuedBytes: 64,
    maxQueuedFrames: 4,
  });
  const first = queue.enqueue("first");
  const close = queue.close();
  const late = queue.enqueue("late");
  const pending = Symbol("pending");
  const lateOutcome = late.then(
    () => undefined,
    (error: unknown) => error,
  );
  const outcome = await Promise.race([lateOutcome, nextTurn().then(() => pending)]);

  if (outcome === pending) {
    queue.fail(new Error("test cleanup after late admission"));
  }
  writable.releaseOne();
  await Promise.allSettled([first, late, close]);

  assert.ok(outcome instanceof TransportClosedError);
  assert.equal(queue.queuedBytes, 0);
  assert.equal(queue.queuedFrames, 0);
});

test("fail atomically rejects active, queued, idle, and future operations", async () => {
  const writable = new ControlledWritable();
  const queue = new NdjsonWriteQueue(writable, {
    maxQueuedBytes: 64,
    maxQueuedFrames: 4,
  });
  const first = queue.enqueue("first");
  const second = queue.enqueue("second");
  const idle = queue.idle();
  const progress = queue.waitForProgress();
  const reportedFailure = queue.failure;
  const failure = new Error("stop now");
  const replacement = new Error("must not replace the first failure");

  const assertions = [first, second, idle, progress].map(async (operation) => {
    await assert.rejects(operation, (error) => error === failure);
  });
  queue.fail(failure);
  queue.fail(replacement);
  await Promise.all(assertions);
  assert.equal(await reportedFailure, failure);

  assert.equal(queue.backpressured, false);
  assert.equal(queue.queuedBytes, 0);
  assert.equal(queue.queuedFrames, 0);
  await assert.rejects(queue.enqueue("late"), (error) => error === failure);
  await assert.rejects(queue.idle(), (error) => error === failure);

  writable.releaseOne();
});

test("an outbound close preserves explicit transport error metadata", async () => {
  const writable = new ControlledWritable();
  const queue = new NdjsonWriteQueue(writable);
  const write = queue.enqueue("pending");

  await nextTurn();
  writable.emit("close");
  await assert.rejects(
    write,
    (error: unknown) =>
      error instanceof TransportClosedError &&
      error.code === "transport_closed" &&
      error.message === "outbound stream closed",
  );
  assert.equal(queue.queuedBytes, 0);
  assert.equal(queue.queuedFrames, 0);

  writable.releaseOne();
});

test("write queue rejects invalid capacities", () => {
  const invalid = [0, -1, 1.5, Number.NaN, Number.POSITIVE_INFINITY];

  for (const maxFrameBytes of invalid) {
    assert.throws(
      () => new NdjsonWriteQueue(new ControlledWritable(), { maxFrameBytes }),
      RangeError,
    );
  }
  for (const maxQueuedBytes of invalid) {
    assert.throws(
      () => new NdjsonWriteQueue(new ControlledWritable(), { maxQueuedBytes }),
      RangeError,
    );
  }
  for (const maxQueuedFrames of invalid) {
    assert.throws(
      () => new NdjsonWriteQueue(new ControlledWritable(), { maxQueuedFrames }),
      RangeError,
    );
  }
});
