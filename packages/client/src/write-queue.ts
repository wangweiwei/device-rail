import type { Writable } from "node:stream";

import {
  TransportClosedError,
  WriteFrameTooLargeError,
  WriteQueueOverflowError,
} from "./errors.js";
import { DEFAULT_MAX_FRAME_BYTES } from "./ndjson.js";

const NEWLINE = Buffer.from("\n");

export interface NdjsonWriteQueueOptions {
  readonly maxFrameBytes?: number;
  readonly maxQueuedBytes?: number;
  readonly maxQueuedFrames?: number;
}

interface QueuedFrame {
  readonly bytes: Buffer;
  readonly reject: (error: Error) => void;
  readonly resolve: () => void;
}

interface IdleWaiter {
  readonly reject: (error: Error) => void;
  readonly resolve: () => void;
}

function positiveSafeInteger(value: number, name: string): number {
  if (!Number.isSafeInteger(value) || value <= 0) {
    throw new RangeError(`${name} must be a positive safe integer`);
  }
  return value;
}

/** Serializes outbound NDJSON writes and makes stream backpressure observable. */
export class NdjsonWriteQueue {
  readonly maxFrameBytes: number;
  readonly maxQueuedBytes: number;
  readonly maxQueuedFrames: number;

  #backpressured = false;
  #accepting = true;
  #activeAbort: ((error: Error) => void) | undefined;
  #closePromise: Promise<void> | undefined;
  #failure: Error | undefined;
  readonly #failurePromise: Promise<Error>;
  #resolveFailure!: (error: Error) => void;
  #idleWaiters: IdleWaiter[] = [];
  #progressWaiters: IdleWaiter[] = [];
  #pumping = false;
  #progressVersion = 0n;
  #queue: QueuedFrame[] = [];
  #queuedBytes = 0;
  #streamErrorsGuarded = false;
  readonly #writable: Writable;
  readonly #ignoreLateStreamError = (): void => {};
  readonly #onGuardedStreamClose = (): void => {
    this.#writable.off("error", this.#ignoreLateStreamError);
    this.#writable.off("close", this.#onGuardedStreamClose);
    this.#streamErrorsGuarded = false;
  };
  readonly #onStreamClose = (): void => {
    this.fail(new TransportClosedError("outbound stream closed"));
  };
  readonly #onStreamError = (cause: Error): void => {
    this.fail(new TransportClosedError("outbound stream failed", { cause }));
  };
  readonly #onStreamFinish = (): void => {
    this.fail(new TransportClosedError("outbound stream finished"));
  };

  constructor(writable: Writable, options: NdjsonWriteQueueOptions = {}) {
    this.#writable = writable;
    this.maxFrameBytes = positiveSafeInteger(
      options.maxFrameBytes ?? DEFAULT_MAX_FRAME_BYTES,
      "maxFrameBytes",
    );
    this.maxQueuedBytes = positiveSafeInteger(
      options.maxQueuedBytes ?? this.maxFrameBytes * 4,
      "maxQueuedBytes",
    );
    this.maxQueuedFrames = positiveSafeInteger(
      options.maxQueuedFrames ?? 256,
      "maxQueuedFrames",
    );
    this.#failurePromise = new Promise<Error>((resolve) => {
      this.#resolveFailure = resolve;
    });
    this.#writable.on("close", this.#onStreamClose);
    this.#writable.on("error", this.#onStreamError);
    this.#writable.on("finish", this.#onStreamFinish);
  }

  get backpressured(): boolean {
    return this.#backpressured;
  }

  /** Resolves exactly once when the underlying stream or queue fails. */
  get failure(): Promise<Error> {
    return this.#failurePromise;
  }

  get queuedBytes(): number {
    return this.#queuedBytes;
  }

  get queuedFrames(): number {
    return this.#queue.length;
  }

  get progressVersion(): bigint {
    return this.#progressVersion;
  }

  enqueue(frame: string): Promise<void> {
    if (this.#failure) {
      return Promise.reject(this.#failure);
    }
    if (!this.#accepting) {
      return Promise.reject(new TransportClosedError("outbound queue is closing"));
    }

    const payload = Buffer.from(frame, "utf8");
    if (payload.length > this.maxFrameBytes) {
      return Promise.reject(new WriteFrameTooLargeError(this.maxFrameBytes, payload.length));
    }

    const bytes = Buffer.concat([payload, NEWLINE]);
    if (this.#queue.length >= this.maxQueuedFrames) {
      return Promise.reject(
        new WriteQueueOverflowError(
          `outbound queue already contains ${this.#queue.length} frames; the limit is ${this.maxQueuedFrames}`,
        ),
      );
    }
    if (this.#queuedBytes + bytes.length > this.maxQueuedBytes) {
      return Promise.reject(
        new WriteQueueOverflowError(
          `outbound queue would contain ${this.#queuedBytes + bytes.length} bytes; the limit is ${this.maxQueuedBytes}`,
        ),
      );
    }

    const completion = new Promise<void>((resolve, reject) => {
      this.#queue.push({ bytes, reject, resolve });
      this.#queuedBytes += bytes.length;
    });
    void this.#pump();
    return completion;
  }

  idle(): Promise<void> {
    if (this.#failure) {
      return Promise.reject(this.#failure);
    }
    if (this.#queue.length === 0) {
      return Promise.resolve();
    }
    return new Promise<void>((resolve, reject) => {
      this.#idleWaiters.push({ reject, resolve });
    });
  }

  /** Resolves after the next admitted frame leaves the bounded queue. */
  waitForProgress(since = this.#progressVersion): Promise<void> {
    if (this.#failure) {
      return Promise.reject(this.#failure);
    }
    if (since !== this.#progressVersion) {
      return Promise.resolve();
    }
    return new Promise<void>((resolve, reject) => {
      this.#progressWaiters.push({ reject, resolve });
    });
  }

  fail(error: Error): void {
    if (this.#failure) {
      return;
    }
    this.#accepting = false;
    this.#failure = error;
    this.#resolveFailure(error);
    this.#backpressured = false;
    this.#activeAbort?.(error);
    const queued = this.#queue.splice(0);
    this.#queuedBytes = 0;
    for (const frame of queued) {
      frame.reject(error);
    }
    for (const waiter of this.#idleWaiters.splice(0)) {
      waiter.reject(error);
    }
    for (const waiter of this.#progressWaiters.splice(0)) {
      waiter.reject(error);
    }
    this.#detachStreamListeners();
  }

  close(): Promise<void> {
    if (this.#closePromise) {
      return this.#closePromise;
    }
    if (this.#failure) {
      this.#closePromise = Promise.reject(this.#failure);
      return this.#closePromise;
    }
    this.#accepting = false;
    this.#closePromise = this.#finishClose();
    return this.#closePromise;
  }

  async #finishClose(): Promise<void> {
    await this.idle();
    if (this.#failure) {
      throw this.#failure;
    }
    this.#failure = new TransportClosedError();
    for (const waiter of this.#progressWaiters.splice(0)) {
      waiter.reject(this.#failure);
    }
    this.#detachStreamListeners();
  }

  async #pump(): Promise<void> {
    if (this.#pumping || this.#failure) {
      return;
    }
    this.#pumping = true;
    try {
      while (!this.#failure) {
        const frame = this.#queue[0];
        if (!frame) {
          return;
        }
        try {
          await this.#write(frame.bytes);
        } catch (cause) {
          const error =
            cause instanceof Error
              ? new TransportClosedError("outbound stream failed", { cause })
              : new TransportClosedError("outbound stream failed");
          this.fail(error);
          return;
        }
        if (this.#failure) {
          return;
        }
        this.#queue.shift();
        this.#queuedBytes -= frame.bytes.length;
        this.#progressVersion += 1n;
        for (const waiter of this.#progressWaiters.splice(0)) {
          waiter.resolve();
        }
        frame.resolve();
        if (this.#queue.length === 0) {
          for (const waiter of this.#idleWaiters.splice(0)) {
            waiter.resolve();
          }
        }
      }
    } finally {
      this.#pumping = false;
    }
  }

  #write(bytes: Buffer): Promise<void> {
    return new Promise((resolve, reject) => {
      let callbackComplete = false;
      let drainComplete = false;
      let settled = false;
      let writeReturned = false;

      const cleanup = (): void => {
        this.#writable.off("drain", onDrain);
        if (this.#activeAbort === abort) {
          this.#activeAbort = undefined;
        }
      };
      const abort = (error: Error): void => {
        if (settled) {
          return;
        }
        settled = true;
        cleanup();
        reject(error);
      };
      const completeIfReady = (): void => {
        if (!settled && writeReturned && callbackComplete && drainComplete) {
          settled = true;
          cleanup();
          this.#backpressured = false;
          resolve();
        }
      };
      const onDrain = (): void => {
        drainComplete = true;
        completeIfReady();
      };

      this.#activeAbort = abort;
      try {
        const accepted = this.#writable.write(bytes, (error?: Error | null) => {
          if (error) {
            abort(error);
            return;
          }
          callbackComplete = true;
          completeIfReady();
        });
        writeReturned = true;
        drainComplete = accepted;
        this.#backpressured = !accepted;
        if (!accepted) {
          this.#writable.once("drain", onDrain);
        }
        completeIfReady();
      } catch (cause) {
        abort(cause instanceof Error ? cause : new Error("outbound write threw"));
      }
    });
  }

  #detachStreamListeners(): void {
    this.#writable.off("close", this.#onStreamClose);
    this.#writable.off("error", this.#onStreamError);
    this.#writable.off("finish", this.#onStreamFinish);
    if (!this.#streamErrorsGuarded) {
      this.#streamErrorsGuarded = true;
      this.#writable.on("error", this.#ignoreLateStreamError);
      this.#writable.once("close", this.#onGuardedStreamClose);
    }
  }
}
